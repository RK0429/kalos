//! Pipeline-level regression benchmark for the mock CodeQL analysis path.
//!
//! This ignored integration test measures kalos' in-process analysis pipeline
//! over a synthetic corpus using mock CodeQL adapters. It is intended to detect
//! regressions in the pipeline's own computation such as metric calculation,
//! scoring, report assembly, and impact analysis.
//!
//! It does not validate the real-world REQ-NF-001 (60s full) or REQ-NF-002
//! (10s diff) thresholds. Those end-to-end thresholds require actual CodeQL
//! execution on the `bench-linux-x64` profile.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use kalos::adapters::dependency_resolver::StubDependencyResolver;
use kalos::adapters::extractor::CodeQlAdapter;
use kalos::application::pipeline::{AnalysisPipeline, DiffConfig};
use kalos::domains::config::{Defaults, ProjectConfig, WorkspaceRoot};
use kalos::domains::cpg::Language;
use kalos::domains::impact::{BaselineFingerprint, DiffBaseline};
use kalos::domains::reporting::{OutputFormat, ReportViewOptions, RequestedLevel};
use kalos::domains::{FilePath, Severity};
use kalos::platform::fs::InMemoryFileSystem;
use kalos::platform::process::{MockCommandRunner, ProcessOutput};
use kalos::ports::cache::CachePort;
use kalos::ports::diff_source::{DiffRequest, DiffSnapshot, DiffSourcePort};
use kalos::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

const WORKSPACE_ROOT: &str = "/workspace";
const FILES_PER_LANGUAGE: usize = 13;
const FUNCTIONS_PER_FILE: usize = 18;
const TARGET_LOC_PER_LANGUAGE: usize = 2_500;
const DEFAULT_RUNS: usize = 30;
const DEFAULT_FULL_THRESHOLD_SECS: f64 = 0.005;
const DEFAULT_DIFF_THRESHOLD_SECS: f64 = 0.002;
const BENCHMARK_SCOPE_KIND: &str = "pipeline_regression_mock_codeql";

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

#[derive(Debug, Default)]
struct InMemoryBaselineCache {
    baseline: Mutex<Option<DiffBaseline>>,
}

impl InMemoryBaselineCache {
    fn has_baseline(&self) -> bool {
        self.baseline
            .lock()
            .expect("baseline cache lock should not be poisoned")
            .is_some()
    }
}

impl CachePort for InMemoryBaselineCache {
    type Error = Infallible;

    fn load(&self, fingerprint: &BaselineFingerprint) -> Result<Option<DiffBaseline>, Self::Error> {
        let stored = self
            .baseline
            .lock()
            .expect("baseline cache lock should not be poisoned")
            .clone();
        Ok(stored.filter(|baseline| baseline.fingerprint == *fingerprint))
    }

    fn store(&self, baseline: &DiffBaseline) -> Result<(), Self::Error> {
        *self
            .baseline
            .lock()
            .expect("baseline cache lock should not be poisoned") = Some(baseline.clone());
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct StaticDiffSource {
    snapshot: DiffSnapshot,
}

impl StaticDiffSource {
    fn new(snapshot: DiffSnapshot) -> Self {
        Self { snapshot }
    }
}

impl DiffSourcePort for StaticDiffSource {
    type Error = Infallible;

    fn diff(&self, _request: &DiffRequest) -> Result<DiffSnapshot, Self::Error> {
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone, Debug)]
struct SyntheticCorpus {
    file_system: InMemoryFileSystem,
    total_loc: usize,
    file_count: usize,
    loc_by_language: BTreeMap<String, usize>,
}

impl SyntheticCorpus {
    fn generate() -> Self {
        let mut file_system = InMemoryFileSystem::new();
        let mut file_count = 0;
        let mut loc_by_language = BTreeMap::new();

        for (language, files) in [
            ("rust", rust_corpus_files()),
            ("typescript", typescript_corpus_files()),
            ("python", python_corpus_files()),
            ("go", go_corpus_files()),
        ] {
            let loc = files
                .iter()
                .map(|(_, contents)| contents.lines().count())
                .sum::<usize>();
            file_count += files.len();
            loc_by_language.insert(language.to_owned(), loc);

            for (path, contents) in files {
                file_system.insert(format!("{WORKSPACE_ROOT}/{path}"), contents);
            }
        }

        let total_loc = loc_by_language.values().sum();

        Self {
            file_system,
            total_loc,
            file_count,
            loc_by_language,
        }
    }
}

#[derive(Debug)]
struct BenchmarkSettings {
    runs: usize,
    full_threshold_secs: f64,
    diff_threshold_secs: f64,
}

impl BenchmarkSettings {
    fn from_env() -> Self {
        Self {
            runs: parse_usize_env("BENCH_RUNS", DEFAULT_RUNS),
            full_threshold_secs: parse_f64_env(
                "BENCH_FULL_THRESHOLD_SECS",
                DEFAULT_FULL_THRESHOLD_SECS,
            ),
            diff_threshold_secs: parse_f64_env(
                "BENCH_DIFF_THRESHOLD_SECS",
                DEFAULT_DIFF_THRESHOLD_SECS,
            ),
        }
    }
}

#[derive(Debug)]
struct BenchmarkResult {
    mode: &'static str,
    run_times_secs: Vec<f64>,
    p95_secs: f64,
    threshold_secs: f64,
    pass: bool,
}

impl BenchmarkResult {
    fn from_times(mode: &'static str, run_times_secs: Vec<f64>, threshold_secs: f64) -> Self {
        let p95_secs = p95(&run_times_secs);
        let pass = p95_secs <= threshold_secs;
        Self {
            mode,
            run_times_secs,
            p95_secs,
            threshold_secs,
            pass,
        }
    }
}

#[test]
#[ignore = "pipeline regression benchmark with mock CodeQL adapters; run explicitly with --ignored (not REQ-NF-001/002 e2e validation)"]
fn mock_pipeline_benchmarks_report_p95_and_enforce_thresholds() {
    let settings = BenchmarkSettings::from_env();
    let corpus = SyntheticCorpus::generate();

    assert!(
        corpus.total_loc >= TARGET_LOC_PER_LANGUAGE * 4,
        "synthetic corpus should be at least 10k LOC, got {}",
        corpus.total_loc
    );
    for (language, loc) in &corpus.loc_by_language {
        assert!(
            *loc >= TARGET_LOC_PER_LANGUAGE,
            "{language} corpus should be at least {TARGET_LOC_PER_LANGUAGE} LOC, got {loc}"
        );
    }

    let full = benchmark_full(&corpus, &settings);
    let diff = benchmark_diff(&corpus, &settings);

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scope": benchmark_scope_json(),
            "corpus": {
                "file_count": corpus.file_count,
                "total_loc": corpus.total_loc,
                "loc_by_language": corpus.loc_by_language,
            },
            "runs": settings.runs,
            "results": [
                benchmark_result_json(&full),
                benchmark_result_json(&diff),
            ],
        }))
        .expect("benchmark output should serialize")
    );

    assert!(
        full.pass,
        "full benchmark p95 {:.6}s exceeded threshold {:.6}s",
        full.p95_secs, full.threshold_secs
    );
    assert!(
        diff.pass,
        "diff benchmark p95 {:.6}s exceeded threshold {:.6}s",
        diff.p95_secs, diff.threshold_secs
    );
}

fn benchmark_full(corpus: &SyntheticCorpus, settings: &BenchmarkSettings) -> BenchmarkResult {
    let mut run_times_secs = Vec::with_capacity(settings.runs);

    for _ in 0..settings.runs {
        let command_runner = MockCommandRunner::new();
        enqueue_codeql_results(&command_runner, full_language_order());
        let pipeline = AnalysisPipeline::new(
            build_extractor(corpus.file_system.clone(), command_runner),
            StubDependencyResolver,
        );

        let start = Instant::now();
        pipeline
            .run(&fixture_config(), fixture_view_options(), None, None)
            .expect("full benchmark run should succeed");
        run_times_secs.push(start.elapsed().as_secs_f64());
    }

    BenchmarkResult::from_times("full", run_times_secs, settings.full_threshold_secs)
}

fn benchmark_diff(corpus: &SyntheticCorpus, settings: &BenchmarkSettings) -> BenchmarkResult {
    let cache = InMemoryBaselineCache::default();
    warm_diff_baseline(corpus, &cache);

    let mut run_times_secs = Vec::with_capacity(settings.runs);
    for _ in 0..settings.runs {
        let command_runner = MockCommandRunner::new();
        enqueue_codeql_results(&command_runner, &[Language::Rust]);
        let pipeline = AnalysisPipeline::new(
            build_extractor(corpus.file_system.clone(), command_runner),
            StubDependencyResolver,
        );
        let diff_source = StaticDiffSource::new(diff_snapshot());

        let start = Instant::now();
        pipeline
            .run_diff(
                &fixture_config(),
                fixture_view_options(),
                &DiffConfig {
                    base_ref: "HEAD~1".to_owned(),
                },
                &diff_source,
                &cache,
                None,
                None,
            )
            .expect("diff benchmark run should succeed");
        run_times_secs.push(start.elapsed().as_secs_f64());
    }

    BenchmarkResult::from_times("diff", run_times_secs, settings.diff_threshold_secs)
}

fn warm_diff_baseline(corpus: &SyntheticCorpus, cache: &InMemoryBaselineCache) {
    let command_runner = MockCommandRunner::new();
    enqueue_codeql_results(&command_runner, &[Language::Rust]);
    enqueue_codeql_results(&command_runner, full_language_order());
    let pipeline = AnalysisPipeline::new(
        build_extractor(corpus.file_system.clone(), command_runner),
        StubDependencyResolver,
    );

    pipeline
        .run_diff(
            &fixture_config(),
            fixture_view_options(),
            &DiffConfig {
                base_ref: "HEAD~1".to_owned(),
            },
            &StaticDiffSource::new(diff_snapshot()),
            cache,
            None,
            None,
        )
        .expect("diff warmup should populate baseline cache");

    assert!(cache.has_baseline(), "diff warmup should store a baseline");
}

fn build_extractor(
    file_system: InMemoryFileSystem,
    command_runner: MockCommandRunner,
) -> CodeQlAdapter<InMemoryFileSystem, MockCommandRunner, MockToolCachePort> {
    CodeQlAdapter::new(
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
    )
}

fn enqueue_codeql_results(command_runner: &MockCommandRunner, languages: &[Language]) {
    for &language in languages {
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .expect("database create mock should queue successfully");
        command_runner
            .push_result(Ok(ProcessOutput {
                stdout: load_fixture(language).into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            }))
            .expect("query run mock should queue successfully");
    }
}

fn full_language_order() -> &'static [Language] {
    &[
        Language::Python,
        Language::TypeScript,
        Language::Rust,
        Language::Go,
    ]
}

fn diff_snapshot() -> DiffSnapshot {
    DiffSnapshot {
        base_snapshot_hash: "benchmark-base-tree".to_owned(),
        changed_files: BTreeSet::from([FilePath::from("src/lib.rs")]),
    }
}

fn fixture_config() -> ProjectConfig {
    ProjectConfig {
        workspace_root: WorkspaceRoot {
            abs_path: WORKSPACE_ROOT.into(),
        },
        analysis_targets: vec![FilePath::from(".")],
        rules: Defaults::default().rules,
        exclude_patterns: Vec::new(),
        score_weights: Defaults::default().score_weights,
        plugin_manifest: Default::default(),
        targets_explicitly_specified: false,
    }
}

fn fixture_view_options() -> ReportViewOptions {
    ReportViewOptions {
        requested_level: RequestedLevel::All,
        output_format: OutputFormat::Json,
        strict: false,
        minimum_severity: Some(Severity::Info),
    }
}

fn benchmark_result_json(result: &BenchmarkResult) -> serde_json::Value {
    json!({
        "mode": result.mode,
        "run_times_secs": result.run_times_secs,
        "p95_secs": result.p95_secs,
        "threshold_secs": result.threshold_secs,
        "pass": result.pass,
    })
}

fn benchmark_scope_json() -> serde_json::Value {
    json!({
        "kind": BENCHMARK_SCOPE_KIND,
        "description": "Pipeline-level performance regression benchmark using mock CodeQL adapters.",
        "validates_real_codeql_requirements": false,
        "real_codeql_requirements": {
            "full": "REQ-NF-001 <= 60s",
            "diff": "REQ-NF-002 <= 10s",
            "required_profile": "bench-linux-x64",
        },
        "threshold_calibration": "Mock thresholds are calibrated to catch regressions in metric calculation, scoring, report assembly, impact analysis, and related pipeline computation.",
    })
}

fn p95(run_times_secs: &[f64]) -> f64 {
    assert!(
        !run_times_secs.is_empty(),
        "benchmark must record at least one run"
    );

    let mut sorted = run_times_secs.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = sorted
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[index]
}

fn parse_usize_env(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => {
            let parsed = value
                .parse::<usize>()
                .unwrap_or_else(|error| panic!("{name} must be a positive integer: {error}"));
            assert!(parsed > 0, "{name} must be greater than zero");
            parsed
        }
        Err(_) => default,
    }
}

fn parse_f64_env(name: &str, default: f64) -> f64 {
    match std::env::var(name) {
        Ok(value) => {
            let parsed = value
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("{name} must be a number: {error}"));
            assert!(parsed >= 0.0, "{name} must be non-negative");
            parsed
        }
        Err(_) => default,
    }
}

fn load_fixture(language: Language) -> String {
    let name = match language {
        Language::Python => "python.json",
        Language::TypeScript => "typescript.json",
        Language::Rust => "rust.json",
        Language::Go => "go.json",
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codeql")
        .join(name);
    fs::read_to_string(path).expect("fixture should load successfully")
}

fn rust_corpus_files() -> Vec<(String, String)> {
    synthetic_language_files(
        "src/lib.rs",
        |index| format!("src/generated/module_{index:02}.rs"),
        rust_file,
    )
}

fn typescript_corpus_files() -> Vec<(String, String)> {
    synthetic_language_files(
        "web/app.ts",
        |index| format!("web/generated/component_{index:02}.ts"),
        typescript_file,
    )
}

fn python_corpus_files() -> Vec<(String, String)> {
    synthetic_language_files(
        "src/app.py",
        |index| format!("pyservices/service_{index:02}.py"),
        python_file,
    )
}

fn go_corpus_files() -> Vec<(String, String)> {
    synthetic_language_files(
        "cmd/app/main.go",
        |index| format!("internal/service_{index:02}/worker.go"),
        go_file,
    )
}

fn synthetic_language_files<F, G>(
    primary_path: &str,
    path_builder: F,
    content_builder: G,
) -> Vec<(String, String)>
where
    F: Fn(usize) -> String,
    G: Fn(usize) -> String,
{
    let mut files = Vec::with_capacity(FILES_PER_LANGUAGE);
    for index in 0..FILES_PER_LANGUAGE {
        let path = if index == 0 {
            primary_path.to_owned()
        } else {
            path_builder(index)
        };
        files.push((path, content_builder(index)));
    }
    files
}

fn rust_file(index: usize) -> String {
    let worker_name = format!("Worker{index:02}");
    let mut lines = vec![
        "use std::collections::BTreeMap;".to_owned(),
        "use std::fmt::Write as _;".to_owned(),
        String::new(),
        format!("pub struct {worker_name} {{"),
        "    pub id: usize,".to_owned(),
        "    pub values: Vec<i64>,".to_owned(),
        "}".to_owned(),
        String::new(),
        format!("impl {worker_name} {{"),
        "    pub fn new(id: usize) -> Self {".to_owned(),
        "        Self { id, values: Vec::new() }".to_owned(),
        "    }".to_owned(),
        String::new(),
        "    pub fn summarize(&self) -> i64 {".to_owned(),
        "        self.values.iter().copied().sum()".to_owned(),
        "    }".to_owned(),
        "}".to_owned(),
        String::new(),
        format!("pub fn build_lookup_{index:02}() -> BTreeMap<String, i64> {{"),
        "    let mut lookup = BTreeMap::new();".to_owned(),
        "    lookup.insert(\"alpha\".to_owned(), 1);".to_owned(),
        "    lookup.insert(\"beta\".to_owned(), 2);".to_owned(),
        "    lookup".to_owned(),
        "}".to_owned(),
    ];

    for function_index in 0..FUNCTIONS_PER_FILE {
        lines.extend([
            String::new(),
            format!("pub fn process_{index:02}_{function_index:02}(seed: i64) -> String {{"),
            format!("    let mut worker = {worker_name}::new({function_index});"),
            "    worker.values.push(seed);".to_owned(),
            format!("    for step in 0..{} {{", 4 + (function_index % 4)),
            "        worker.values.push(seed + step as i64);".to_owned(),
            "    }".to_owned(),
            "    let mut message = String::new();".to_owned(),
            "    let total = worker.summarize();".to_owned(),
            "    write!(&mut message, \"worker:{} total:{}\", worker.id, total).unwrap();"
                .to_owned(),
            "    message".to_owned(),
            "}".to_owned(),
        ]);
    }

    lines.join("\n") + "\n"
}

fn typescript_file(index: usize) -> String {
    let class_name = format!("Widget{index:02}");
    let payload_name = format!("Payload{index:02}");
    let mut lines = vec![
        "import { join } from \"node:path\";".to_owned(),
        String::new(),
        format!("type {payload_name} = {{"),
        "  id: number;".to_owned(),
        "  label: string;".to_owned(),
        "};".to_owned(),
        String::new(),
        format!("export class {class_name} {{"),
        "  constructor(public readonly id: number) {}".to_owned(),
        String::new(),
        "  describe(values: number[]): string {".to_owned(),
        "    return `${this.id}:${values.reduce((sum, value) => sum + value, 0)}`;".to_owned(),
        "  }".to_owned(),
        "}".to_owned(),
        String::new(),
        format!("export function buildRoute{index:02}(name: string): string {{"),
        format!("  return join(\"modules\", \"{index:02}\", name).replace(/\\\\/g, \"/\");"),
        "}".to_owned(),
    ];

    for function_index in 0..FUNCTIONS_PER_FILE {
        lines.extend([
            String::new(),
            format!(
                "export function render_{index:02}_{function_index:02}(seed: number): {payload_name} {{"
            ),
            format!("  const widget = new {class_name}({function_index});"),
            "  const values: number[] = [seed];".to_owned(),
            format!("  for (let step = 0; step < {}; step += 1) {{", 5 + (function_index % 3)),
            "    values.push(seed + step);".to_owned(),
            "  }".to_owned(),
            "  return {".to_owned(),
            "    id: widget.id,".to_owned(),
            "    label: widget.describe(values),".to_owned(),
            "  };".to_owned(),
            "}".to_owned(),
        ]);
    }

    lines.join("\n") + "\n"
}

fn python_file(index: usize) -> String {
    let class_name = format!("Worker{index:02}");
    let mut lines = vec![
        "from dataclasses import dataclass, field".to_owned(),
        "from typing import List".to_owned(),
        String::new(),
        "@dataclass".to_owned(),
        format!("class {class_name}:"),
        "    index: int".to_owned(),
        "    values: List[int] = field(default_factory=list)".to_owned(),
        String::new(),
        "    def total(self) -> int:".to_owned(),
        "        return sum(self.values)".to_owned(),
        String::new(),
        "    def extend(self, seed: int, width: int) -> None:".to_owned(),
        "        for step in range(width):".to_owned(),
        "            self.values.append(seed + step)".to_owned(),
        String::new(),
        format!("def build_label_{index:02}(name: str) -> str:"),
        format!("    return f\"service-{index:02}-{{name}}\""),
    ];

    for function_index in 0..FUNCTIONS_PER_FILE {
        lines.extend([
            String::new(),
            format!("def process_{index:02}_{function_index:02}(seed: int) -> int:"),
            format!("    worker = {class_name}(index={function_index}, values=[seed])"),
            format!("    label = build_label_{index:02}(\"task\")"),
            "    adjustments: List[int] = []".to_owned(),
            format!("    for step in range({}):", 5 + (function_index % 4)),
            "        value = seed + step".to_owned(),
            "        worker.values.append(value)".to_owned(),
            "        adjustments.append(len(label) + value)".to_owned(),
            "    worker.values.extend(adjustments)".to_owned(),
            "    worker.extend(seed, 2)".to_owned(),
            "    return worker.total() + len(label)".to_owned(),
        ]);
    }

    lines.join("\n") + "\n"
}

fn go_file(index: usize) -> String {
    let package_name = format!("service{:02}", index);
    let worker_name = format!("Worker{index:02}");
    let mut lines = vec![
        format!("package {package_name}"),
        String::new(),
        "import \"strings\"".to_owned(),
        String::new(),
        format!("type {worker_name} struct {{"),
        "    ID int".to_owned(),
        "    Values []int".to_owned(),
        "}".to_owned(),
        String::new(),
        format!("func New{worker_name}(id int) {worker_name} {{"),
        format!("    return {worker_name}{{ID: id, Values: []int{{}}}}"),
        "}".to_owned(),
        String::new(),
        format!("func (worker {worker_name}) Total() int {{"),
        "    total := 0".to_owned(),
        "    for _, value := range worker.Values {".to_owned(),
        "        total += value".to_owned(),
        "    }".to_owned(),
        "    return total".to_owned(),
        "}".to_owned(),
        String::new(),
        format!("func BuildLabel{index:02}(parts []string) string {{"),
        "    return strings.Join(parts, \"-\")".to_owned(),
        "}".to_owned(),
    ];

    for function_index in 0..FUNCTIONS_PER_FILE {
        lines.extend([
            String::new(),
            format!("func Process{index:02}{function_index:02}(seed int) int {{"),
            format!("    worker := New{worker_name}({function_index})"),
            "    worker.Values = append(worker.Values, seed)".to_owned(),
            format!(
                "    for step := 0; step < {}; step++ {{",
                4 + (function_index % 5)
            ),
            "        worker.Values = append(worker.Values, seed+step)".to_owned(),
            "    }".to_owned(),
            "    _ = BuildLabel00([]string{\"go\", \"bench\"})".to_owned(),
            "    return worker.Total()".to_owned(),
            "}".to_owned(),
        ]);
    }

    lines.join("\n") + "\n"
}
