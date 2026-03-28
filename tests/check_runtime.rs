use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use kalos::adapters::dependency_resolver::StubDependencyResolver;
use kalos::adapters::extractor::CodeQlAdapter;
use kalos::application::pipeline::AnalysisPipeline;
use kalos::domains::config::{Defaults, ProjectConfig, WorkspaceRoot};
use kalos::domains::reporting::{OutputFormat, ReportViewOptions, RequestedLevel};
use kalos::domains::{FilePath, Severity};
use kalos::platform::fs::InMemoryFileSystem;
use kalos::platform::process::{MockCommandRunner, ProcessOutput};
use kalos::ports::dependency_resolver::{DependencyResolutionRequest, DependencyResolverPort};
use kalos::ports::extractor::{ExtractionRequest, ExtractorPort};
use kalos::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

#[derive(Clone, Debug)]
struct MockToolCachePort {
    bundle: ResolvedToolBundle,
}

impl ToolCachePort for MockToolCachePort {
    type Error = Infallible;

    fn resolve_bundle(
        &self,
        _request: &ToolCacheRequest,
    ) -> Result<ResolvedToolBundle, Self::Error> {
        Ok(self.bundle.clone())
    }
}

#[test]
fn codeql_adapter_with_stub_dependency_resolver_emits_req_func_007_warning() {
    let mut file_system = InMemoryFileSystem::new();
    file_system.insert(
        "/workspace/src/lib.rs",
        "pub fn placeholder() -> i32 { 1 }\n",
    );
    let command_runner = MockCommandRunner::new();
    command_runner
        .push_result(Ok(ProcessOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        }))
        .unwrap();
    command_runner
        .push_result(Ok(ProcessOutput {
            stdout: load_fixture("rust.json").into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        }))
        .unwrap();
    let extractor = CodeQlAdapter::new(
        file_system,
        command_runner,
        MockToolCachePort {
            bundle: ResolvedToolBundle {
                tool_name: "codeql".to_owned(),
                version: "2.16.0".to_owned(),
                cache_path: PathBuf::from("/cache/codeql/2.16.0"),
                checksum: "a".repeat(64),
            },
        },
        "2.16.0",
        Vec::new(),
    );
    let resolver = StubDependencyResolver;

    let mut analysis = extractor
        .extract(&ExtractionRequest {
            workspace_root: PathBuf::from("/workspace"),
            analysis_targets: vec![FilePath::from(".")],
        })
        .unwrap();
    let dependency_resolution = resolver
        .resolve(&DependencyResolutionRequest {
            workspace_root: PathBuf::from("/workspace"),
            source_files: analysis.source_files.clone(),
        })
        .unwrap();
    analysis
        .cpg
        .nodes
        .extend(dependency_resolution.external_symbols);
    analysis.warnings.extend(dependency_resolution.warnings);

    assert_eq!(analysis.source_files.len(), 1);
    assert_eq!(analysis.warnings.len(), 1);
    assert_eq!(
        analysis.warnings[0].message,
        "External symbol resolution is not yet implemented (REQ-FUNC-007). Analysis results may be incomplete for cross-crate/cross-package references."
    );
}

#[test]
fn managed_tool_cache_cache_miss_exits_with_code_2_and_clear_error() {
    let temp = seeded_workspace();
    let cache_dir = temp.path().join("missing-cache");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("failed to resolve CodeQL bundle"))
        .stderr(predicate::str::contains("Run kalos bootstrap while online"));
}

#[test]
fn analysis_pipeline_runs_end_to_end_with_codeql_adapter_and_stub_dependency_resolver() {
    let mut file_system = InMemoryFileSystem::new();
    file_system.insert(
        "/workspace/src/lib.rs",
        "pub fn placeholder() -> i32 { 1 }\n",
    );
    let command_runner = MockCommandRunner::new();
    command_runner
        .push_result(Ok(ProcessOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        }))
        .unwrap();
    command_runner
        .push_result(Ok(ProcessOutput {
            stdout: load_fixture("rust.json").into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        }))
        .unwrap();
    let extractor = CodeQlAdapter::new(
        file_system,
        command_runner,
        MockToolCachePort {
            bundle: ResolvedToolBundle {
                tool_name: "codeql".to_owned(),
                version: "2.16.0".to_owned(),
                cache_path: PathBuf::from("/cache/codeql/2.16.0"),
                checksum: "a".repeat(64),
            },
        },
        "2.16.0",
        Vec::new(),
    );
    let pipeline = AnalysisPipeline::new(extractor, StubDependencyResolver);

    let result = pipeline
        .run(
            &fixture_config(),
            ReportViewOptions {
                requested_level: RequestedLevel::All,
                output_format: OutputFormat::Json,
                strict: false,
                minimum_severity: Some(Severity::Info),
            },
            None,
        )
        .unwrap();

    assert_eq!(
        result.report.metadata.analysis_targets,
        vec![FilePath::from(".")]
    );
    assert!(!result.report.metrics.is_empty());
    assert!(matches!(
        result.exit_code,
        kalos::domains::diagnostics::ExitCode::Success
            | kalos::domains::diagnostics::ExitCode::DiagnosticFailure
    ));
}

fn fixture_config() -> ProjectConfig {
    ProjectConfig {
        workspace_root: WorkspaceRoot {
            abs_path: "/workspace".into(),
        },
        analysis_targets: vec![FilePath::from(".")],
        rules: Defaults::default().rules,
        exclude_patterns: Vec::new(),
        score_weights: Defaults::default().score_weights,
        plugin_manifest: Default::default(),
        targets_explicitly_specified: false,
    }
}

fn seeded_workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn placeholder() -> i32 { 1 }\n",
    )
    .unwrap();
    temp
}

fn load_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codeql")
        .join(name);
    fs::read_to_string(path).unwrap()
}
