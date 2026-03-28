use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

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

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn kalos_check_json_output_has_required_top_level_fields() {
    let temp = seeded_workspace();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
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
fn kalos_check_diff_reports_not_implemented() {
    let temp = seeded_workspace();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--diff", "origin/main"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not implemented"));
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
