use std::collections::BTreeMap;
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};

use crate::application::pipeline::AnalysisPipeline;
use crate::domains::config::{Defaults, ProjectConfig, ResolveOptions};
use crate::domains::cpg::{CpgId, SourceAnalysis, UnifiedCpg};
use crate::domains::reporting::{
    OutputFormat as DomainOutputFormat, ReportViewOptions, RequestedLevel as DomainRequestedLevel,
};
use crate::domains::{FilePath, Severity};
use crate::ports::extractor::{ExtractionRequest, ExtractorPort};

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

        if self.diff.is_some() {
            eprintln!("diff mode is not implemented yet");
            return ExitCode::from(2);
        }

        let pipeline = AnalysisPipeline::new(StubExtractor);
        let view_options = ReportViewOptions {
            requested_level: self.level.into(),
            output_format: self.format.into(),
            strict: self.strict,
            minimum_severity: self.severity.map(Severity::from),
        };

        let result = match pipeline.run(&config, view_options) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };

        let rendered = match result.report.render(None, std::io::stdout().is_terminal()) {
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

#[derive(Copy, Clone, Debug)]
struct StubExtractor;

impl ExtractorPort for StubExtractor {
    type Error = std::convert::Infallible;

    fn extract(&self, _request: &ExtractionRequest) -> Result<SourceAnalysis, Self::Error> {
        Ok(SourceAnalysis {
            cpg: UnifiedCpg {
                id: CpgId::from("stub"),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            source_files: BTreeMap::<FilePath, crate::domains::cpg::SourceFile>::new(),
            suppressions: Vec::new(),
            warnings: Vec::new(),
        })
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
