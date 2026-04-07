use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use kalos::adapters::dependency_resolver::StubDependencyResolver;
use kalos::adapters::extractor::CodeQlAdapter;
use kalos::application::pipeline::AnalysisPipeline;
use kalos::domains::config::{Defaults, ProjectConfig, WorkspaceRoot};
use kalos::domains::reporting::{OutputFormat, ReportViewOptions, RequestedLevel};
use kalos::domains::{FilePath, Severity};
use kalos::platform::fs::InMemoryFileSystem;
use kalos::platform::process::{MockCommandRunner, ProcessOutput};
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
fn determinism_10_runs_produce_identical_json_hash() {
    let fixture = load_fixture("rust.json");
    let config = fixture_config();
    let view_options = ReportViewOptions {
        requested_level: RequestedLevel::All,
        output_format: OutputFormat::Json,
        strict: false,
        minimum_severity: Some(Severity::Info),
    };

    let hashes = (0..10)
        .map(|_| run_pipeline_hash(&fixture, &config, view_options.clone()))
        .collect::<Vec<_>>();

    let first_hash = hashes.first().expect("hashes should not be empty").clone();
    assert!(
        hashes.iter().all(|hash| *hash == first_hash),
        "determinism hash mismatch: {hashes:?}"
    );
}

fn run_pipeline_hash(
    fixture: &str,
    config: &ProjectConfig,
    view_options: ReportViewOptions,
) -> Vec<u8> {
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
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        }))
        .unwrap();
    command_runner
        .push_result(Ok(ProcessOutput {
            stdout: fixture.as_bytes().to_vec(),
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
    let result = pipeline.run(config, view_options, None, None).unwrap();
    let rendered = result.report.render_json(None).unwrap();

    Sha256::digest(rendered.as_bytes()).to_vec()
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
        include_tests: false,
        targets_explicitly_specified: false,
    }
}

fn load_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codeql")
        .join(name);
    fs::read_to_string(path).unwrap()
}
