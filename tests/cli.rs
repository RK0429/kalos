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
    assert!(
        config.contains("# CI baseline gates should normally use `kalos check --level project`.")
    );
    assert!(config.contains("# Module/all runs are domain-owner architecture triage"));
    assert!(config.contains("For KAL-M003 bursts, inspect dependency direction"));
    assert!(config.contains("raise `threshold`, lower `severity`, or set `enabled = false`"));
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
fn kalos_init_refuses_overwrite_on_non_tty_stdin_without_force() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .write_stdin(b"n\n")
        .arg("init")
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Overwrite?").not())
        .stderr(predicate::str::contains(
            ".kalos.toml already exists; pass --force to overwrite (refusing to prompt on non-interactive stdin)",
        ));

    assert_eq!(
        fs::read_to_string(config_path).unwrap(),
        "existing = true\n"
    );
}

#[test]
fn kalos_init_overwrites_existing_config_when_force_flag_is_set() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "-f"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    let config = fs::read_to_string(config_path).unwrap();
    assert!(config.contains("[score.weights]"));
    assert!(!config.contains("existing = true"));
}

#[test]
fn kalos_init_overwrites_existing_config_when_yes_alias_is_used() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".kalos.toml");
    fs::write(&config_path, "existing = true\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["init", "--yes"])
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
        "target/\n.kalos/\n"
    );
}

#[test]
fn kalos_init_appends_kalos_entry_after_existing_gitignore_without_trailing_newline() {
    let temp = TempDir::new().unwrap();
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("added .kalos/ to .gitignore"));

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        "target/\n.kalos/\n"
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
fn kalos_check_does_not_modify_parent_gitignore_when_run_from_nested_subdirectory() {
    let parent_workspace = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(parent_workspace.path());
    let parent_gitignore = parent_workspace.path().join(".gitignore");
    fs::write(&parent_gitignore, "target/\n").unwrap();
    let nested_dir = parent_workspace.path().join("tmp").join("sandbox");
    fs::create_dir_all(&nested_dir).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(&nested_dir)
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore"))
        .stderr(predicate::str::contains("--update-gitignore"))
        .stderr(predicate::str::contains("notice: added .kalos/ to .gitignore").not())
        .stderr(predicate::str::contains("notice: created .gitignore").not());

    assert_eq!(fs::read_to_string(&parent_gitignore).unwrap(), "target/\n");
    assert!(!nested_dir.join(".gitignore").exists());
}

#[test]
fn kalos_check_update_gitignore_from_nested_subdirectory_does_not_modify_parent_gitignore() {
    let parent_workspace = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(parent_workspace.path());
    let parent_gitignore = parent_workspace.path().join(".gitignore");
    fs::write(&parent_gitignore, "target/\n").unwrap();
    let nested_dir = parent_workspace.path().join("tmp").join("sandbox");
    fs::create_dir_all(&nested_dir).unwrap();
    fs::write(nested_dir.join("main.rs"), "fn main() {}\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(&nested_dir)
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--update-gitignore"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "notice: created .gitignore with .kalos/ entry",
        ));

    assert_eq!(fs::read_to_string(&parent_gitignore).unwrap(), "target/\n");
    assert_eq!(
        fs::read_to_string(nested_dir.join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
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
fn kalos_check_preserves_tracked_gitignore_by_default() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let gitignore_path = temp.path().join(".gitignore");
    fs::write(&gitignore_path, "target/\n").unwrap();
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["commit", "-m", "track gitignore"]);

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore"))
        .stderr(predicate::str::contains("notice: added .kalos/ to .gitignore").not());

    assert_eq!(fs::read_to_string(&gitignore_path).unwrap(), "target/\n");
    let output = StdCommand::new("git")
        .args(["status", "--short", "--", ".gitignore"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git status --short -- .gitignore should succeed"
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
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
        .stderr(predicate::str::contains(
            "notice: added .kalos/ to .gitignore",
        ));

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        "target/\n.kalos/\n"
    );
}

#[test]
fn kalos_check_json_update_gitignore_keeps_stderr_free_of_human_notice() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--update-gitignore"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str::<Value>(&stdout).unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
}

#[test]
fn kalos_check_sarif_update_gitignore_keeps_stderr_free_of_human_notice() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "sarif", "--update-gitignore"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str::<Value>(&stdout).unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
}

#[test]
fn kalos_check_update_gitignore_missing_diff_ref_does_not_create_gitignore() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--update-gitignore", "--diff", "missing-ref"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "git ref `missing-ref` was not found",
        ))
        .stderr(predicate::str::contains(".gitignore").not());

    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_update_gitignore_output_directory_failure_does_not_create_gitignore() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_dir = temp.path().join("report-dir");
    fs::create_dir(&output_dir).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg("--update-gitignore")
        .arg("--output")
        .arg(&output_dir)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("is a directory; pass a file path"))
        .stderr(predicate::str::contains(".gitignore").not());

    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_update_gitignore_no_supported_files_does_not_create_gitignore() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("README.md"), "# placeholder\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--update-gitignore"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyzed 0 files"))
        .stderr(predicate::str::contains(".gitignore").not());

    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_update_gitignore_no_supported_files_preserves_existing_gitignore() {
    let temp = TempDir::new().unwrap();
    let gitignore_path = temp.path().join(".gitignore");
    let original_contents = "target/\n";
    fs::write(temp.path().join("README.md"), "# placeholder\n").unwrap();
    fs::write(&gitignore_path, original_contents).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--update-gitignore"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyzed 0 files"))
        .stderr(predicate::str::contains(".gitignore").not());

    assert_eq!(
        fs::read_to_string(gitignore_path).unwrap(),
        original_contents
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
fn kalos_root_help_mentions_init_filesystem_side_effects() {
    Command::cargo_bin("kalos")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "init   create .kalos.toml and update .gitignore; see `kalos help init`",
        ));
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
fn kalos_check_help_explains_project_gate_and_module_triage() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The default `project` level is the recommended first-run and CI baseline gate",
        ))
        .stdout(predicate::str::contains(
            "`--level function`, `--level module`, and `--level all` opt in",
        ))
        .stdout(predicate::str::contains("KAL-F001/KAL-F003"))
        .stdout(predicate::str::contains(
            "module diagnostics such as KAL-M001",
        ))
        .stdout(predicate::str::contains("KAL-M002, and KAL-M003"))
        .stdout(predicate::str::contains("tune noisy"))
        .stdout(predicate::str::contains(
            "inspect dependency direction, owner boundaries, and configured thresholds",
        ))
        .stdout(predicate::str::contains(
            "threshold, severity, or enabled overrides",
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
fn kalos_check_help_documents_filesystem_side_effects() {
    let expected = r#"NOTE: Normal `check` execution may write to locations such as:
  - `<repo>/.kalos/codeql/<language>/` stores per-language CodeQL databases unless --cache-dir is passed.
  - `$KALOS_CACHE_DIR/codeql/` or `--cache-dir <path>/codeql/` may store managed CodeQL bundles.
  - `$KALOS_CACHE_DIR/baselines/` or `--cache-dir <path>/baselines/` may store cached baselines for full-workspace runs in Git repositories.
  - `--cache-dir <path>/codeql/databases/<language>/` stores per-language CodeQL databases when --cache-dir is passed.
  - `<repo>/.gitignore` is only created or updated when --update-gitignore is passed."#;

    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(expected));
}

#[test]
fn kalos_check_help_recommends_large_matrix_evaluation_profile() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "For repeated matrix evaluation over large repositories",
        ))
        .stdout(predicate::str::contains(
            "--evaluation-profile recommended --cache-dir <shared-cache-dir>",
        ))
        .stdout(predicate::str::contains(
            "pre-populate or warm the managed CodeQL cache",
        ))
        .stdout(predicate::str::contains(
            "equivalent to `--level project --format json --codeql-total-timeout 1200 --codeql-timeout 240`",
        ))
        .stdout(predicate::str::contains(
            "Avoid per-case cache directories for repeated level/format evaluation",
        ))
        .stdout(predicate::str::contains(
            "Account for cold bundle setup and CodeQL database creation separately from rule runtime",
        ))
        .stdout(predicate::str::contains("--level project --format json"))
        .stdout(predicate::str::contains("apply a named evaluation profile"))
        .stdout(predicate::str::contains("--diff"))
        .stdout(predicate::str::contains("--exclude"));
}

#[test]
fn kalos_check_help_documents_codeql_timeout_option() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--codeql-timeout <seconds>"))
        .stdout(predicate::str::contains(
            "managed CodeQL bundle setup and each CodeQL subprocess phase",
        ))
        .stdout(predicate::str::contains(
            "cold/cache-heavy managed CodeQL bundle",
        ))
        .stdout(predicate::str::contains(
            "0 to disable subprocess phase timeouts; managed bundle setup keeps its default timeout unless --codeql-total-timeout sets a stricter total budget",
        ))
        .stdout(predicate::str::contains("--codeql-total-timeout <seconds>"))
        .stdout(predicate::str::contains(
            "maximum total seconds allowed for CodeQL setup and subprocess phases",
        ))
        .stdout(predicate::str::contains(
            "Default: 1200 seconds, unless --codeql-timeout 0 is passed",
        ))
        .stdout(predicate::str::contains(
            "the total budget is also disabled unless this option is explicitly provided",
        ))
        .stdout(predicate::str::contains(
            "0 to disable the total CodeQL wall-clock budget",
        ));
}

#[test]
fn kalos_check_rejects_invalid_codeql_timeout_value() {
    Command::cargo_bin("kalos")
        .unwrap()
        .args(["check", "--codeql-timeout", "not-a-number"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid CodeQL timeout value"));
}

#[test]
fn kalos_check_accepts_zero_codeql_timeout() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--codeql-timeout", "0"])
        .assert()
        .success();
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

    assert!(temp.path().join(".kalos/codeql/rust.cache_key").exists());
}

#[test]
fn kalos_check_cache_dir_avoids_repo_local_kalos_directory() {
    let temp = seeded_git_workspace();
    let external = TempDir::new().unwrap();
    let cache_dir = seed_fake_codeql_bundle(external.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_CACHE_DIR")
        .args(["check", "--cache-dir"])
        .arg(&cache_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains(".kalos/ is not in .gitignore").not());

    assert!(!temp.path().join(".kalos").exists());
    assert!(cache_dir.join("codeql/databases/rust.cache_key").exists());
    assert_eq!(baseline_entry_count(&cache_dir), 1);
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
fn kalos_check_with_external_config_and_explicit_target_path_succeeds() {
    let target_workspace = seeded_workspace();
    let external_config_dir = TempDir::new().unwrap();
    let external_cwd = TempDir::new().unwrap();
    let config_path = external_config_dir.path().join(".kalos.toml");
    fs::write(
        &config_path,
        "[rules.KAL-F001]\nthreshold = 0.0\nseverity = \"warning\"\n",
    )
    .unwrap();
    let cache_dir = seed_fake_codeql_bundle(target_workspace.path());
    let target_workspace_path = fs::canonicalize(target_workspace.path()).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(external_cwd.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg("--config")
        .arg(&config_path)
        .arg(&target_workspace_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not())
        .stderr(predicate::str::contains("is outside workspace root").not());
}

#[test]
fn kalos_check_external_config_with_current_infrastructure_failure_reports_error_class() {
    let target_workspace = seeded_workspace();
    let external_config_dir = TempDir::new().unwrap();
    let config_path = external_config_dir.path().join(".kalos.toml");
    fs::write(
        &config_path,
        "[rules.KAL-F001]\nthreshold = 0.0\nseverity = \"warning\"\n",
    )
    .unwrap();
    let cache_dir = seed_invalid_managed_bundle(target_workspace.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(target_workspace.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg("--config")
        .arg(&config_path)
        .arg(".")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "error class: codeql_infrastructure",
        ))
        .stderr(predicate::str::contains("outcome: infrastructure_error"))
        .stderr(predicate::str::contains("is outside workspace root").not());
}

#[test]
fn kalos_check_human_codeql_extraction_failure_does_not_report_error_class() {
    let temp = seeded_workspace();
    let cache_dir = seed_failing_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "CodeQL `query run` failed for `rust`",
        ))
        .stderr(predicate::str::contains("outcome: infrastructure_error"))
        .stderr(predicate::str::contains("error class: codeql_extraction").not());
}

#[test]
fn kalos_check_workspace_root_rejects_external_target_path() {
    let workspace = seeded_workspace();
    let external = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(workspace.path());
    let workspace_path = fs::canonicalize(workspace.path()).unwrap();
    let external_path = fs::canonicalize(external.path()).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(workspace.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg("--workspace-root")
        .arg(&workspace_path)
        .arg(&external_path)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("analysis target path"))
        .stderr(predicate::str::contains("is outside workspace root"));
}

#[test]
fn kalos_check_workspace_root_resolves_relative_target_from_outside_cwd() {
    let workspace = seeded_workspace();
    let external_cwd = TempDir::new().unwrap();
    let cache_dir = seed_fake_codeql_bundle(workspace.path());
    let workspace_path = fs::canonicalize(workspace.path()).unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(external_cwd.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .arg("--workspace-root")
        .arg(&workspace_path)
        .arg("src")
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
        .stderr(predicate::str::contains("setup bundle"))
        .stderr(predicate::str::contains("setup bundle done"))
        .stderr(predicate::str::contains("database create"))
        .stderr(predicate::str::contains("database create done"))
        .stderr(predicate::str::contains("query run"))
        .stderr(predicate::str::contains("query run done"))
        .stderr(predicate::str::contains("bqrs decode"))
        .stderr(predicate::str::contains("bqrs decode done"));
}

#[test]
fn kalos_check_progress_accounts_non_subprocess_codeql_stages() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    for expected in [
        "cache fingerprint done (",
        "cache lookup miss (",
        "cache lock wait done (",
        "cache lookup after lock miss (",
        "cache write done (",
        "bqrs parse done (",
        "codeql: normalization done (",
    ] {
        assert!(
            stderr.contains(expected),
            "expected progress to account for `{expected}` in stderr: {stderr}"
        );
    }
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
            predicate::str::is_match(r"setup bundle done \(([0-9]+\.[0-9]s|[0-9]+m [0-9]+s)\)")
                .unwrap(),
        )
        .stderr(
            predicate::str::is_match(r"database create done \(([0-9]+\.[0-9]s|[0-9]+m [0-9]+s)\)")
                .unwrap(),
        );
}

#[test]
fn kalos_check_emits_timeout_mitigation_before_codeql_phases() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let timing_index = stderr
        .find("phase timing: long CodeQL phases for rust report elapsed time on completion")
        .expect("phase timing context should be emitted before long CodeQL phases");
    let mitigation_index = stderr
        .find("timeout mitigation: if CodeQL exceeds the harness timeout")
        .expect("timeout mitigation should be emitted before long CodeQL phases");
    let database_create_index = stderr
        .find("database create")
        .expect("database create progress should be emitted");

    assert!(
        timing_index < database_create_index,
        "phase timing context should precede database create progress: {stderr}"
    );
    assert!(
        mitigation_index < database_create_index,
        "timeout mitigation should precede database create progress: {stderr}"
    );
    assert!(stderr.contains("--exclude"));
    assert!(stderr.contains("--diff"));
    assert!(stderr.contains("--cache-dir"));
    assert!(stderr.contains("--codeql-total-timeout"));
    assert!(stderr.contains("--codeql-timeout"));
    assert!(stderr.contains("--min-language-ratio"));
}

#[test]
fn kalos_check_human_format_emits_source_inventory_before_codeql_slow_path() {
    let temp = seeded_large_workspace(100);
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .arg("check")
        .assert()
        .success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    let source_inventory = "codeql: found 100 source files (rust=100)";
    let inventory_index = stderr
        .find(source_inventory)
        .expect("source inventory should be emitted");
    let database_create_index = stderr
        .find("database create")
        .expect("CodeQL database progress should be emitted");

    assert!(
        inventory_index < database_create_index,
        "source inventory should be emitted before database create progress: {stderr}"
    );
    assert!(stderr.contains("slow-path guidance"));
    assert!(stderr.contains("recommended evaluation profile"));
    assert!(stderr.contains("--level project --format json"));
    assert!(stderr.contains("--exclude"));
    assert!(stderr.contains("--cache-dir"));
    assert!(stderr.contains("--diff"));
    assert!(stderr.contains("--min-language-ratio"));
}

#[test]
fn kalos_check_quiet_human_output_still_emits_codeql_progress() {
    let temp = seeded_large_workspace(100);
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("function.txt");

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "check",
            "--level",
            "function",
            "--format",
            "human",
            "--quiet",
            "--codeql-timeout",
            "0",
            "--codeql-total-timeout",
            "0",
            "--cache-dir",
        ])
        .arg(&cache_dir)
        .arg("--output")
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("wrote ").not());
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    for expected in [
        "codeql: found 100 source files (rust=100)",
        "slow-path guidance",
        "setup bundle",
        "database create",
        "query run",
        "bqrs decode",
        "codeql: normalization done",
    ] {
        assert!(
            stderr.contains(expected),
            "quiet human output-file runs should retain CodeQL progress `{expected}` in stderr: {stderr}"
        );
    }
    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(
        rendered.contains("Summary") && rendered.contains("outcome:"),
        "expected human report to be written to output file: {rendered}"
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
fn kalos_check_rejects_min_language_ratio_outside_unit_range() {
    for args in [
        vec!["check", "--min-language-ratio", "1.5"],
        vec!["check", "--min-language-ratio=-0.1"],
    ] {
        Command::cargo_bin("kalos")
            .unwrap()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("invalid value"))
            .stderr(predicate::str::contains(
                "ratio must be between 0.0 and 1.0",
            ));
    }
}

#[test]
fn kalos_check_json_format_does_not_emit_progress_on_stderr() {
    let temp = seeded_large_workspace(100);
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Apple Silicon").not())
        .stderr(predicate::str::contains("setup bundle").not())
        .stderr(predicate::str::contains("source files").not())
        .stderr(predicate::str::contains("slow-path guidance").not())
        .stderr(predicate::str::contains("first run").not())
        .stderr(predicate::str::contains("database create").not())
        .stderr(predicate::str::contains("query run").not())
        .stderr(predicate::str::contains("bqrs decode").not())
        .stderr(predicate::str::contains("bqrs parse").not())
        .stderr(predicate::str::contains("cache fingerprint").not())
        .stderr(predicate::str::contains("normalization").not());
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let warnings = parsed["analysis_warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|warning| {
                warning.contains("CodeQL slow-path guidance")
                    && warning.contains("recommended evaluation profile")
                    && warning.contains("--level project --format json")
                    && warning.contains("shared --cache-dir")
            })
        }),
        "JSON analysis_warnings should expose slow-path guidance: {stdout}"
    );
}

#[test]
fn kalos_check_json_unbounded_large_repo_fails_fast_with_guidance() {
    let temp = seeded_large_workspace(100);

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "check",
            "--format",
            "json",
            "--codeql-timeout",
            "0",
            "--codeql-total-timeout",
            "0",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(parsed["error_class"], "expected_skip");
    assert_eq!(parsed["outcome"], "expected_skip");
    let message = parsed["message"].as_str().unwrap();
    assert!(message.contains("unbounded large-repo CodeQL analysis skipped"));
    assert!(message.contains("found 100 source files"));
    assert!(message.contains("recommended narrower command"));
    assert!(message.contains("--codeql-total-timeout 1200"));
    assert!(message.contains("--codeql-timeout 240"));
    assert!(message.contains("--cache-dir <shared-cache-dir>"));
    assert!(message.contains("--diff"));
    assert!(message.contains("--exclude"));
    assert!(message.contains("--min-language-ratio"));
    assert!(message.contains("--allow-unbounded-large-repo-analysis"));
}

#[test]
fn kalos_check_json_unbounded_large_repo_allows_explicit_opt_in() {
    let temp = seeded_large_workspace(100);
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--format",
            "json",
            "--codeql-timeout",
            "0",
            "--codeql-total-timeout",
            "0",
            "--allow-unbounded-large-repo-analysis",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[test]
fn kalos_check_sarif_format_does_not_emit_progress_on_stderr() {
    let temp = seeded_large_workspace(100);
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "sarif"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Apple Silicon").not())
        .stderr(predicate::str::contains("setup bundle").not())
        .stderr(predicate::str::contains("source files").not())
        .stderr(predicate::str::contains("slow-path guidance").not())
        .stderr(predicate::str::contains("first run").not())
        .stderr(predicate::str::contains("database create").not())
        .stderr(predicate::str::contains("query run").not())
        .stderr(predicate::str::contains("bqrs decode").not())
        .stderr(predicate::str::contains("bqrs parse").not())
        .stderr(predicate::str::contains("cache fingerprint").not())
        .stderr(predicate::str::contains("normalization").not());
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let warnings = parsed["runs"][0]["properties"]["analysis_warnings"]
        .as_array()
        .unwrap();
    assert!(
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|warning| {
                warning.contains("CodeQL slow-path guidance")
                    && warning.contains("recommended evaluation profile")
                    && warning.contains("--level project --format json")
                    && warning.contains("shared --cache-dir")
            })
        }),
        "SARIF analysis_warnings should expose slow-path guidance: {stdout}"
    );
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
        "outcome",
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
    assert_eq!(parsed["schema_version"], "1.1.0");
}

#[test]
fn kalos_check_json_strict_quality_gate_reports_diagnostics_failed_outcome() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_strict_warning_fixture(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--strict", "--format", "json"])
        .assert()
        .code(1)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["outcome"], "diagnostics_failed");
    assert!(
        parsed["summary"]["error_count"].as_u64().unwrap()
            + parsed["summary"]["warning_count"].as_u64().unwrap()
            > 0,
        "strict failure should be caused by diagnostics: {stdout}"
    );
}

#[test]
fn kalos_check_human_strict_quality_gate_reports_diagnostics_failed_outcome() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_strict_warning_fixture(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--strict", "--format", "human"])
        .assert()
        .code(1);

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("── Summary ──────────────────────────\noutcome: diagnostics_failed"),
        "human summary should expose diagnostics_failed outcome: {stdout}"
    );
}

#[test]
fn kalos_check_sarif_strict_quality_gate_reports_diagnostics_failed_outcome() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_strict_warning_fixture(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--strict", "--format", "sarif"])
        .assert()
        .code(1)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        parsed["runs"][0]["properties"]["kalos"]["schema_version"],
        "1.1.0"
    );
    assert_eq!(
        parsed["runs"][0]["properties"]["kalos"]["outcome"],
        "diagnostics_failed"
    );
}

#[test]
fn kalos_check_json_default_uses_project_level_triage_view() {
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

    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert!(parsed["scores"]["function"].is_null());
    assert!(parsed["scores"]["module"].is_null());
    assert!(parsed["scores"]["project"].is_number());
    assert!(
        parsed["metrics"]
            .as_array()
            .expect("metrics array")
            .iter()
            .all(|scope| scope["scope"]["level"] == "project")
    );
    assert!(
        parsed["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .all(|diagnostic| {
                diagnostic["primary_scope"]["level"] == "project"
                    && !diagnostic["rule_id"]
                        .as_str()
                        .is_some_and(|rule_id| matches!(rule_id, "KAL-F001" | "KAL-F003"))
            })
    );
}

#[test]
fn kalos_check_recommended_evaluation_profile_runs_project_json_with_shared_cache() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_CACHE_DIR")
        .args([
            "check",
            "--evaluation-profile",
            "recommended",
            "--cache-dir",
        ])
        .arg(&cache_dir)
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert!(parsed["scores"]["function"].is_null());
    assert!(parsed["scores"]["module"].is_null());
    assert!(parsed["scores"]["project"].is_number());
    assert!(
        parsed["metrics"]
            .as_array()
            .expect("metrics array")
            .iter()
            .all(|scope| scope["scope"]["level"] == "project")
    );
    assert!(cache_dir.join("codeql/databases/rust.cache_key").exists());
}

#[test]
fn kalos_check_output_flag_writes_json_to_file() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("result.json");

    let output_display = output_path.display().to_string();
    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--output"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(
            predicate::str::contains(format!("wrote {output_display}"))
                .and(predicate::str::contains("analyzed"))
                .and(predicate::str::contains("diagnostic")),
        );

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

    let output_display = output_path.display().to_string();
    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "sarif", "-o"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(format!("wrote {output_display}")));

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

    let output_display = output_path.display().to_string();
    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--output"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(format!("wrote {output_display}")));

    assert!(output_path.parent().unwrap().is_dir());
    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert!(parsed.is_object(), "expected JSON object output");
}

#[test]
fn kalos_check_json_output_file_receives_late_codeql_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_failing_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("failure.json");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "--output"])
        .arg(&output_path)
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value =
        serde_json::from_str(&rendered).expect("JSON failure output file should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(
        parsed["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(parsed["outcome"], "infrastructure_error");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("CodeQL `query run` failed for `rust`")
    );
    assert!(
        parsed["cause"]
            .as_str()
            .unwrap()
            .contains("kalos test forced query failure")
    );
}

#[test]
fn kalos_check_quiet_human_output_file_receives_late_codeql_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_failing_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("failure.txt");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--quiet", "--output"])
        .arg(&output_path)
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("setup bundle"))
        .stderr(predicate::str::contains("query run"))
        .stderr(predicate::str::contains("wrote ").not());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    assert!(
        rendered.contains("CodeQL `query run` failed for `rust`"),
        "expected human failure message in output file: {rendered}"
    );
    assert!(
        rendered.contains("kalos test forced query failure"),
        "expected CodeQL failure cause in output file: {rendered}"
    );
    assert!(
        !rendered.contains("error class: codeql_extraction"),
        "human extraction failures should not add an error class line: {rendered}"
    );
    assert!(
        rendered.contains("outcome: infrastructure_error"),
        "human extraction failures should expose infrastructure outcome: {rendered}"
    );
}

#[test]
fn kalos_check_json_output_file_receives_codeql_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("timeout.json");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--codeql-timeout",
            "1",
            "--format",
            "json",
            "--output",
        ])
        .arg(&output_path)
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    let parsed: Value =
        serde_json::from_str(&rendered).expect("JSON timeout output file should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(
        parsed["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(parsed["outcome"], "infrastructure_error");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("failed to execute `")
    );
    assert!(
        parsed["cause"]
            .as_str()
            .unwrap()
            .contains("timed out after 1s")
    );
}

#[test]
fn kalos_check_sarif_stdout_receives_codeql_total_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--codeql-timeout",
            "0",
            "--codeql-total-timeout",
            "1",
            "--format",
            "sarif",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("SARIF timeout output should parse as JSON");
    assert_eq!(
        parsed["runs"][0]["invocations"][0]["executionSuccessful"],
        Value::Bool(false)
    );
    assert_eq!(
        parsed["runs"][0]["properties"]["kalos"]["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(
        parsed["runs"][0]["properties"]["kalos"]["outcome"],
        Value::String("infrastructure_error".to_owned())
    );
    assert!(
        stdout.contains("timed out after 1s"),
        "expected total timeout cause in SARIF output: {stdout}"
    );
}

#[test]
fn kalos_check_json_stdout_receives_codeql_total_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--codeql-timeout",
            "0",
            "--codeql-total-timeout",
            "1",
            "--format",
            "json",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("JSON timeout output should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(
        parsed["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(parsed["outcome"], "infrastructure_error");
    assert!(
        parsed["cause"]
            .as_str()
            .unwrap()
            .contains("timed out after 1s")
    );
}

#[test]
fn kalos_check_update_gitignore_happens_before_codeql_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--update-gitignore",
            "--codeql-timeout",
            "1",
            "--format",
            "json",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("JSON timeout output should parse as JSON");
    assert_eq!(
        parsed["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(parsed["outcome"], "infrastructure_error");
    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
    assert!(temp.path().join(".kalos").exists());
}

#[test]
fn kalos_check_sarif_update_gitignore_happens_before_codeql_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--update-gitignore",
            "--codeql-timeout",
            "1",
            "--format",
            "sarif",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("SARIF timeout output should parse as JSON");
    assert_eq!(
        parsed["runs"][0]["invocations"][0]["executionSuccessful"],
        Value::Bool(false)
    );
    assert_eq!(
        parsed["runs"][0]["properties"]["kalos"]["error_class"],
        Value::String("codeql_extraction".to_owned())
    );
    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
    assert!(temp.path().join(".kalos").exists());
}

#[test]
fn kalos_check_quiet_human_output_file_receives_codeql_timeout_failure() {
    let temp = seeded_workspace();
    let cache_dir = seed_timeout_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("timeout.txt");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--codeql-timeout", "1", "--quiet", "--output"])
        .arg(&output_path)
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("setup bundle"))
        .stderr(predicate::str::contains("database create"))
        .stderr(predicate::str::contains("wrote ").not());

    let rendered = fs::read_to_string(&output_path).unwrap();
    assert!(rendered.ends_with('\n'), "expected trailing newline");
    assert!(
        rendered.contains("failed to execute `"),
        "expected human timeout message in output file: {rendered}"
    );
    assert!(
        rendered.contains("timed out after 1s"),
        "expected timeout cause in output file: {rendered}"
    );
    assert!(
        !rendered.contains("error class: codeql_extraction"),
        "human extraction failures should not add an error class line: {rendered}"
    );
    assert!(
        rendered.contains("outcome: infrastructure_error"),
        "human timeout failures should expose infrastructure outcome: {rendered}"
    );
}

#[test]
fn kalos_check_codeql_timeout_bounds_managed_bundle_setup() {
    let temp = seeded_workspace();
    let manifest = codeql_bundle_manifest().unwrap();
    let cache_dir = temp.path().join(".kalos-test-cache");
    let lock_dir = cache_dir
        .join("codeql")
        .join(format!(".codeql-bundle-{}.lock.d", manifest.version));
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("owner"), "pid=999999\n").unwrap();
    fs::write(lock_dir.join("heartbeat"), "fresh\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--cache-dir"])
        .arg(&cache_dir)
        .args(["--codeql-timeout", "1"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "setup bundle ... (timeout 1.0s; cold/cache-heavy cache phase)",
        ))
        .stderr(predicate::str::contains("bootstrap lock wait"))
        .stderr(predicate::str::contains("setup timeout is 1.0s"))
        .stderr(predicate::str::contains(
            "cold/cache-heavy CodeQL bundle setup",
        ));
}

#[test]
fn kalos_check_project_human_default_reports_bounded_cache_heavy_setup() {
    let temp = seeded_workspace();
    let manifest = codeql_bundle_manifest().unwrap();
    let cache_dir = temp.path().join(".kalos-test-cache");
    let lock_dir = cache_dir
        .join("codeql")
        .join(format!(".codeql-bundle-{}.lock.d", manifest.version));
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("owner"), "pid=999999\n").unwrap();
    fs::write(lock_dir.join("heartbeat"), "fresh\n").unwrap();

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", ".", "--level", "project", "--format", "human"])
        .args(["--cache-dir"])
        .arg(&cache_dir)
        .args(["--codeql-timeout", "1"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "setup bundle ... (timeout 1.0s; cold/cache-heavy cache phase)",
        ))
        .stderr(predicate::str::contains("setup timeout is 1.0s"))
        .stderr(predicate::str::contains(
            "Timed out during cold/cache-heavy CodeQL bundle setup or extraction",
        ))
        .stderr(predicate::str::contains(
            "error class: codeql_infrastructure",
        ));
}

#[test]
fn kalos_check_output_directory_fails_before_codeql_setup() {
    let temp = seeded_workspace();
    let output_dir = temp.path().join("report-dir");
    fs::create_dir(&output_dir).unwrap();
    let expected_message = format!(
        "output path `{}` is a directory; pass a file path to --output",
        output_dir.display()
    );

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_CACHE_DIR")
        .args(["check", "--format", "json", "--output"])
        .arg(&output_dir)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("\"error\":true"))
        .stdout(predicate::str::contains(expected_message.clone()))
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains("CodeQL").not())
        .stdout(predicate::str::contains("bundle").not())
        .stdout(predicate::str::contains("download").not())
        .stderr(predicate::str::contains("CodeQL").not())
        .stderr(predicate::str::contains("bundle").not())
        .stderr(predicate::str::contains("download").not());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed.get("error").and_then(Value::as_bool), Some(true));
    assert_eq!(
        parsed.get("message").and_then(Value::as_str),
        Some(expected_message.as_str())
    );
    assert!(output_dir.is_dir());
}

#[test]
fn kalos_check_output_flag_quiet_suppresses_acknowledgment() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("result.json");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--format",
            "json",
            "--update-gitignore",
            "--quiet",
            "--output",
        ])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    let parsed: Value = serde_json::from_str(&rendered).unwrap();
    assert!(parsed.is_object(), "expected JSON object output");
    assert_eq!(
        fs::read_to_string(temp.path().join(".gitignore")).unwrap(),
        ".kalos/\n"
    );
}

#[test]
fn kalos_check_output_flag_quiet_short_flag_suppresses_acknowledgment() {
    let temp = seeded_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let output_path = temp.path().join("reports").join("result.json");

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--format", "json", "-q", "--output"])
        .arg(&output_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("wrote ").not());

    let rendered = fs::read_to_string(&output_path).unwrap();
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
    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert_eq!(
        parsed["summary"]["error_count"].as_u64().unwrap()
            + parsed["summary"]["warning_count"].as_u64().unwrap()
            + parsed["summary"]["info_count"].as_u64().unwrap(),
        parsed["diagnostics"].as_array().unwrap().len() as u64
    );
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

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("JSON failure output on stdout should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(parsed["error_class"], "tool_error");
    assert_eq!(parsed["outcome"], "tool_error");
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
    assert!(
        assert.get_output().stderr.is_empty(),
        "stderr should not carry JSON failure payload"
    );
}

#[test]
fn kalos_check_missing_target_json_error_output_names_requested_path() {
    let temp = TempDir::new().unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "check",
            "--format",
            "json",
            "--level",
            "project",
            "does-not-exist-kalos-eval",
        ])
        .assert()
        .code(2);

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("JSON failure output on stdout should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(parsed["error_class"], "input_error");
    assert_eq!(parsed["outcome"], "input_error");

    let message = parsed["message"].as_str().unwrap();
    assert!(message.contains("analysis target path"));
    assert!(message.contains("does-not-exist-kalos-eval"));
    assert!(
        assert.get_output().stderr.is_empty(),
        "stderr should not carry JSON failure payload"
    );
}

#[test]
fn kalos_check_missing_target_human_error_output_reports_input_error_outcome() {
    let temp = TempDir::new().unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args(["check", "--level", "project", "does-not-exist-kalos-eval"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("analysis target path"));
    assert!(stderr.contains("outcome: input_error"));
    assert!(serde_json::from_str::<Value>(&stderr).is_err());
}

#[test]
fn kalos_check_missing_config_sarif_error_output_is_sarif_document() {
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

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("SARIF failure output on stdout should parse as JSON");

    assert_eq!(parsed["version"], Value::String("2.1.0".to_owned()));
    assert_eq!(
        parsed["$schema"],
        Value::String("https://json.schemastore.org/sarif-2.1.0.json".to_owned())
    );

    let run = &parsed["runs"][0];
    assert_eq!(
        run["tool"]["driver"]["name"],
        Value::String("kalos".to_owned())
    );
    assert_eq!(run["results"].as_array().expect("results array").len(), 0);

    let invocation = &run["invocations"][0];
    assert_eq!(invocation["executionSuccessful"], Value::Bool(false));
    assert_eq!(invocation["exitCode"], Value::Number(2.into()));

    let notification = &invocation["toolExecutionNotifications"][0];
    assert_eq!(notification["level"], Value::String("error".to_owned()));
    assert_eq!(notification["properties"]["outcome"], "tool_error");
    assert!(
        notification["message"]["text"]
            .as_str()
            .unwrap()
            .contains("failed to load config file")
    );
    assert!(
        notification["properties"]["cause"]
            .as_str()
            .unwrap()
            .contains("No such file or directory")
    );

    let kalos_props = &run["properties"]["kalos"];
    assert_eq!(kalos_props["error"], Value::Bool(true));
    assert_eq!(kalos_props["outcome"], "tool_error");
    assert!(
        kalos_props["message"]
            .as_str()
            .unwrap()
            .contains("failed to load config file")
    );

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        serde_json::from_str::<Value>(&stderr).is_err(),
        "stderr should not carry a JSON error object when SARIF is requested: {stderr}"
    );
}

#[test]
fn kalos_check_missing_config_sarif_with_output_writes_sarif_error_document_to_file() {
    let temp = TempDir::new().unwrap();
    let output_path = temp.path().join("outputs").join("out.sarif");

    assert!(!output_path.parent().unwrap().exists());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .args([
            "check",
            "--format",
            "sarif",
            "--config",
            "/nonexistent/path",
            "--output",
        ])
        .arg(&output_path)
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let rendered = fs::read_to_string(&output_path).unwrap();
    let parsed: Value =
        serde_json::from_str(&rendered).expect("SARIF failure output file should parse as JSON");

    assert_eq!(parsed["version"], Value::String("2.1.0".to_owned()));
    assert_eq!(
        parsed["$schema"],
        Value::String("https://json.schemastore.org/sarif-2.1.0.json".to_owned())
    );

    let run = &parsed["runs"][0];
    assert_eq!(run["results"], Value::Array(vec![]));

    let invocation = &run["invocations"][0];
    assert_eq!(invocation["executionSuccessful"], Value::Bool(false));
    assert_eq!(invocation["exitCode"], Value::Number(2.into()));

    let notification = &invocation["toolExecutionNotifications"][0];
    assert_eq!(notification["level"], Value::String("error".to_owned()));
    assert!(
        notification["message"]["text"]
            .as_str()
            .unwrap()
            .contains("failed to load config file")
    );
    assert!(
        notification["properties"]["cause"]
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
    assert!(stderr.contains("outcome: tool_error"));
}

#[test]
fn kalos_check_llm_missing_api_key_fails_preflight() {
    let temp = seeded_workspace();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_LLM_API_KEY")
        .env_remove("KALOS_LLM_PROVIDER")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("KALOS_LLM_API_KEY"));
    assert!(stderr.contains("outcome: expected_skip"));
    assert_no_gitignore_chatter(&stderr);
    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_llm_missing_api_key_json_is_expected_skip() {
    let temp = seeded_workspace();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_LLM_API_KEY")
        .env_remove("KALOS_LLM_PROVIDER")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm", "--format", "json"])
        .assert()
        .code(2);

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value =
        serde_json::from_str(&stdout).expect("JSON failure output should parse as JSON");
    assert_eq!(parsed["error"], Value::Bool(true));
    assert_eq!(parsed["error_class"], "expected_skip");
    assert_eq!(parsed["outcome"], "expected_skip");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("KALOS_LLM_API_KEY")
    );
    assert!(
        assert.get_output().stderr.is_empty(),
        "stderr should not carry JSON failure payload"
    );
}

#[test]
fn kalos_check_llm_unsupported_provider_fails_preflight() {
    let temp = seeded_workspace();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_LLM_API_KEY", "secret")
        .env("KALOS_LLM_PROVIDER", "anthropic")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("unsupported KALOS_LLM_PROVIDER `anthropic`"));
    assert_no_gitignore_chatter(&stderr);
    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn kalos_check_llm_invalid_endpoint_fails_preflight() {
    let temp = seeded_workspace();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_LLM_API_KEY", "secret")
        .env_remove("KALOS_LLM_PROVIDER")
        .env("KALOS_LLM_ENDPOINT_URL", "not a url")
        .args(["check", "--llm"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("KALOS_LLM_ENDPOINT_URL is not a valid URL"));
    assert_no_gitignore_chatter(&stderr);
    assert!(!temp.path().join(".gitignore").exists());
}

// Issue #69 regression: when a user already has a `.gitignore` without the
// `.kalos/` entry, the `--llm` preflight failure must not surface any
// gitignore-related notice/warning (which would obscure the real cause) and
// must leave the existing file untouched.
#[test]
fn kalos_check_llm_preflight_failure_preserves_existing_gitignore() {
    let temp = seeded_workspace();
    let gitignore_path = temp.path().join(".gitignore");
    let original_contents = "target/\n";
    fs::write(&gitignore_path, original_contents).unwrap();

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env_remove("KALOS_LLM_API_KEY")
        .env_remove("KALOS_LLM_PROVIDER")
        .env_remove("KALOS_LLM_ENDPOINT_URL")
        .args(["check", "--llm"])
        .assert()
        .code(2);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("KALOS_LLM_API_KEY"));
    assert_no_gitignore_chatter(&stderr);

    let contents = fs::read_to_string(&gitignore_path).unwrap();
    assert_eq!(contents, original_contents);
}

// Catches issue #69 regressions: a `.gitignore`-related notice/warning must
// never accompany a `--llm` preflight failure because it precedes the root
// cause and misleads the user. The preflight errors themselves do not mention
// `.gitignore`, so this substring check is safe.
fn assert_no_gitignore_chatter(stderr: &str) {
    assert!(
        !stderr.contains(".gitignore"),
        "stderr on --llm preflight failure must not mention .gitignore; got:\n{stderr}"
    );
}

#[test]
fn kalos_check_diff_falls_back_to_full_analysis_when_baseline_is_missing() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "compatible diff baseline was not found; falling back to full analysis",
        ));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.starts_with("Full analysis fallback completed; showing "),
        "human headline should make full-analysis fallback explicit, got:\n{stdout}"
    );
    assert!(
        !stdout.starts_with("Analyzed "),
        "human headline must not look like a plain diff-scoped analysis after fallback, got:\n{stdout}"
    );
    let note_position = stdout
        .find("note: compatible diff baseline was not found; falling back to full analysis")
        .expect("human output should surface the fallback notice");
    let summary_position = stdout
        .find("── Summary ")
        .expect("human output should include Summary header");
    assert!(
        note_position < summary_position,
        "fallback notice should appear before Summary, got:\n{stdout}"
    );

    let baselines_dir = cache_dir.join("baselines");
    assert!(baselines_dir.is_dir());
    assert_eq!(
        fs::read_dir(&baselines_dir).unwrap().count(),
        1,
        "baseline cache should contain exactly one entry after fallback",
    );
}

#[test]
fn kalos_check_diff_explicit_target_reports_full_fallback_context() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());
    let explicit_target_fallback_reason = "diff mode is not available for explicitly specified targets; falling back to full analysis";

    let human = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1", "src/lib.rs"])
        .assert()
        .success()
        .stderr(predicate::str::contains(explicit_target_fallback_reason));
    let human_stdout = String::from_utf8(human.get_output().stdout.clone()).unwrap();
    assert!(
        human_stdout.starts_with("Full analysis fallback completed; analyzed "),
        "human headline should identify explicit-target diff fallback as full analysis, got:\n{human_stdout}"
    );
    assert!(
        human_stdout.contains(
            "note: diff requested base HEAD~1; base status not_evaluated; effective analysis full"
        ),
        "human output should include diff base evaluation state, got:\n{human_stdout}"
    );

    let json = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args([
            "check",
            "--diff",
            "HEAD~1",
            "src/lib.rs",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(explicit_target_fallback_reason));
    let json_stdout = String::from_utf8(json.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&json_stdout).unwrap();

    assert_eq!(parsed["diff_execution"]["requested_mode"], "diff");
    assert_eq!(parsed["diff_execution"]["requested_base_ref"], "HEAD~1");
    assert_eq!(parsed["diff_execution"]["base_status"], "not_evaluated");
    assert_eq!(parsed["diff_execution"]["effective_mode"], "full");
    assert_eq!(
        parsed["diff_execution"]["fallback_reason"],
        explicit_target_fallback_reason
    );
    assert!(parsed["diff_execution"]["changed_file_count"].is_null());
    assert_eq!(
        parsed["analysis_targets"],
        serde_json::json!(["src/lib.rs"])
    );
    let warnings = parsed["analysis_warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|warning| warning == explicit_target_fallback_reason),
        "JSON analysis_warnings should include the explicit-target fallback reason, got {warnings:?}"
    );
    assert_eq!(baseline_entry_count(&cache_dir), 0);
}

#[test]
fn kalos_check_diff_json_surfaces_fallback_reason_in_analysis_warnings() {
    let temp = seeded_git_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD~1", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "compatible diff baseline was not found; falling back to full analysis",
        ));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let warnings = parsed["analysis_warnings"]
        .as_array()
        .expect("analysis_warnings should be an array");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.as_str().is_some_and(|text| text
                == "compatible diff baseline was not found; falling back to full analysis")),
        "JSON analysis_warnings should include the fallback reason, got {warnings:?}",
    );
}

#[test]
fn kalos_check_diff_cached_run_json_does_not_report_fallback_warning() {
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
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let warnings = parsed["analysis_warnings"]
        .as_array()
        .expect("analysis_warnings should be an array");
    assert!(
        !warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("falling back to full analysis"))),
        "cached diff run should not surface fallback warnings, got {warnings:?}",
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
    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert_eq!(
        parsed["summary"]["error_count"].as_u64().unwrap()
            + parsed["summary"]["warning_count"].as_u64().unwrap()
            + parsed["summary"]["info_count"].as_u64().unwrap(),
        parsed["diagnostics"].as_array().unwrap().len() as u64
    );
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
    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert_eq!(
        parsed["summary"]["error_count"].as_u64().unwrap()
            + parsed["summary"]["warning_count"].as_u64().unwrap()
            + parsed["summary"]["info_count"].as_u64().unwrap(),
        parsed["diagnostics"].as_array().unwrap().len() as u64
    );
}

#[test]
fn kalos_check_diff_keeps_summary_consistent_with_affected_diagnostics_issue_56() {
    let temp = seeded_issue_56_workspace();
    let cache_dir = seed_fake_codeql_bundle(temp.path());

    Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD^", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("falling back to full analysis"));
    assert_eq!(baseline_entry_count(&cache_dir), 1);

    let assert = Command::cargo_bin("kalos")
        .unwrap()
        .current_dir(temp.path())
        .env("KALOS_CACHE_DIR", &cache_dir)
        .args(["check", "--diff", "HEAD^", "--format", "json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("falling back to full analysis").not());
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let diagnostics = parsed["diagnostics"].as_array().unwrap();
    let summary_total = parsed["summary"]["error_count"].as_u64().unwrap()
        + parsed["summary"]["warning_count"].as_u64().unwrap()
        + parsed["summary"]["info_count"].as_u64().unwrap();

    assert_eq!(parsed["diagnostics_scope"], "affected_only");
    assert_eq!(parsed["summary_scope"], "listed_diagnostics");
    assert_eq!(summary_total, diagnostics.len() as u64);
    assert!(diagnostics.is_empty());
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

fn seeded_large_workspace(source_file_count: usize) -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    for index in 0..source_file_count {
        fs::write(
            temp.path().join(format!("src/module_{index}.rs")),
            format!("pub fn module_{index}() -> i32 {{ {index} }}\n"),
        )
        .unwrap();
    }
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

fn seeded_issue_56_workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn entry() -> i32 {\n    helper()\n}\n\npub fn helper() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(temp.path().join("README.md"), "before\n").unwrap();
    fs::write(
        temp.path().join(".kalos.toml"),
        "[rules.KAL-F001]\nthreshold = 0.0\nseverity = \"error\"\n",
    )
    .unwrap();

    run_git(temp.path(), &["init"]);
    run_git(temp.path(), &["config", "user.email", "kalos@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Kalos"]);
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-m", "initial"]);

    fs::write(temp.path().join("README.md"), "after\n").unwrap();
    run_git(temp.path(), &["add", "README.md"]);
    run_git(temp.path(), &["commit", "-m", "docs"]);

    temp
}

fn seed_fake_codeql_bundle(workspace_root: &Path) -> PathBuf {
    seed_fake_codeql_bundle_with_fixture(workspace_root, &load_fixture("rust.json"))
}

fn seed_strict_warning_fixture(workspace_root: &Path) -> PathBuf {
    fs::write(
        workspace_root.join(".kalos.toml"),
        "[rules.KAL-PAT003]\nseverity = \"warning\"\n",
    )
    .unwrap();
    fs::write(
        workspace_root.join("src/lib.rs"),
        "mod a;\nmod b;\n\npub fn placeholder() -> i32 {\n    a::call_b() + b::call_a()\n}\n",
    )
    .unwrap();
    fs::write(
        workspace_root.join("src/a.rs"),
        "pub fn call_b() -> i32 {\n    crate::b::call_a()\n}\n",
    )
    .unwrap();
    fs::write(
        workspace_root.join("src/b.rs"),
        "pub fn call_a() -> i32 {\n    crate::a::call_b()\n}\n",
    )
    .unwrap();
    run_git(workspace_root, &["add", "."]);
    run_git(workspace_root, &["commit", "-m", "strict warning fixture"]);

    seed_fake_codeql_bundle_with_fixture(
        workspace_root,
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
    )
}

fn seed_failing_codeql_bundle(workspace_root: &Path) -> PathBuf {
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
    write_failing_codeql_executable(&codeql_executable_path(&bundle_dir));
    cache_dir
}

fn seed_timeout_codeql_bundle(workspace_root: &Path) -> PathBuf {
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
    write_timeout_codeql_executable(&codeql_executable_path(&bundle_dir));
    cache_dir
}

fn seed_invalid_managed_bundle(workspace_root: &Path) -> PathBuf {
    let manifest = codeql_bundle_manifest().unwrap();
    let cache_dir = workspace_root.join(".kalos-test-cache");
    let bundle_dir = cache_dir.join("codeql").join(&manifest.version);
    fs::create_dir_all(&bundle_dir).unwrap();
    fs::write(bundle_dir.join("bundle.marker"), "0".repeat(64)).unwrap();
    cache_dir
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

#[cfg(unix)]
fn write_failing_codeql_executable(path: &Path) {
    let script = "#!/bin/sh\nif [ \"$1\" = \"resolve\" ] && [ \"$2\" = \"languages\" ]; then\n  cat <<'EOF'\n{\"go\":[],\"javascript\":[],\"python\":[],\"rust\":[]}\nEOF\n  exit 0\nfi\nif [ \"$1\" = \"database\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"query\" ] && [ \"$2\" = \"run\" ]; then\n  echo \"kalos test forced query failure\" >&2\n  exit 7\nfi\necho \"unexpected invocation: $@\" >&2\nexit 1\n";
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn write_failing_codeql_executable(path: &Path) {
    let source_path = path.with_file_name("codeql_failing_fixture.rs");
    let source = "use std::env;\nuse std::io::Write;\n\nfn main() {\n    let args = env::args().skip(1).collect::<Vec<_>>();\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"resolve\" && action == \"languages\") {\n        print!(\"{\\\"go\\\":[],\\\"javascript\\\":[],\\\"python\\\":[],\\\"rust\\\":[]}\");\n        return;\n    }\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"database\" && action == \"create\") {\n        return;\n    }\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"query\" && action == \"run\") {\n        let _ = writeln!(std::io::stderr(), \"kalos test forced query failure\");\n        std::process::exit(7);\n    }\n    let _ = writeln!(std::io::stderr(), \"unexpected invocation: {}\", args.join(\" \"));\n    std::process::exit(1);\n}\n";
    fs::write(&source_path, source).unwrap();
    let status = StdCommand::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--edition=2024")
        .arg("--crate-name")
        .arg("kalos_failing_codeql")
        .arg(&source_path)
        .arg("-o")
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "failing fake CodeQL fixture compilation should succeed"
    );
    fs::remove_file(source_path).unwrap();
}

#[cfg(unix)]
fn write_timeout_codeql_executable(path: &Path) {
    let script = "#!/bin/sh\nif [ \"$1\" = \"resolve\" ] && [ \"$2\" = \"languages\" ]; then\n  cat <<'EOF'\n{\"go\":[],\"javascript\":[],\"python\":[],\"rust\":[]}\nEOF\n  exit 0\nfi\nif [ \"$1\" = \"database\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"query\" ] && [ \"$2\" = \"run\" ]; then\n  sleep 30\n  exit 0\nfi\necho \"unexpected invocation: $@\" >&2\nexit 1\n";
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn write_timeout_codeql_executable(path: &Path) {
    let source_path = path.with_file_name("codeql_timeout_fixture.rs");
    let source = "use std::env;\nuse std::io::Write;\nuse std::time::Duration;\n\nfn main() {\n    let args = env::args().skip(1).collect::<Vec<_>>();\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"resolve\" && action == \"languages\") {\n        print!(\"{\\\"go\\\":[],\\\"javascript\\\":[],\\\"python\\\":[],\\\"rust\\\":[]}\");\n        return;\n    }\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"database\" && action == \"create\") {\n        return;\n    }\n    if matches!(args.as_slice(), [stage, action, ..] if stage == \"query\" && action == \"run\") {\n        std::thread::sleep(Duration::from_secs(30));\n        return;\n    }\n    let _ = writeln!(std::io::stderr(), \"unexpected invocation: {}\", args.join(\" \"));\n    std::process::exit(1);\n}\n";
    fs::write(&source_path, source).unwrap();
    let status = StdCommand::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--edition=2024")
        .arg("--crate-name")
        .arg("kalos_timeout_codeql")
        .arg(&source_path)
        .arg("-o")
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "timeout fake CodeQL fixture compilation should succeed"
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
