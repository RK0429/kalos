use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

use kalos::adapters::tool_cache::codeql_bundle_manifest;

#[test]
fn kalos_init_creates_default_config_file() {
    let temp = TempDir::new().unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let config = fs::read_to_string(temp.path().join(".kalos.toml")).unwrap();
    assert!(config.contains("[score.weights]"));
    assert!(config.contains("# [rules.KAL-F001]"));
    assert!(config.contains("# [rules.KAL-PAT003]"));
}

#[test]
fn kalos_init_reports_existing_config() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"));

    assert_eq!(
        fs::read_to_string(config_path).unwrap(),
        "existing = true\n"
    );
}

#[test]
fn kalos_check_succeeds_with_non_empty_output_in_temp_workspace() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn kalos_check_json_output_has_required_top_level_fields() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();

    for field in [
        "schema_version",
        "analysis_targets",
        "scores",
        "metrics",
        "diagnostics",
        "summary",
        "tool_version",
    ] {
        assert!(parsed.get(field).is_some(), "missing field `{field}`");
    }
}

#[test]
fn kalos_check_llm_missing_api_key_fails_preflight() {
    let temp = seeded_workspace();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_LLM_API_KEY")
        .env_remove("KALOS_LLM_PROVIDER")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("KALOS_LLM_API_KEY"));
}

#[test]
fn kalos_check_llm_unsupported_provider_fails_preflight() {
    let temp = seeded_workspace();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_LLM_API_KEY", "secret")
        .env("KALOS_LLM_PROVIDER", "anthropic")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unsupported KALOS_LLM_PROVIDER `anthropic`",
        ));
}

#[test]
fn kalos_check_llm_invalid_endpoint_fails_preflight() {
    let temp = seeded_workspace();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_LLM_API_KEY", "secret")
        .env_remove("KALOS_LLM_PROVIDER")
        .env("KALOS_LLM_ENDPOINT_URL", "not a url")
        .args(["check", "--llm"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "KALOS_LLM_ENDPOINT_URL is not a valid URL",
        ));
}

#[test]
fn kalos_check_diff_falls_back_to_full_analysis_when_baseline_is_missing() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("falling back to full analysis"));

    let baselines_dir = cache_dir.join("baselines");
    assert!(baselines_dir.is_dir());
    assert_eq!(
        fs::read_dir(&baselines_dir).unwrap().count(),
        1,
        "baseline cache should contain exactly one entry after fallback",
    );
}

#[test]
fn kalos_check_diff_uses_cached_baseline_on_subsequent_run() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1"])
        .assert()
        .success();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("falling back to full analysis").not());
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(parsed["diagnostics_scope"], "affected_only");
    assert_eq!(parsed["summary_scope"], "whole_project");
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

fn seeded_git_workspace() -> TempDir {
    let temp = seeded_workspace();
    run_git(temp.path(), &["init"]);
    run_git(temp.path(), &["config", "user.email", "kalos@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Kalos"]);
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-m", "initial"]);

    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn placeholder() -> i32 {\n    let branch_alpha = 1;\n    let branch_beta = 2;\n    let branch_gamma = 3;\n    if branch_alpha > 0 { branch_beta } else { branch_gamma }\n}\n",
    )
    .unwrap();
    run_git(temp.path(), &["add", "src/lib.rs"]);
    run_git(temp.path(), &["commit", "-m", "change"]);

    temp
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
        "#!/bin/sh\nif [ \"$1\" = \"database\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"query\" ] && [ \"$2\" = \"run\" ]; then\n  cat <<'EOF'\n{fixture}\nEOF\n  exit 0\nfi\necho \"unexpected invocation: $@\" >&2\nexit 1\n"
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
        "use std::env;\nuse std::io::Write;\n\nfn main() {{\n    let args = env::args().skip(1).collect::<Vec<_>>();\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"database\" && action == \"create\") {{\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"query\" && action == \"run\") {{\n        print!({fixture:?});\n        return;\n    }}\n    let _ = writeln!(std::io::stderr(), \"unexpected invocation: {{}}\", args.join(\" \"));\n    std::process::exit(1);\n}}\n"
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

fn run_git(workspace_root: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(workspace_root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} should succeed");
}
