use std::collections::BTreeSet;
use std::env;
use std::error::Error as _;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};
use serde_json::json;
use tracing::debug;

use super::init::{
    GitignoreStatus, GitignoreUpdate, KALOS_DIR_ENTRY, ensure_gitignore_entry,
    gitignore_entry_status,
};

use crate::adapters::baseline_cache::BaselineCacheAdapter;
use crate::adapters::dependency_resolver::StubDependencyResolver;
use crate::adapters::diff_source::GitDiffAdapter;
use crate::adapters::extractor::{CodeQlAdapter, FileCollector};
use crate::adapters::llm::HttpLlmAdapter;
use crate::adapters::llm::http::validate_llm_config;
use crate::adapters::plugin::{
    EvaluationWarning, ModuleLoadWarning, PluginHostError, WasmPluginHost,
};
use crate::adapters::tool_cache::{ManagedToolCacheAdapter, Platform, codeql_bundle_manifest};
use crate::application::pipeline::{AnalysisPipeline, DiffConfig};
use crate::domains::MetricId;
use crate::domains::Severity;
use crate::domains::config::{Defaults, ProjectConfig, ResolveOptions};
use crate::domains::metrics::builtin_metric_definitions;
use crate::domains::reporting::{
    OutputFormat as DomainOutputFormat, ReportViewOptions, RequestedLevel as DomainRequestedLevel,
    outcome_for_error_class, render_sarif_error_document,
};
use crate::platform::fs::RealFileSystem;
use crate::platform::process::SystemCommandRunner;
use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};
use crate::ports::{DiffRequest, DiffSourcePort, PluginPort};

const DEFAULT_CODEQL_TOTAL_TIMEOUT_SECS: u64 = 1200;

#[derive(Debug, Clone, Args)]
#[command(
    about = "run code quality analysis",
    after_help = "NOTE: Normal `check` execution may write to locations such as:\n  \
                  - `<repo>/.kalos/codeql/<language>/` stores per-language CodeQL databases unless --cache-dir is passed.\n  \
                  - `$KALOS_CACHE_DIR/codeql/` or `--cache-dir <path>/codeql/` may store managed CodeQL bundles.\n  \
                  - `$KALOS_CACHE_DIR/baselines/` or `--cache-dir <path>/baselines/` may store cached baselines for full-workspace runs in Git repositories.\n  \
                  - `--cache-dir <path>/codeql/databases/<language>/` stores per-language CodeQL databases when --cache-dir is passed.\n  \
                  - `<repo>/.gitignore` is only created or updated when --update-gitignore is passed.\n\n\
                  NOTE: For repeated matrix evaluation over large repositories, use one stable shared `--cache-dir` per repository and pre-populate or warm the managed CodeQL cache before cold harness runs. Avoid per-case cache directories for repeated level/format evaluation. Account for cold bundle setup and CodeQL database creation separately from rule runtime. Start with a single full `--level project --format json` run, then run level/format matrices with `--diff`, narrower targets, and `--exclude` for generated or vendor paths.\n\n\
                  NOTE: On Apple Silicon (aarch64), CodeQL runs via Rosetta 2 using an x86_64 bundle, \
                  which may cause significantly slower analysis on first invocation."
)]
pub struct CheckCommand {
    #[arg(
        value_name = "path",
        help = "target files or directories to analyze (defaults to workspace root)"
    )]
    pub paths: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, help = "output format")]
    pub format: OutputFormat,
    #[arg(
        long,
        value_enum,
        default_value_t = RequestedLevel::Project,
        help = "analysis granularity level",
        long_help = "analysis granularity level

The default `project` level is the recommended first-run and CI baseline gate.
`--level function`, `--level module`, and `--level all` opt in to broader
diagnostic inventories. Function/all runs may emit many KAL-F001/KAL-F003
function findings; module/all runs expose module diagnostics such as KAL-M001,
KAL-M002, and KAL-M003. Use the broader levels for domain-owner triage:
inspect dependency direction, owner boundaries, and configured thresholds, then
tune noisy rules in .kalos.toml with threshold, severity, or enabled overrides."
    )]
    pub level: RequestedLevel,
    #[arg(
        long,
        value_name = "path",
        help = "path to configuration file (.kalos.toml)"
    )]
    pub config: Option<PathBuf>,
    #[arg(
        long,
        value_name = "path",
        help = "store managed bundles, baselines, and CodeQL databases under this cache directory"
    )]
    pub cache_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "path",
        help = "workspace root to resolve target paths against"
    )]
    pub workspace_root: Option<PathBuf>,
    #[arg(
        long,
        value_name = "pattern",
        help = "glob patterns to exclude from analysis (repeatable)"
    )]
    pub exclude: Vec<String>,
    #[arg(
        long,
        value_enum,
        help = "minimum severity threshold for diagnostics (omit to show all)"
    )]
    pub severity: Option<MinimumSeverity>,
    #[arg(
        long,
        value_name = "base-ref",
        help = "git base ref for differential analysis"
    )]
    pub diff: Option<String>,
    #[arg(
        long,
        help = "enable llm-assisted analysis (requires KALOS_LLM_API_KEY)",
        long_help = "enable llm-assisted analysis (requires KALOS_LLM_API_KEY)\n\nSet KALOS_LLM_API_KEY before running evaluation cases that include --llm. When the key is missing, kalos exits before analysis with code 2 and structured outputs classify the case as expected_skip so evaluation summaries can distinguish it from tool failures."
    )]
    pub llm: bool,
    #[arg(
        long,
        help = "treat warnings as errors",
        long_help = "treat warnings as errors

returns exit code 1 when any warning-severity diagnostic is emitted.
Examples of rules that may warn by default include KAL-PAT001 (god unit) and KAL-PAT002 (feature envy).
Metric rules from KAL-F001 through KAL-P003 report warnings when overflow ratio is in [0.25, 0.60).
Override severity per rule in .kalos.toml under [rules.<rule-id>]."
    )]
    pub strict: bool,
    #[arg(long, help = "show per-scope metrics in human output")]
    pub verbose: bool,
    #[arg(
        long,
        value_name = "threshold",
        help = "filter verbose metrics list by scope risk",
        long_help = "filter verbose metrics list by scope risk.\n\nDefault: hide scopes with risk=0. Pass --min-risk 0 to include all scopes."
    )]
    pub min_risk: Option<f64>,
    #[arg(
        long,
        value_name = "ratio",
        value_parser = parse_min_language_ratio,
        help = "minimum file ratio (0.0-1.0) for a language to be analyzed (default: 0.05)"
    )]
    pub min_language_ratio: Option<f64>,
    #[arg(
        long,
        value_name = "MiB",
        value_parser = parse_codeql_ram_mib,
        help = "maximum RAM in MiB passed to CodeQL database/query phases via --ram"
    )]
    pub codeql_ram: Option<u32>,
    #[arg(
        long,
        value_name = "seconds",
        value_parser = parse_codeql_timeout_secs,
        default_value_t = 240,
        help = "maximum seconds allowed for CodeQL setup and subprocess phases",
        long_help = "maximum seconds allowed for managed CodeQL bundle setup and each CodeQL subprocess phase.\n\nThe cap also applies while preparing a cold/cache-heavy managed CodeQL bundle. Pass --codeql-timeout 0 to disable subprocess phase timeouts; managed bundle setup keeps its default timeout unless --codeql-total-timeout sets a stricter total budget."
    )]
    pub codeql_timeout: u64,
    #[arg(
        long,
        value_name = "seconds",
        value_parser = parse_codeql_timeout_secs,
        help = "maximum total seconds allowed for CodeQL setup and subprocess phases",
        long_help = "maximum total seconds allowed for CodeQL setup and subprocess phases.\n\nDefault: 1200 seconds, unless --codeql-timeout 0 is passed. When --codeql-timeout 0 disables subprocess phase timeouts, the total budget is also disabled unless this option is explicitly provided. Pass --codeql-total-timeout 0 to disable the total CodeQL wall-clock budget."
    )]
    pub codeql_total_timeout: Option<u64>,
    #[arg(
        long,
        help = "allow structured-output runs to disable all CodeQL timeouts on large repositories",
        long_help = "allow structured-output runs to disable all CodeQL timeouts on large repositories.\n\nWithout this opt-in, `kalos check --format json|sarif --codeql-timeout 0 --codeql-total-timeout 0` fails fast on large source inventories before CodeQL execution and returns a structured expected_skip outcome with a narrower recommended command. Human output keeps progress guidance visible and is not blocked by this guard."
    )]
    pub allow_unbounded_large_repo_analysis: bool,
    #[arg(
        long,
        help = "include test files in test-noisy diagnostics (KAL-F001, KAL-F003, KAL-M001, KAL-M003)",
        long_help = "include test files in test-noisy diagnostics (KAL-F001, KAL-F003, KAL-M001, KAL-M003)

By default, KAL-F001 (CFG branch entropy), KAL-F003 (data flow density), \
KAL-M001 (module fan-out), and KAL-M003 (instability) diagnostics are suppressed \
for test files because tests commonly contain intentionally complex helpers and \
import many modules. Test files are NOT excluded from analysis: their metrics \
are computed, they count toward files_analyzed, and every other rule (for \
example KAL-PAT003) still applies to them.

Pass --include-tests to re-enable these diagnostics on test files. \
The flag has no observable effect on JSON output when no test module crosses the \
configured thresholds in .kalos.toml.

Test files are detected by path:
  - tests/** (at project root or any ancestor directory)
  - __tests__/** (at project root or any ancestor directory)
  - *_test.* (file name)
  - *.test.* (file name)
  - *.spec.* (file name)
  - test_*.py (Python test modules)"
    )]
    pub include_tests: bool,
    #[arg(
        long,
        short = 'o',
        value_name = "path",
        help = "write output to a file instead of stdout"
    )]
    pub output: Option<PathBuf>,
    #[arg(
        long,
        short = 'q',
        help = "suppress the stderr acknowledgment printed on --output success"
    )]
    pub quiet: bool,
    #[arg(
        long,
        help = "add .kalos/ to .gitignore when missing (default: warn only, do not modify .gitignore)"
    )]
    pub update_gitignore: bool,
}

impl CheckCommand {
    pub fn requested_paths(&self) -> Vec<PathBuf> {
        if self.paths.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            self.paths.clone()
        }
    }

    pub fn targets_explicitly_specified(&self) -> bool {
        !self.paths.is_empty()
    }

    pub fn resolve_options(&self, cwd: PathBuf) -> ResolveOptions {
        ResolveOptions {
            cwd,
            workspace_root: self.workspace_root.clone(),
            config_path: self.config.clone(),
            analysis_targets: self.requested_paths(),
            targets_explicitly_specified: self.targets_explicitly_specified(),
            exclude_patterns: self.exclude.clone(),
        }
    }

    pub fn execute(&self) -> ExitCode {
        let cwd = match env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                let message = format!("failed to determine current directory: {error}");
                emit_error(self.format, self.output.as_deref(), &message, Some(&error));
                return ExitCode::from(2);
            }
        };
        let options = self.resolve_options(cwd);
        let mut config = match ProjectConfig::load_and_resolve(&options, &Defaults::default()) {
            Ok(config) => config,
            Err(error) => {
                let message = error.to_string();
                emit_error(
                    self.format,
                    self.output.as_deref(),
                    &message,
                    error.source(),
                );
                return ExitCode::from(2);
            }
        };
        config.include_tests = self.include_tests;
        if let Some(path) = &self.output {
            if path.is_dir() {
                let message = format!(
                    "output path `{}` is a directory; pass a file path to --output",
                    path.display()
                );
                emit_error(self.format, self.output.as_deref(), &message, None);
                return ExitCode::from(2);
            }
        }
        debug!(
            workspace_root = %config.workspace_root.abs_path.display(),
            target_count = config.analysis_targets.len(),
            rule_count = config.rules.len(),
            "check config loaded"
        );
        let llm_adapter = if self.llm {
            match validate_llm_config() {
                Ok(config) => Some(HttpLlmAdapter::new(config)),
                Err(error) => {
                    let message = error.to_string();
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &message,
                        error.source(),
                    );
                    return ExitCode::from(2);
                }
            }
        } else {
            None
        };
        let manifest = match codeql_bundle_manifest() {
            Ok(manifest) => manifest,
            Err(error) => {
                let message = error.to_string();
                emit_error(
                    self.format,
                    self.output.as_deref(),
                    &message,
                    error.source(),
                );
                return ExitCode::from(2);
            }
        };
        let platform = Platform::detect();
        let human_progress = self.format == OutputFormat::Human;
        if human_progress {
            if let Some(notice) = platform.and_then(|platform| platform.emulation_notice()) {
                eprintln!("{notice}");
            }
        }
        let codeql_version = manifest.version.clone();
        let tool_cache = match &self.cache_dir {
            Some(cache_dir) => ManagedToolCacheAdapter::with_cache_dir(manifest, cache_dir.clone()),
            None => ManagedToolCacheAdapter::new(manifest),
        };
        let codeql_total_timeout =
            effective_codeql_total_timeout(self.codeql_timeout, self.codeql_total_timeout);
        let setup_timeout = codeql_setup_timeout(self.codeql_timeout, codeql_total_timeout);
        let tool_cache = match setup_timeout {
            Some(timeout) => tool_cache.with_bundle_setup_timeout(timeout),
            None => tool_cache,
        };
        let tool_cache = ProgressToolCacheAdapter::new(tool_cache, human_progress, setup_timeout);
        let exclude_patterns = config
            .exclude_patterns
            .iter()
            .map(|pattern| pattern.pattern.clone())
            .collect::<Vec<_>>();
        let mut extractor = CodeQlAdapter::new(
            RealFileSystem,
            SystemCommandRunner,
            tool_cache,
            codeql_version,
            exclude_patterns,
        );
        if let Some(cache_dir) = &self.cache_dir {
            extractor = extractor.with_database_root(cache_dir.join("codeql").join("databases"));
        }
        if human_progress {
            extractor = extractor.with_progress();
        }
        if platform.map_or(false, |platform| platform.is_emulated()) {
            extractor = extractor.with_emulated();
        }
        if let Some(ratio) = self.min_language_ratio {
            extractor = extractor.with_min_language_ratio(ratio);
        }
        if let Some(ram_mib) = self.codeql_ram {
            extractor = extractor.with_codeql_ram_mib(ram_mib);
        }
        extractor = extractor.with_codeql_timeout(
            (self.codeql_timeout > 0).then(|| Duration::from_secs(self.codeql_timeout)),
        );
        extractor = extractor.with_codeql_total_timeout(
            codeql_total_timeout
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs),
        );
        extractor = extractor.with_fail_fast_unbounded_slow_path(
            self.format != OutputFormat::Human
                && self.codeql_timeout == 0
                && codeql_total_timeout
                    .filter(|seconds| *seconds > 0)
                    .is_none()
                && !self.allow_unbounded_large_repo_analysis,
        );
        let dependency_resolver = StubDependencyResolver;
        let pipeline = AnalysisPipeline::new(extractor, dependency_resolver);
        let mut plugin_host = Some(WasmPluginHost::load(
            config.workspace_root.as_path(),
            &config.plugin_manifest,
            &builtin_metric_ids(),
            if self.diff.is_some() {
                DIFF_PLUGIN_AGGREGATE_FUEL_BUDGET
            } else {
                FULL_PLUGIN_AGGREGATE_FUEL_BUDGET
            },
        ));
        if let Some(plugin_host) = &plugin_host {
            emit_module_load_warnings(plugin_host.warnings());
        }
        let view_options = ReportViewOptions {
            requested_level: self.level.into(),
            output_format: self.format.into(),
            strict: self.strict,
            minimum_severity: self.severity.map(Severity::from),
            min_risk: self.min_risk,
            verbose: self.verbose,
        };
        debug!(
            mode = if self.diff.is_some() { "diff" } else { "full" },
            "check analysis mode selected"
        );

        let result = if let Some(base_ref) = &self.diff {
            let cache = match baseline_cache_adapter(self.cache_dir.as_ref()) {
                Ok(cache) => cache,
                Err(error) => {
                    let message = error.to_string();
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &message,
                        error.source(),
                    );
                    return ExitCode::from(2);
                }
            };
            let diff_source = GitDiffAdapter;
            if self.cache_dir.is_none() && self.update_gitignore {
                if !config.targets_explicitly_specified {
                    if let Err(error) = diff_source.diff(&DiffRequest {
                        workspace_root: config.workspace_root.abs_path.clone(),
                        base_ref: base_ref.clone(),
                        analysis_targets: config.analysis_targets.clone(),
                    }) {
                        let message = error.to_string();
                        emit_error(
                            self.format,
                            self.output.as_deref(),
                            &message,
                            error.source(),
                        );
                        return ExitCode::from(2);
                    }
                }
                if let Err(error) =
                    handle_gitignore_policy_for_config(&config, self.format == OutputFormat::Human)
                {
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &error.message,
                        Some(&error.source),
                    );
                    return ExitCode::from(2);
                }
            }
            match pipeline.run_diff(
                &config,
                view_options,
                &DiffConfig {
                    base_ref: base_ref.clone(),
                },
                &diff_source,
                &cache,
                plugin_host
                    .as_mut()
                    .map(|host| host as &mut dyn PluginPort<Error = PluginHostError>),
                llm_adapter.as_ref().map(|adapter| adapter as _),
            ) {
                Ok(result) => result,
                Err(error) => {
                    if let Some(plugin_host) = &plugin_host {
                        emit_evaluation_warnings(plugin_host.evaluation_warnings());
                    }
                    let message = error.to_string();
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &message,
                        error.source(),
                    );
                    return ExitCode::from(2);
                }
            }
        } else {
            let baseline_result = baseline_cache_adapter(self.cache_dir.as_ref())
                .ok()
                .and_then(|cache| {
                    let head_tree_hash = resolve_head_tree_hash(&config.workspace_root.abs_path)?;
                    Some((cache, head_tree_hash))
                });

            if self.cache_dir.is_none() && self.update_gitignore {
                if let Err(error) =
                    handle_gitignore_policy_for_config(&config, self.format == OutputFormat::Human)
                {
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &error.message,
                        Some(&error.source),
                    );
                    return ExitCode::from(2);
                }
            }

            let run_result = if let Some((cache, head_tree_hash)) = baseline_result.as_ref() {
                pipeline.run_full_workspace(
                    &config,
                    view_options,
                    head_tree_hash,
                    cache,
                    plugin_host
                        .as_mut()
                        .map(|host| host as &mut dyn PluginPort<Error = PluginHostError>),
                    llm_adapter.as_ref().map(|adapter| adapter as _),
                )
            } else {
                pipeline.run(
                    &config,
                    view_options,
                    plugin_host
                        .as_mut()
                        .map(|host| host as &mut dyn PluginPort<Error = PluginHostError>),
                    llm_adapter.as_ref().map(|adapter| adapter as _),
                )
            };

            match run_result {
                Ok(result) => result,
                Err(error) => {
                    if let Some(plugin_host) = &plugin_host {
                        emit_evaluation_warnings(plugin_host.evaluation_warnings());
                    }
                    let message = error.to_string();
                    emit_error(
                        self.format,
                        self.output.as_deref(),
                        &message,
                        error.source(),
                    );
                    return ExitCode::from(2);
                }
            }
        };
        debug!(
            exit_code = ?result.exit_code,
            diagnostic_count = result.report.diagnostics.diagnostics.len(),
            "check analysis completed"
        );
        if let Some(plugin_host) = &plugin_host {
            emit_evaluation_warnings(plugin_host.evaluation_warnings());
        }

        let rendered = match result.report.render(
            result.llm_suggestions.as_ref(),
            self.output.is_none() && std::io::stdout().is_terminal(),
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                let message = error.to_string();
                emit_error(
                    self.format,
                    self.output.as_deref(),
                    &message,
                    error.source(),
                );
                return ExitCode::from(2);
            }
        };
        if let Some(path) = &self.output {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                if let Err(error) = fs::create_dir_all(parent) {
                    let message = format!(
                        "failed to create output directory `{}`: {error}",
                        parent.display()
                    );
                    emit_error(self.format, self.output.as_deref(), &message, Some(&error));
                    return ExitCode::from(2);
                }
            }

            if let Err(error) = fs::write(path, format!("{rendered}\n")) {
                let message = format!("failed to write output file `{}`: {error}", path.display());
                emit_error(self.format, self.output.as_deref(), &message, Some(&error));
                return ExitCode::from(2);
            }

            if self.cache_dir.is_none() && !self.update_gitignore {
                handle_gitignore_policy(
                    self.update_gitignore,
                    &config.workspace_root.abs_path,
                    result.report.metadata.file_count,
                    self.format == OutputFormat::Human,
                );
            }

            if !self.quiet {
                let file_count = result.report.metadata.file_count;
                let diagnostic_count = result.report.diagnostics.diagnostics.len();
                eprintln!(
                    "wrote {} ({} {} analyzed, {} {})",
                    path.display(),
                    file_count,
                    if file_count == 1 { "file" } else { "files" },
                    diagnostic_count,
                    if diagnostic_count == 1 {
                        "diagnostic"
                    } else {
                        "diagnostics"
                    }
                );
            }
        } else {
            if self.cache_dir.is_none() && !self.update_gitignore {
                handle_gitignore_policy(
                    self.update_gitignore,
                    &config.workspace_root.abs_path,
                    result.report.metadata.file_count,
                    self.format == OutputFormat::Human,
                );
            }
            println!("{rendered}");
        }

        map_exit_code(result.exit_code)
    }
}

#[derive(Clone, Debug)]
struct ProgressToolCacheAdapter<T> {
    inner: T,
    progress: bool,
    setup_timeout: Option<Duration>,
}

impl<T> ProgressToolCacheAdapter<T> {
    fn new(inner: T, progress: bool, setup_timeout: Option<Duration>) -> Self {
        Self {
            inner,
            progress,
            setup_timeout,
        }
    }
}

impl<T> ToolCachePort for ProgressToolCacheAdapter<T>
where
    T: ToolCachePort,
{
    type Error = T::Error;

    fn resolve_bundle(
        &self,
        request: &ToolCacheRequest,
    ) -> Result<ResolvedToolBundle, Self::Error> {
        if !self.progress {
            return self.inner.resolve_bundle(request);
        }

        match self.setup_timeout {
            Some(timeout) => eprintln!(
                "  codeql: setup bundle ... (timeout {}; cold/cache-heavy cache phase)",
                format_elapsed(timeout)
            ),
            None => eprintln!("  codeql: setup bundle ..."),
        }
        let started = Instant::now();
        let bundle = self.inner.resolve_bundle(request)?;
        eprintln!(
            "  codeql: setup bundle done ({})",
            format_elapsed(started.elapsed())
        );
        Ok(bundle)
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() < 60 {
        let secs = elapsed.as_secs();
        let tenths = elapsed.subsec_millis() / 100;
        format!("{secs}.{tenths}s")
    } else {
        let secs = elapsed.as_secs();
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn baseline_cache_adapter(
    cache_dir: Option<&PathBuf>,
) -> Result<BaselineCacheAdapter, crate::adapters::baseline_cache::CacheError> {
    match cache_dir {
        Some(cache_dir) => Ok(BaselineCacheAdapter::with_cache_dir(cache_dir.clone())),
        None => BaselineCacheAdapter::new(),
    }
}

fn source_file_count_for_gitignore_policy(config: &ProjectConfig) -> Result<usize, std::io::Error> {
    let exclude_patterns = config
        .exclude_patterns
        .iter()
        .map(|pattern| pattern.pattern.clone())
        .collect::<Vec<_>>();
    let file_system = RealFileSystem;
    let collector = FileCollector::new(
        &file_system,
        &config.workspace_root.abs_path,
        &["py", "ts", "tsx", "rs", "go"],
        &exclude_patterns,
    );
    Ok(collector.collect(&config.analysis_targets)?.len())
}

struct GitignorePolicyError {
    message: String,
    source: std::io::Error,
}

fn handle_gitignore_policy_for_config(
    config: &ProjectConfig,
    human_output: bool,
) -> Result<(), GitignorePolicyError> {
    match source_file_count_for_gitignore_policy(config) {
        Ok(file_count) => {
            handle_gitignore_policy(
                true,
                &config.workspace_root.abs_path,
                file_count,
                human_output,
            );
            Ok(())
        }
        Err(source) => Err(GitignorePolicyError {
            message: format!(
                "failed to collect source files under `{}` for analysis target path(s) `{}`: {source}",
                config.workspace_root.abs_path.display(),
                format_analysis_targets(&config.analysis_targets),
            ),
            source,
        }),
    }
}

fn format_analysis_targets(analysis_targets: &[crate::domains::FilePath]) -> String {
    if analysis_targets.is_empty() {
        ".".to_owned()
    } else {
        analysis_targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn handle_gitignore_policy(
    update_gitignore: bool,
    workspace_root: &std::path::Path,
    file_count: usize,
    emit_messages: bool,
) {
    if update_gitignore && file_count == 0 {
        return;
    }

    if update_gitignore {
        match ensure_gitignore_entry(workspace_root) {
            Ok(GitignoreUpdate::Created) => {
                if emit_messages {
                    eprintln!("notice: created .gitignore with {KALOS_DIR_ENTRY} entry");
                }
            }
            Ok(GitignoreUpdate::Added) => {
                if emit_messages {
                    eprintln!("notice: added {KALOS_DIR_ENTRY} to .gitignore");
                }
            }
            Ok(GitignoreUpdate::Unchanged) => {}
            Err(error) => {
                if emit_messages {
                    eprintln!("warning: failed to update .gitignore: {error}");
                }
            }
        }
    } else {
        match gitignore_entry_status(workspace_root) {
            Ok(GitignoreStatus::EntryPresent) => {}
            Ok(GitignoreStatus::Missing | GitignoreStatus::EntryAbsent) => {
                if emit_messages {
                    eprintln!(
                        "warning: {KALOS_DIR_ENTRY} is not in .gitignore. \
                         Run with --update-gitignore to add it, \
                         or add it manually to avoid committing analysis cache."
                    );
                }
            }
            Err(error) => {
                if emit_messages {
                    eprintln!("warning: failed to inspect .gitignore: {error}");
                }
            }
        }
    }
}

const FULL_PLUGIN_AGGREGATE_FUEL_BUDGET: u64 = 30_000_000;
const DIFF_PLUGIN_AGGREGATE_FUEL_BUDGET: u64 = 5_000_000;

fn parse_min_language_ratio(value: &str) -> Result<f64, String> {
    let ratio = value
        .parse::<f64>()
        .map_err(|error| format!("invalid ratio `{value}`: {error}"))?;
    if (0.0..=1.0).contains(&ratio) {
        Ok(ratio)
    } else {
        Err(format!("ratio must be between 0.0 and 1.0, got {value}"))
    }
}

fn parse_codeql_ram_mib(value: &str) -> Result<u32, String> {
    let ram_mib = value
        .parse::<u32>()
        .map_err(|error| format!("invalid CodeQL RAM value `{value}`: {error}"))?;
    if ram_mib == 0 {
        return Err("CodeQL RAM must be greater than 0 MiB".to_owned());
    }
    Ok(ram_mib)
}

fn parse_codeql_timeout_secs(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|error| format!("invalid CodeQL timeout value `{value}`: {error}"))
}

fn effective_codeql_total_timeout(
    codeql_timeout: u64,
    explicit_total_timeout: Option<u64>,
) -> Option<u64> {
    explicit_total_timeout
        .or_else(|| (codeql_timeout > 0).then_some(DEFAULT_CODEQL_TOTAL_TIMEOUT_SECS))
}

fn codeql_setup_timeout(
    codeql_timeout: u64,
    codeql_total_timeout: Option<u64>,
) -> Option<Duration> {
    match (codeql_timeout, codeql_total_timeout) {
        (0, None | Some(0)) => None,
        (0, Some(total)) => Some(Duration::from_secs(total)),
        (phase, None | Some(0)) => Some(Duration::from_secs(phase)),
        (phase, Some(total)) => Some(Duration::from_secs(phase.min(total))),
    }
}

fn resolve_head_tree_hash(workspace_root: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!hash.is_empty()).then_some(hash)
}

fn builtin_metric_ids() -> BTreeSet<MetricId> {
    builtin_metric_definitions()
        .into_iter()
        .map(|definition| definition.id().clone())
        .collect()
}

const SARIF_TOOL_ERROR_EXIT_CODE: i64 = 2;

fn emit_error(
    format: OutputFormat,
    output: Option<&std::path::Path>,
    message: &str,
    source: Option<&(dyn std::error::Error + 'static)>,
) {
    match format {
        OutputFormat::Human => {
            let error_class = classify_error(message, source);
            let outcome = outcome_for_error_class(error_class);
            let mut document = message.to_owned();
            if error_class == "codeql_infrastructure" {
                document.push_str(&format!("\nerror class: {error_class}"));
            }
            document.push_str(&format!("\noutcome: {outcome}"));
            if let Some(path) = output {
                if write_error_output_file(path, &document) {
                    return;
                }
            }

            eprintln!("{message}");
            if error_class == "codeql_infrastructure" {
                eprintln!("error class: {error_class}");
            }
            eprintln!("outcome: {outcome}");
        }
        OutputFormat::Json => {
            let error_class = classify_error(message, source);
            let mut payload = json!({
                "error": true,
                "error_class": error_class,
                "outcome": outcome_for_error_class(error_class),
                "message": message,
            });
            if let Some(source) = source {
                payload["cause"] = json!(source.to_string());
            }
            let document = payload.to_string();
            if let Some(path) = output {
                if write_error_output_file(path, &document) {
                    return;
                }
            }

            println!("{document}");
        }
        OutputFormat::Sarif => {
            let cause = source.map(|source| source.to_string());
            let error_class = classify_error(message, source);
            let document = render_sarif_error_document(
                message,
                cause.as_deref(),
                env!("CARGO_PKG_VERSION"),
                SARIF_TOOL_ERROR_EXIT_CODE,
                error_class,
            );
            if let Some(path) = output {
                if write_error_output_file(path, &document) {
                    return;
                }
            }

            println!("{document}");
        }
    }
}

fn classify_error(
    message: &str,
    source: Option<&(dyn std::error::Error + 'static)>,
) -> &'static str {
    let cause = source.map(|source| source.to_string()).unwrap_or_default();
    let text = format!("{message}\n{cause}");

    if text.contains("`--llm` requires KALOS_LLM_API_KEY to be set")
        || text.contains("unbounded large-repo CodeQL analysis skipped before CodeQL execution")
    {
        "expected_skip"
    } else if text.contains("analysis target path")
        && (text.contains("No such file or directory")
            || text.contains("not found")
            || text.contains("does not exist")
            || text.contains("is outside workspace root"))
    {
        "input_error"
    } else if text.contains("failed to resolve CodeQL bundle")
        || text.contains("CodeQL bundle bootstrap lock")
        || text.contains("managed CodeQL cache")
        || text.contains("failed to extract CodeQL bundle")
    {
        "codeql_infrastructure"
    } else if text.contains("CodeQL `") || text.contains("failed to execute `") {
        "codeql_extraction"
    } else {
        "tool_error"
    }
}

fn write_error_output_file(path: &std::path::Path, document: &str) -> bool {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }

    fs::write(path, format!("{document}\n")).is_ok()
}

fn emit_module_load_warnings(warnings: &[ModuleLoadWarning]) {
    for warning in warnings {
        eprintln!(
            "plugin load warning [{:?}] {}: {}",
            warning.kind,
            warning.path.display(),
            warning.message
        );
    }
}

fn emit_evaluation_warnings(warnings: &[EvaluationWarning]) {
    for warning in warnings {
        let metric = warning
            .metric_id
            .as_ref()
            .map(|metric_id| metric_id.as_str().to_owned())
            .unwrap_or_else(|| "-".to_owned());
        let scope = warning
            .scope_id
            .as_ref()
            .map(|scope_id| {
                format!(
                    "{}:{}:{}",
                    analysis_level_label(scope_id.level),
                    scope_id.qualified_name,
                    scope_id.file_path.as_str()
                )
            })
            .unwrap_or_else(|| "-".to_owned());
        eprintln!(
            "plugin evaluation warning [{:?}] {} metric={} scope={}: {}",
            warning.kind,
            warning.path.display(),
            metric,
            scope,
            warning.message
        );
    }
}

fn analysis_level_label(level: crate::domains::AnalysisLevel) -> &'static str {
    match level {
        crate::domains::AnalysisLevel::Function => "function",
        crate::domains::AnalysisLevel::Module => "module",
        crate::domains::AnalysisLevel::Project => "project",
    }
}

fn map_exit_code(code: crate::domains::diagnostics::ExitCode) -> ExitCode {
    match code {
        crate::domains::diagnostics::ExitCode::Success => ExitCode::SUCCESS,
        crate::domains::diagnostics::ExitCode::DiagnosticFailure => ExitCode::from(1),
        crate::domains::diagnostics::ExitCode::ToolError => ExitCode::from(2),
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Sarif,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum RequestedLevel {
    Function,
    Module,
    #[default]
    Project,
    All,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum MinimumSeverity {
    Error,
    Warning,
    Info,
}

impl From<OutputFormat> for DomainOutputFormat {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Human => Self::Human,
            OutputFormat::Json => Self::Json,
            OutputFormat::Sarif => Self::Sarif,
        }
    }
}

impl From<RequestedLevel> for DomainRequestedLevel {
    fn from(value: RequestedLevel) -> Self {
        match value {
            RequestedLevel::Function => Self::Function,
            RequestedLevel::Module => Self::Module,
            RequestedLevel::Project => Self::Project,
            RequestedLevel::All => Self::All,
        }
    }
}

impl From<MinimumSeverity> for Severity {
    fn from(value: MinimumSeverity) -> Self {
        match value {
            MinimumSeverity::Error => Severity::Error,
            MinimumSeverity::Warning => Severity::Warning,
            MinimumSeverity::Info => Severity::Info,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{classify_error, codeql_setup_timeout, effective_codeql_total_timeout};

    #[test]
    fn classify_error_marks_codeql_bundle_cache_lock_as_infrastructure() {
        let message = "failed to resolve CodeQL bundle: failed to extract CodeQL bundle v2.25.1";
        let source = std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "CodeQL bundle bootstrap lock `/cache/codeql/.codeql-bundle-2.25.1.lock.d` showed no progress for 30000ms; remove the stale lock directory and retry",
        );

        assert_eq!(
            classify_error(message, Some(&source)),
            "codeql_infrastructure"
        );
    }

    #[test]
    fn classify_error_marks_codeql_command_failures_as_extraction() {
        let message = "CodeQL `query run` failed for `rust` (exit code 2)";

        assert_eq!(classify_error(message, None), "codeql_extraction");
    }

    #[test]
    fn classify_error_marks_missing_llm_api_key_as_expected_skip() {
        let message = "`--llm` requires KALOS_LLM_API_KEY to be set";

        assert_eq!(classify_error(message, None), "expected_skip");
    }

    #[test]
    fn classify_error_marks_unbounded_large_repo_guard_as_expected_skip() {
        let message = "unbounded large-repo CodeQL analysis skipped before CodeQL execution: found 100 source files";

        assert_eq!(classify_error(message, None), "expected_skip");
    }

    #[test]
    fn classify_error_marks_missing_analysis_target_as_input_error() {
        let message = "failed to collect source files under `/repo` for analysis target path(s) `missing`: No such file or directory (os error 2)";

        assert_eq!(classify_error(message, None), "input_error");
    }

    #[test]
    fn classify_error_marks_extraction_wrapped_missing_target_as_input_error() {
        let message = "failed to extract CPG: failed to collect source files under `/repo` for analysis target path(s) `__kalos_missing_path__`: No such file or directory (os error 2)";

        assert_eq!(classify_error(message, None), "input_error");
    }

    #[test]
    fn codeql_setup_timeout_uses_stricter_phase_or_total_budget() {
        assert_eq!(
            codeql_setup_timeout(240, Some(1200)),
            Some(Duration::from_secs(240))
        );
        assert_eq!(
            codeql_setup_timeout(240, Some(60)),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            codeql_setup_timeout(0, Some(60)),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            codeql_setup_timeout(60, None),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            codeql_setup_timeout(60, Some(0)),
            Some(Duration::from_secs(60))
        );
        assert_eq!(codeql_setup_timeout(0, None), None);
        assert_eq!(codeql_setup_timeout(0, Some(0)), None);
    }

    #[test]
    fn effective_codeql_total_timeout_preserves_zero_phase_timeout_contract() {
        assert_eq!(effective_codeql_total_timeout(240, None), Some(1200));
        assert_eq!(effective_codeql_total_timeout(240, Some(60)), Some(60));
        assert_eq!(effective_codeql_total_timeout(0, None), None);
        assert_eq!(effective_codeql_total_timeout(0, Some(60)), Some(60));
    }
}
