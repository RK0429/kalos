use std::collections::BTreeSet;
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};

use crate::adapters::baseline_cache::BaselineCacheAdapter;
use crate::adapters::dependency_resolver::StubDependencyResolver;
use crate::adapters::diff_source::GitDiffAdapter;
use crate::adapters::extractor::CodeQlAdapter;
use crate::adapters::llm::HttpLlmAdapter;
use crate::adapters::llm::http::validate_llm_config;
use crate::adapters::plugin::{
    EvaluationWarning, ModuleLoadWarning, PluginHostError, WasmPluginHost,
};
use crate::adapters::tool_cache::{ManagedToolCacheAdapter, codeql_bundle_manifest};
use crate::application::pipeline::{AnalysisPipeline, DiffConfig};
use crate::domains::MetricId;
use crate::domains::Severity;
use crate::domains::config::{Defaults, ProjectConfig, ResolveOptions};
use crate::domains::metrics::builtin_metric_definitions;
use crate::domains::reporting::{
    OutputFormat as DomainOutputFormat, ReportViewOptions, RequestedLevel as DomainRequestedLevel,
};
use crate::platform::fs::RealFileSystem;
use crate::platform::process::SystemCommandRunner;
use crate::ports::PluginPort;

#[derive(Debug, Clone, Args)]
pub struct CheckCommand {
    #[arg(value_name = "path")]
    pub paths: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    pub format: OutputFormat,
    #[arg(long, value_enum, default_value_t = RequestedLevel::All)]
    pub level: RequestedLevel,
    #[arg(long, value_name = "path")]
    pub config: Option<PathBuf>,
    #[arg(long, value_name = "pattern")]
    pub exclude: Vec<String>,
    #[arg(long, value_enum)]
    pub severity: Option<MinimumSeverity>,
    #[arg(long, value_name = "base-ref")]
    pub diff: Option<String>,
    #[arg(long)]
    pub llm: bool,
    #[arg(long)]
    pub strict: bool,
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
                eprintln!("failed to determine current directory: {error}");
                return ExitCode::from(2);
            }
        };

        let options = self.resolve_options(cwd);
        let config = match ProjectConfig::load_and_resolve(&options, &Defaults::default()) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        let llm_adapter = if self.llm {
            match validate_llm_config() {
                Ok(config) => Some(HttpLlmAdapter::new(config)),
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            }
        } else {
            None
        };

        let manifest = match codeql_bundle_manifest() {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        let codeql_version = manifest.version.clone();
        let tool_cache = ManagedToolCacheAdapter::new(manifest);
        let exclude_patterns = config
            .exclude_patterns
            .iter()
            .map(|pattern| pattern.pattern.clone())
            .collect::<Vec<_>>();
        let extractor = CodeQlAdapter::new(
            RealFileSystem,
            SystemCommandRunner,
            tool_cache,
            codeql_version,
            exclude_patterns,
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
        };

        let result = if let Some(base_ref) = &self.diff {
            let cache = match BaselineCacheAdapter::new() {
                Ok(cache) => cache,
                Err(error) => {
                    eprintln!("{error}");
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
                    eprintln!("{error}");
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
                    eprintln!("{error}");
                    return ExitCode::from(2);
                }
            }
        };
        if let Some(plugin_host) = &plugin_host {
            emit_evaluation_warnings(plugin_host.evaluation_warnings());
        }

        let rendered = match result.report.render(
            result.llm_suggestions.as_ref(),
            std::io::stdout().is_terminal(),
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };
        println!("{rendered}");

        map_exit_code(result.exit_code)
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
