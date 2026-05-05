use std::collections::BTreeSet;
use std::env;
use std::error::Error as _;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

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
use crate::adapters::extractor::CodeQlAdapter;
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
    render_sarif_error_document,
};
use crate::platform::fs::RealFileSystem;
use crate::platform::process::SystemCommandRunner;
use crate::ports::PluginPort;

#[derive(Debug, Clone, Args)]
#[command(
    about = "run code quality analysis",
    after_help = "NOTE: Normal `check` execution may write to locations such as:\n  \
                  - `<repo>/.kalos/codeql/<language>/` stores per-language CodeQL databases.\n  \
                  - `$KALOS_CACHE_DIR/baselines/` may store cached baselines for full-workspace runs in Git repositories.\n  \
                  - `<repo>/.gitignore` is only created or updated when --update-gitignore is passed.\n\n\
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
        default_value_t = RequestedLevel::All,
        help = "analysis granularity level"
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
    #[arg(long, help = "enable llm-assisted analysis")]
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
        help = "minimum file ratio (0.0-1.0) for a language to be analyzed (default: 0.05)"
    )]
    pub min_language_ratio: Option<f64>,
    #[arg(
        long,
        help = "include test files in module-level diagnostics (KAL-M001, KAL-M003)",
        long_help = "include test files in module-level diagnostics (KAL-M001, KAL-M003)

By default, KAL-M001 (module fan-out) and KAL-M003 (instability) diagnostics \
are suppressed for test files because tests inherently import many modules and \
score as highly unstable. Test files are NOT excluded from analysis: their \
metrics are computed, they count toward files_analyzed, and every other rule \
(for example KAL-PAT003) still applies to them.

Pass --include-tests to re-enable KAL-M001 and KAL-M003 diagnostics on test files. \
The flag has no observable effect on JSON output when no test module crosses the \
KAL-M001 or KAL-M003 threshold configured in .kalos.toml.

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
        if self.format == OutputFormat::Human {
            if let Some(notice) = platform.and_then(|platform| platform.emulation_notice()) {
                eprintln!("{notice}");
            }
        }
        let codeql_version = manifest.version.clone();
        let tool_cache = ManagedToolCacheAdapter::new(manifest);
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
        if self.format == OutputFormat::Human {
            extractor = extractor.with_progress();
        }
        if platform.map_or(false, |platform| platform.is_emulated()) {
            extractor = extractor.with_emulated();
        }
        if let Some(ratio) = self.min_language_ratio {
            extractor = extractor.with_min_language_ratio(ratio);
        }
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
            let cache = match BaselineCacheAdapter::new() {
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
            match pipeline.run_diff(
                &config,
                view_options,
                &DiffConfig {
                    base_ref: base_ref.clone(),
                },
                &GitDiffAdapter,
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
            let baseline_result = BaselineCacheAdapter::new().ok().and_then(|cache| {
                let head_tree_hash = resolve_head_tree_hash(&config.workspace_root.abs_path)?;
                Some((cache, head_tree_hash))
            });

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

            handle_gitignore_policy(
                self.update_gitignore,
                &config.workspace_root.abs_path,
                result.report.metadata.file_count,
                self.format == OutputFormat::Human,
            );

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
            handle_gitignore_policy(
                self.update_gitignore,
                &config.workspace_root.abs_path,
                result.report.metadata.file_count,
                self.format == OutputFormat::Human,
            );
            println!("{rendered}");
        }

        map_exit_code(result.exit_code)
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
        OutputFormat::Human => eprintln!("{message}"),
        OutputFormat::Json => {
            let mut payload = json!({
                "error": true,
                "message": message,
            });
            if let Some(source) = source {
                payload["cause"] = json!(source.to_string());
            }
            eprintln!("{payload}");
        }
        OutputFormat::Sarif => {
            let cause = source.map(|source| source.to_string());
            let document = render_sarif_error_document(
                message,
                cause.as_deref(),
                env!("CARGO_PKG_VERSION"),
                SARIF_TOOL_ERROR_EXIT_CODE,
            );
            if let Some(path) = output {
                if let Some(parent) = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    if fs::create_dir_all(parent).is_err() {
                        println!("{document}");
                        return;
                    }
                }

                if fs::write(path, format!("{document}\n")).is_ok() {
                    return;
                }
            }

            println!("{document}");
        }
    }
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
    Project,
    #[default]
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
