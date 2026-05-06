use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin;
use kalos::adapters::tool_cache::codeql_bundle_manifest;
use tempfile::TempDir;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn action_manifest_path() -> PathBuf {
    repo_root().join("action.yml")
}

fn wrapper_script_path() -> PathBuf {
    repo_root().join("scripts").join("github-action-wrapper.sh")
}

fn code_scanning_workflow_path() -> PathBuf {
    repo_root()
        .join(".github")
        .join("workflows")
        .join("code-scanning.yml")
}

fn parse_outputs(stdout: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8(stdout.to_vec())
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn official_action_wrapper_declares_cache_and_check_steps() {
    let manifest = fs::read_to_string(action_manifest_path()).unwrap();

    for needle in [
        "using: composite",
        "actions/cache/restore@v4",
        "actions/cache/save@v4",
        "cargo install --force --locked --path \"$GITHUB_ACTION_PATH\" --root \"$KALOS_INSTALL_ROOT\"",
        "github-action-wrapper.sh\" prewarm",
        "github-action-wrapper.sh\" run-check",
        "github/codeql-action/upload-sarif@v4",
        "KALOS_ACTION_SARIF_FILE",
    ] {
        assert!(
            manifest.contains(needle),
            "action.yml should contain `{needle}`"
        );
    }
}

#[test]
fn resolve_context_outputs_effective_cache_paths_and_keys() {
    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("resolve-context")
        .env("GITHUB_ACTION_PATH", repo_root())
        .env("GITHUB_REF_NAME", "feature/action-wrapper")
        .env("GITHUB_REPOSITORY_ID", "123456")
        .env("GITHUB_SHA", "0123456789abcdef0123456789abcdef01234567")
        .env("GITHUB_WORKSPACE", "/workspace/repo")
        .env("INPUT_BASELINE_CACHE_SCOPE", "pull-request")
        .env("INPUT_CACHE_DIR", "/tmp/custom-kalos-cache")
        .env("INPUT_SARIF_FILE", ".github/results/kalos.sarif")
        .env("INPUT_UPLOAD_SARIF", "true")
        .env("RUNNER_ARCH", "X64")
        .env("RUNNER_OS", "Linux")
        .env("RUNNER_TEMP", "/tmp/runner-temp")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let outputs = parse_outputs(&output.stdout);

    assert_eq!(
        outputs.get("cache_dir").map(String::as_str),
        Some("/tmp/custom-kalos-cache")
    );
    assert_eq!(
        outputs.get("install_root").map(String::as_str),
        Some("/tmp/runner-temp/kalos-tool")
    );
    assert_eq!(
        outputs.get("kalos_bin").map(String::as_str),
        Some("/tmp/runner-temp/kalos-tool/bin/kalos")
    );

    let bundle_key = outputs.get("bundle_cache_key").unwrap();
    assert!(bundle_key.starts_with("kalos-bundle-Linux-X64-"));

    assert_eq!(
        outputs.get("baseline_restore_prefix").map(String::as_str),
        Some("kalos-baseline-Linux-X64-123456-pull-request-feature-action-wrapper-")
    );
    assert_eq!(
        outputs.get("baseline_cache_key").map(String::as_str),
        Some(
            "kalos-baseline-Linux-X64-123456-pull-request-feature-action-wrapper-0123456789abcdef0123456789abcdef01234567"
        )
    );
    assert_eq!(
        outputs.get("sarif_file").map(String::as_str),
        Some(".github/results/kalos.sarif")
    );
    assert_eq!(
        outputs.get("sarif_file_abs").map(String::as_str),
        Some("/workspace/repo/.github/results/kalos.sarif")
    );
}

#[test]
fn run_check_forwards_newline_delimited_arguments_without_shell_splitting() {
    let temp = TempDir::new().unwrap();
    let trace_dir = temp.path().join("trace");
    fs::create_dir_all(&trace_dir).unwrap();
    let working_dir = temp.path().join("workspace");
    fs::create_dir_all(&working_dir).unwrap();

    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(
        &fake_kalos,
        &trace_dir,
        r#"printf '%s\n' "$PWD" > "$TRACE_DIR/pwd.txt"
printf '%s\n' "$KALOS_CACHE_DIR" > "$TRACE_DIR/cache-dir.txt"
printf '%s\n' "$@" > "$TRACE_DIR/args.txt"
"#,
    );

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("run-check")
        .current_dir(&working_dir)
        .env(
            "KALOS_ACTION_ARGS",
            "--diff\norigin/main\npath with spaces\n--exclude\nvendor/**",
        )
        .env("KALOS_BIN", &fake_kalos)
        .env("KALOS_CACHE_DIR", "/tmp/kalos-cache")
        .env("TRACE_DIR", &trace_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");

    let forwarded_args = fs::read_to_string(trace_dir.join("args.txt")).unwrap();
    assert_eq!(
        forwarded_args.lines().collect::<Vec<_>>(),
        vec![
            "check",
            "--diff",
            "origin/main",
            "path with spaces",
            "--exclude",
            "vendor/**",
        ]
    );
    assert_eq!(
        fs::canonicalize(
            fs::read_to_string(trace_dir.join("pwd.txt"))
                .unwrap()
                .trim()
        )
        .unwrap(),
        fs::canonicalize(&working_dir).unwrap()
    );
    assert_eq!(
        fs::read_to_string(trace_dir.join("cache-dir.txt"))
            .unwrap()
            .trim(),
        "/tmp/kalos-cache"
    );
}

#[test]
fn run_check_allows_empty_argument_list() {
    let temp = TempDir::new().unwrap();
    let trace_dir = temp.path().join("trace");
    fs::create_dir_all(&trace_dir).unwrap();

    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(
        &fake_kalos,
        &trace_dir,
        r#"printf '%s\n' "$@" > "$TRACE_DIR/args.txt"
"#,
    );

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("run-check")
        .env("KALOS_ACTION_ARGS", "")
        .env("KALOS_BIN", &fake_kalos)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let forwarded_args = fs::read_to_string(trace_dir.join("args.txt")).unwrap();
    assert_eq!(forwarded_args.lines().collect::<Vec<_>>(), vec!["check"]);
}

#[test]
fn run_check_captures_sarif_output_for_code_scanning_upload() {
    let temp = TempDir::new().unwrap();
    let trace_dir = temp.path().join("trace");
    fs::create_dir_all(&trace_dir).unwrap();
    let sarif_path = temp.path().join("results").join("kalos.sarif");

    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(
        &fake_kalos,
        &trace_dir,
        r#"printf '%s\n' "$@" > "$TRACE_DIR/args.txt"
output_path=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output_path="$2"
      shift 2
      ;;
    --output=*)
      output_path="${1#--output=}"
      shift
      ;;
    *)
      shift
      ;;
  esac
done
mkdir -p "$(dirname "$output_path")"
cat <<'EOF' > "$output_path"
{"runs":[{"tool":{"driver":{"name":"kalos"}},"results":[]}]}
EOF
"#,
    );

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("run-check")
        .env("KALOS_ACTION_ARGS", "--level\nproject")
        .env("KALOS_ACTION_SARIF_FILE", &sarif_path)
        .env("KALOS_BIN", &fake_kalos)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let forwarded_args = fs::read_to_string(trace_dir.join("args.txt")).unwrap();
    assert_eq!(
        forwarded_args.lines().collect::<Vec<_>>(),
        vec![
            "check",
            "--format",
            "sarif",
            "--output",
            sarif_path.to_str().unwrap(),
            "--level",
            "project",
        ]
    );
    assert_eq!(
        fs::read_to_string(&sarif_path).unwrap(),
        "{\"runs\":[{\"tool\":{\"driver\":{\"name\":\"kalos\"}},\"results\":[]}]}\n"
    );
}

#[test]
fn run_check_rejects_explicit_format_when_code_scanning_capture_is_enabled() {
    let temp = TempDir::new().unwrap();
    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(&fake_kalos, temp.path(), "exit 0\n");

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("run-check")
        .env("KALOS_ACTION_ARGS", "--format\njson")
        .env(
            "KALOS_ACTION_SARIF_FILE",
            temp.path().join("results").join("kalos.sarif"),
        )
        .env("KALOS_BIN", &fake_kalos)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("omit --format/--output from args"),
        "{stderr}"
    );
}

#[test]
fn run_check_rejects_explicit_output_when_code_scanning_capture_is_enabled() {
    let temp = TempDir::new().unwrap();
    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(&fake_kalos, temp.path(), "exit 0\n");

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("run-check")
        .env("KALOS_ACTION_ARGS", "--output\ncustom.sarif")
        .env(
            "KALOS_ACTION_SARIF_FILE",
            temp.path().join("results").join("kalos.sarif"),
        )
        .env("KALOS_BIN", &fake_kalos)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("omit --format/--output from args"),
        "{stderr}"
    );
}

#[test]
fn prewarm_runs_check_against_temporary_workspace_via_cli_path() {
    let temp = TempDir::new().unwrap();
    let trace_dir = temp.path().join("trace");
    fs::create_dir_all(&trace_dir).unwrap();

    let fake_kalos = temp.path().join("kalos");
    write_fake_kalos_binary(
        &fake_kalos,
        &trace_dir,
        r#"printf '%s\n' "$PWD" > "$TRACE_DIR/pwd.txt"
printf '%s\n' "$KALOS_CACHE_DIR" > "$TRACE_DIR/cache-dir.txt"
printf '%s\n' "$@" > "$TRACE_DIR/args.txt"
if [ ! -f "$PWD/src/lib.rs" ]; then
  echo "missing prewarm fixture" >&2
  exit 9
fi
if [ ! -f "$PWD/Cargo.toml" ]; then
  echo "missing cargo fixture" >&2
  exit 10
fi
"#,
    );

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("prewarm")
        .env("KALOS_BIN", &fake_kalos)
        .env("KALOS_CACHE_DIR", "/tmp/kalos-cache")
        .env("RUNNER_TEMP", temp.path())
        .env("TRACE_DIR", &trace_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");

    let forwarded_args = fs::read_to_string(trace_dir.join("args.txt")).unwrap();
    assert_eq!(
        forwarded_args.lines().collect::<Vec<_>>(),
        vec!["check", "--level", "project", "--format", "json"]
    );
    assert_eq!(
        fs::read_to_string(trace_dir.join("cache-dir.txt"))
            .unwrap()
            .trim(),
        "/tmp/kalos-cache"
    );
    assert!(
        fs::read_to_string(trace_dir.join("pwd.txt"))
            .unwrap()
            .trim_end()
            .contains("kalos-prewarm."),
        "prewarm should run inside a temporary workspace"
    );
}

#[test]
fn prewarm_succeeds_with_real_kalos_binary_and_seeded_bundle_cache() {
    let temp = TempDir::new().unwrap();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let runner_temp = temp.path().join("runner-temp");
    fs::create_dir_all(&runner_temp).unwrap();

    let output = StdCommand::new("bash")
        .arg(wrapper_script_path())
        .arg("prewarm")
        .env("KALOS_BIN", cargo_bin("kalos"))
        .env("KALOS_CACHE_DIR", &cache_dir)
        .env("RUNNER_TEMP", &runner_temp)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
}

#[test]
fn code_scanning_workflow_uses_official_action_upload_path() {
    let workflow = fs::read_to_string(code_scanning_workflow_path()).unwrap();

    for needle in [
        "name: Code Scanning",
        "security-events: write",
        "uses: ./",
        "upload-sarif: true",
    ] {
        assert!(
            workflow.contains(needle),
            "code-scanning.yml should contain `{needle}`"
        );
    }
}

fn write_fake_kalos_binary(path: &Path, trace_dir: &Path, body: &str) {
    let script = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nTRACE_DIR=\"{}\"\n{}\n",
        trace_dir.display(),
        body
    );
    fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn seed_fake_codeql_bundle(workspace_root: &Path) -> PathBuf {
    let manifest = codeql_bundle_manifest().unwrap();
    let cache_dir = workspace_root.join(".kalos-test-cache");
    let bundle_dir = cache_dir.join("codeql").join(&manifest.version);
    let queries_dir = bundle_dir.join("queries");
    fs::create_dir_all(&queries_dir).unwrap();
    fs::write(bundle_dir.join("bundle.marker"), manifest.sha256.as_bytes()).unwrap();
    fs::write(queries_dir.join("extract-rust.ql"), "// fixture query\n").unwrap();
    write_fake_codeql_executable(
        &codeql_executable_path(&bundle_dir),
        &load_fixture("rust.json"),
    );
    cache_dir
}

fn codeql_executable_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join(format!("codeql{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(unix)]
fn write_fake_codeql_executable(path: &Path, fixture: &str) {
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"resolve\" ] && [ \"$2\" = \"languages\" ]; then\n  cat <<'EOF'\n{{\"go\":[],\"javascript\":[],\"python\":[],\"rust\":[]}}\nEOF\n  exit 0\nfi\nif [ \"$1\" = \"database\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"query\" ] && [ \"$2\" = \"run\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"bqrs\" ] && [ \"$2\" = \"decode\" ]; then\n  cat <<'EOF'\n{fixture}\nEOF\n  exit 0\nfi\necho \"unexpected invocation: $@\" >&2\nexit 1\n"
    );
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn write_fake_codeql_executable(path: &Path, fixture: &str) {
    let source_path = path.with_file_name("codeql_fixture.rs");
    let source = format!(
        "use std::env;\nuse std::io::Write;\n\nfn main() {{\n    let args = env::args().skip(1).collect::<Vec<_>>();\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"resolve\" && action == \"languages\") {{\n        print!(\"{{\\\"go\\\":[],\\\"javascript\\\":[],\\\"python\\\":[],\\\"rust\\\":[]}}\");\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"database\" && action == \"create\") {{\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"query\" && action == \"run\") {{\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"bqrs\" && action == \"decode\") {{\n        print!({fixture:?});\n        return;\n    }}\n    let _ = writeln!(std::io::stderr(), \"unexpected invocation: {{}}\", args.join(\" \"));\n    std::process::exit(1);\n}}\n"
    );
    fs::write(&source_path, source).unwrap();
    let status = StdCommand::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--edition=2024")
        .arg("--crate-name")
        .arg("kalos_fake_codeql")
        .arg(&source_path)
        .arg("-o")
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "fake CodeQL fixture compilation should succeed"
    );
    fs::remove_file(source_path).unwrap();
}

fn load_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codeql")
        .join(name);
    fs::read_to_string(path).unwrap()
}
