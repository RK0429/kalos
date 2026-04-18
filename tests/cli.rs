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
use kalos::domains::diagnostics::builtin_metric_rules;

#[test]
fn kalos_init_creates_default_config_file() {
    let temp = TempDir::new().unwrap();
    let metric_description = builtin_metric_rules()
        .into_iter()
        .find(|rule| rule.id.as_str() == "KAL-F001")
        .map(|rule| rule.description)
        .unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let config = fs::read_to_string(temp.path().join(".kalos.toml")).unwrap();
    assert!(config.contains("[score.weights]"));
    assert!(config.contains(&format!("# {metric_description}")));
    assert!(config.contains("# [rules.KAL-F001]"));
    assert!(config.contains("# [rules.KAL-PAT003]"));
}

#[test]
fn kalos_init_config_excludes_internal_milestone_terms() {
    let temp = TempDir::new().unwrap();
    let f002_description = builtin_metric_rules()
        .into_iter()
        .find(|rule| rule.id.as_str() == "KAL-F002")
        .map(|rule| rule.description)
        .unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success();

    let config = fs::read_to_string(temp.path().join(".kalos.toml")).unwrap();
    assert!(config.contains(&format!("# {f002_description}")));
    assert!(
        !config.contains("Wave 2"),
        ".kalos.toml must not contain internal milestone term 'Wave 2'"
    );
    assert!(
        !config.contains("Wave 3"),
        ".kalos.toml must not contain internal milestone term 'Wave 3'"
    );
}

#[test]
fn kalos_init_preserves_existing_config_when_declined() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .write_stdin(b"n\n")
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
fn kalos_init_overwrites_existing_config_when_confirmed() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .write_stdin(b"y\n")
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("[score.weights]"));
    assert!(!config.contains("existing = true"));
}

#[test]
fn kalos_init_overwrites_existing_config_when_confirmed_with_uppercase_y() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .write_stdin(b"Y\n")
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("[score.weights]"));
    assert!(!config.contains("existing = true"));
}

#[test]
fn kalos_init_creates_gitignore_with_kalos_entry() {
    let temp = TempDir::new().unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created .gitignore with .kalos/ entry",
        ));

    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
}

#[test]
fn kalos_init_appends_kalos_entry_to_existing_gitignore() {
    let temp = TempDir::new().unwrap();
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("added .kalos/ to .gitignore"));

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        "target/\n\n.kalos/\n"
    );
}

#[test]
fn kalos_init_skips_gitignore_when_kalos_entry_exists() {
    let temp = TempDir::new().unwrap();
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n.kalos/\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("added .kalos/ to .gitignore").not())
        .stdout(predicate::str::contains("created .gitignore with .kalos/ entry").not());

    let gitignore = fs::read_to_string(gitignore_path).unwrap();
    assert_eq!(gitignore, "target/\n.kalos/\n");
    assert_eq!(gitignore.matches(".kalos/").count(), 1);
}

#[test]
fn kalos_check_does_not_modify_gitignore_by_default() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore"))
        .stderr(predicate::str::contains("--update-gitignore"))
        .stderr(predicate::str::contains("notice: created .gitignore").not())
        .stderr(predicate::str::contains("notice: added .kalos/ to .gitignore").not());

    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_does_not_warn_when_gitignore_already_contains_kalos_entry() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n.kalos/\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore").not());

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        "target/\n.kalos/\n"
    );
}

#[test]
fn kalos_check_warns_when_gitignore_exists_without_kalos_entry() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore"))
        .stderr(predicate::str::contains("--update-gitignore"));

    assert_eq!(fs::read_to_string(gitignore_path).unwrap(), "target/\n");
}

#[test]
fn kalos_check_creates_gitignore_when_update_gitignore_flag_set() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--update-gitignore"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "notice: created .gitignore with .kalos/ entry",
        ))
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore").not());

    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
}

#[test]
fn kalos_check_appends_to_existing_gitignore_when_update_gitignore_flag_set() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--update-gitignore"])
        .assert()
        .success()
        .stderr(predicate::str::contains("notice: added .kalos/ to .gitignore"));

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        "target/\n\n.kalos/\n"
    );
}

#[test]
fn kalos_check_help_describes_update_gitignore_flag() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--update-gitignore"))
        .stdout(predicate::str::contains("default: warn only"));
}

#[test]
fn kalos_init_preserves_existing_config_on_empty_input() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .write_stdin(b"\n")
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("aborted"));

    assert_eq!(
        fs::read_to_string(config_path).unwrap(),
        "existing = true\n"
    );
}

#[test]
fn kalos_root_help_uses_uppercase_cpg_in_about_text() {
    Command::cargo_bin("kalos")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CPG-based code quality analysis tool",
        ))
        .stdout(predicate::str::contains("cpg-based code quality analysis tool").not());
}

#[test]
fn kalos_check_help_describes_omitted_severity_behavior() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "minimum severity threshold for diagnostics (omit to show all)",
        ));
}

#[test]
fn kalos_check_help_mentions_apple_silicon() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Apple Silicon"));
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
fn kalos_check_human_output_explains_when_no_supported_files_are_found() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("README.md"), "# placeholder\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyzed 0 files"))
        .stdout(predicate::str::contains(
            "no files with supported extensions (.py, .ts, .tsx, .rs, .go) were found in the analysis targets",
        ));
}

#[test]
fn kalos_check_with_external_target_path_succeeds() {
    let target_workspace = seeded_workspace();
    let external_cwd = TempDir::new().unwrap();
    let cache_dir = seed_fake_codeql_bundle(target_workspace.path());
    let target_workspace_path = fs::canonicalize(target_workspace.path()).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(external_cwd.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg(&target_workspace_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn kalos_check_emits_codeql_phase_progress_on_stderr() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("database create"))
        .stderr(predicate::str::contains("database create done"))
        .stderr(predicate::str::contains("query run"))
        .stderr(predicate::str::contains("query run done"))
        .stderr(predicate::str::contains("bqrs decode"))
        .stderr(predicate::str::contains("bqrs decode done"));
}

#[test]
fn kalos_check_emits_first_run_hint_on_initial_run() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("database create (first run"))
        .stderr(predicate::str::contains("first run"));
}

#[test]
fn kalos_check_emits_elapsed_time_on_database_create() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(
            predicate::str::is_match(r"database create done \(([0-9]+\.[0-9]s|[0-9]+m [0-9]+s)\)")
                .unwrap(),
        );
}

#[test]
fn kalos_check_cached_run_does_not_emit_first_run_hint() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("cached"))
        .stderr(predicate::str::contains("first run").not())
        .stderr(predicate::str::contains("database create done").not())
        .stderr(predicate::str::contains("query run").not())
        .stderr(predicate::str::contains("bqrs decode").not());
}

#[test]
fn kalos_check_json_format_does_not_emit_first_run_hint() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("first run").not());
}

#[test]
fn kalos_check_skips_incidental_language_database_creation() {
    let temp = seeded_mixed_language_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("skipping python"))
        .stderr(predicate::str::contains("analyzing python").not())
        .stderr(predicate::str::contains("analyzing rust"));
}

#[test]
fn kalos_check_analyzes_all_languages_when_min_ratio_zero() {
    let temp = seeded_mixed_language_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--min-language-ratio", "0.0"])
        .assert()
        .success()
        .stderr(predicate::str::contains("analyzing rust"))
        .stderr(predicate::str::contains("analyzing python"));
}

#[test]
fn kalos_check_json_format_does_not_emit_progress_on_stderr() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Apple Silicon").not())
        .stderr(predicate::str::contains("first run").not())
        .stderr(predicate::str::contains("database create").not())
        .stderr(predicate::str::contains("query run").not())
        .stderr(predicate::str::contains("bqrs decode").not());
}

#[test]
fn kalos_check_sarif_format_does_not_emit_progress_on_stderr() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "sarif"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Apple Silicon").not())
        .stderr(predicate::str::contains("first run").not())
        .stderr(predicate::str::contains("database create").not())
        .stderr(predicate::str::contains("query run").not())
        .stderr(predicate::str::contains("bqrs decode").not());
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
        "analysis_warnings",
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
fn kalos_check_output_flag_writes_json_to_file() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("result.json");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--output"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert!(parsed.is_object(), "expected JSON object output");
}

#[test]
fn kalos_check_output_flag_writes_sarif_to_file() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("result.sarif");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "sarif", "-o"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert!(parsed.get("version").is_some(), "missing SARIF version");
    assert!(parsed.get("runs").is_some(), "missing SARIF runs");
}

#[test]
fn kalos_check_output_flag_creates_parent_directories() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp
        .path()
        .join("nested")
        .join("reports")
        .join("result.json");

    assert!(!output_path.parent().unwrap().exists());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--output"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    assert!(output_path.parent().unwrap().is_dir());
    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert!(parsed.is_object(), "expected JSON object output");
}

#[test]
fn kalos_check_non_diff_full_workspace_writes_baseline() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();

    assert_eq!(baseline_entry_count(&cache_dir), 1);
}

#[test]
fn kalos_check_non_diff_strict_exit_code_1_still_writes_baseline() {
    let temp = seeded_git_workspace();
    fs::write(
        temp.path().join(".kalos.toml"),
        "[rules.KAL-PAT003]\nseverity = \"warning\"\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("src/lib.rs"),
        "mod a;\nmod b;\n\npub fn placeholder() -> i32 {\n    a::call_b() + b::call_a()\n}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("src/a.rs"),
        "pub fn call_b() -> i32 {\n    crate::b::call_a()\n}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("src/b.rs"),
        "pub fn call_a() -> i32 {\n    crate::a::call_b()\n}\n",
    )
    .unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-m", "strict warning fixture"]);
    let cache_dir = seed_fake_codeql_bundle_with_fixture(
        temp.path(),
        r#"{
  "modules": [
    { "id": "m1", "name": "crate::a", "file": "src/a.rs", "start_line": 1, "end_line": 3, "language": "rust" },
    { "id": "m2", "name": "crate::b", "file": "src/b.rs", "start_line": 1, "end_line": 3, "language": "rust" }
  ],
  "functions": [
    { "id": "f1", "name": "crate::a::call_b", "file": "src/a.rs", "start_line": 1, "end_line": 3, "language": "rust" },
    { "id": "f2", "name": "crate::b::call_a", "file": "src/b.rs", "start_line": 1, "end_line": 3, "language": "rust" }
  ],
  "calls": [
    { "source": "f1", "target": "f2", "language": "rust" },
    { "source": "f2", "target": "f1", "language": "rust" }
  ],
  "contains": [
    { "source": "m1", "target": "f1", "language": "rust" },
    { "source": "m2", "target": "f2", "language": "rust" }
  ]
}"#,
    );

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--strict"])
        .assert()
        .code(1);

    assert_eq!(baseline_entry_count(&cache_dir), 1);
}

#[test]
fn kalos_check_explicit_targets_skip_baseline_write_back() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "src/lib.rs"])
        .assert()
        .success();

    assert_eq!(baseline_entry_count(&cache_dir), 0);
}

#[test]
fn kalos_check_non_diff_then_diff_reuses_baseline() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();

    assert_eq!(baseline_entry_count(&cache_dir), 1);

    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn placeholder() -> i32 {\n    let branch_alpha = 1;\n    let branch_beta = 2;\n    let branch_gamma = 3;\n    let branch_delta = 4;\n    if branch_alpha > 0 { branch_beta + branch_delta } else { branch_gamma }\n}\n",
    )
    .unwrap();
    run_git(temp.path(), &["add", "src/lib.rs"]);
    run_git(temp.path(), &["commit", "-m", "third"]);

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
    assert_eq!(baseline_entry_count(&cache_dir), 1);
}

#[test]
fn kalos_check_missing_config_json_error_output_is_structured() {
    let temp = TempDir::new().unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--format", "json", "--config", "/nonexistent/path"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(parsed["error"], Value::Bool(true));
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("failed to load config file")
    );
    assert!(
        parsed["cause"]
            .as_str()
            .unwrap()
            .contains("No such file or directory")
    );
}

#[test]
fn kalos_check_missing_config_sarif_error_output_is_structured() {
    let temp = TempDir::new().unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "check",
            "--format",
            "sarif",
            "--config",
            "/nonexistent/path",
        ])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(parsed["error"], Value::Bool(true));
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("failed to load config file")
    );
    assert!(
        parsed["cause"]
            .as_str()
            .unwrap()
            .contains("No such file or directory")
    );
}

#[test]
fn kalos_check_missing_config_human_error_output_remains_plain_text() {
    let temp = TempDir::new().unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--config", "/nonexistent/path"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(serde_json::from_str::<Value>(&stderr).is_err());
    assert!(stderr.contains("failed to load config file"));
    assert!(stderr.contains("No such file or directory"));
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
fn kalos_check_diff_first_run_json_reports_affected_only_scope() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1", "--format", "json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(parsed["diagnostics_scope"], "affected_only");
    assert_eq!(parsed["summary_scope"], "whole_project");
    assert_eq!(baseline_entry_count(&cache_dir), 1);
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

#[test]
fn kalos_check_emits_debug_logs_when_rust_log_is_set() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("RUST_LOG", "kalos=debug")
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("check config loaded"));
}

#[test]
fn kalos_check_does_not_emit_debug_logs_by_default() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("RUST_LOG")
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains("check config loaded").not());
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

fn seeded_mixed_language_workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn placeholder() -> i32 { 1 }\n",
    )
    .unwrap();
    for index in 1..20 {
        fs::write(
            temp.path().join(format!("src/module_{index}.rs")),
            format!("pub fn module_{index}() -> i32 {{ {index} }}\n"),
        )
        .unwrap();
    }
    fs::create_dir_all(temp.path().join("scripts")).unwrap();
    fs::write(
        temp.path().join("scripts/tool.py"),
        "def tool() -> int:\n    return 1\n",
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
    seed_fake_codeql_bundle_with_fixture(workspace_root, &load_fixture("rust.json"))
}

fn seed_fake_codeql_bundle_with_fixture(workspace_root: &Path, fixture: &str) -> PathBuf {
    let manifest = codeql_bundle_manifest().unwrap();
    let cache_dir = workspace_root.join(".kalos-test-cache");
    let bundle_dir = cache_dir.join("codeql").join(&manifest.version);
    let queries_dir = bundle_dir.join("queries");
    fs::create_dir_all(&queries_dir).unwrap();
    fs::write(bundle_dir.join("bundle.marker"), manifest.sha256.as_bytes()).unwrap();
    for language in ["python", "javascript-typescript", "rust", "go"] {
        let language_dir = queries_dir.join(language);
        fs::create_dir_all(&language_dir).unwrap();
        fs::write(
            language_dir.join(format!("extract-{language}.ql")),
            "// fixture query\n",
        )
        .unwrap();
    }
    write_fake_codeql_executable(&codeql_executable_path(&bundle_dir), fixture);
    cache_dir
}

fn baseline_entry_count(cache_dir: &Path) -> usize {
    let baselines_dir = cache_dir.join("baselines");
    match fs::read_dir(baselines_dir) {
        Ok(entries) => entries.count(),
        Err(_) => 0,
    }
}

fn codeql_executable_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join(format!("codeql{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(unix)]
fn write_fake_codeql_executable(path: &Path, fixture: &str) {
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"database\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"query\" ] && [ \"$2\" = \"run\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"bqrs\" ] && [ \"$2\" = \"decode\" ]; then\n  cat <<'EOF'\n{fixture}\nEOF\n  exit 0\nfi\necho \"unexpected invocation: $@\" >&2\nexit 1\n"
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
        "use std::env;\nuse std::io::Write;\n\nfn main() {{\n    let args = env::args().skip(1).collect::<Vec<_>>();\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"database\" && action == \"create\") {{\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"query\" && action == \"run\") {{\n        return;\n    }}\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"bqrs\" && action == \"decode\") {{\n        print!({fixture:?});\n        return;\n    }}\n    let _ = writeln!(std::io::stderr(), \"unexpected invocation: {{}}\", args.join(\" \"));\n    std::process::exit(1);\n}}\n"
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
