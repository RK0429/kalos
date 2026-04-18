use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

use thiserror::Error;

use crate::domains::FilePath;
use crate::platform::fs::path_to_forward_slashes;
use crate::ports::diff_source::{DiffRequest, DiffSnapshot, DiffSourcePort};

#[derive(Clone, Debug, Default)]
pub struct GitDiffAdapter;

#[derive(Debug, Error)]
pub enum GitDiffError {
    #[error("git ref `{base_ref}` was not found")]
    RefNotFound { base_ref: String },
    #[error("git command `git {args}` failed with status {status}: {stderr}")]
    CommandFailed {
        args: String,
        status: i32,
        stderr: String,
    },
    #[error("git command `git {args}` produced non-utf-8 {stream}")]
    NonUtf8Output { args: String, stream: &'static str },
    #[error("failed to execute git command: {0}")]
    Io(#[from] io::Error),
}

impl DiffSourcePort for GitDiffAdapter {
    type Error = GitDiffError;

    fn diff(&self, request: &DiffRequest) -> Result<DiffSnapshot, Self::Error> {
        verify_base_ref(&request.workspace_root, &request.base_ref)?;

        let tree_ref = format!("{}^{{tree}}", request.base_ref);
        let base_snapshot_hash =
            git_stdout(&request.workspace_root, &["rev-parse", tree_ref.as_str()])?;
        let changed = git_stdout(
            &request.workspace_root,
            &[
                "diff",
                "--name-only",
                &format!("{}...HEAD", request.base_ref),
            ],
        )?;

        Ok(DiffSnapshot {
            base_snapshot_hash,
            changed_files: filter_changed_files(&changed, &request.analysis_targets),
        })
    }
}

fn verify_base_ref(workspace_root: &Path, base_ref: &str) -> Result<(), GitDiffError> {
    let args = ["rev-parse", "--verify", base_ref];
    let output = execute_git(workspace_root, &args)?;
    let _ = decode_utf8(&args, "stderr", output.stderr)?;

    if output.status.success() {
        let _ = decode_utf8(&args, "stdout", output.stdout)?;
        Ok(())
    } else {
        Err(GitDiffError::RefNotFound {
            base_ref: base_ref.to_owned(),
        })
    }
}

fn git_stdout(workspace_root: &Path, args: &[&str]) -> Result<String, GitDiffError> {
    let output = execute_git(workspace_root, args)?;
    let stdout = decode_utf8(args, "stdout", output.stdout)?;
    let stderr = decode_utf8(args, "stderr", output.stderr)?;

    if output.status.success() {
        Ok(stdout.trim().to_owned())
    } else {
        Err(GitDiffError::CommandFailed {
            args: format_git_args(args),
            status: output.status.code().unwrap_or(-1),
            stderr,
        })
    }
}

fn execute_git(workspace_root: &Path, args: &[&str]) -> Result<Output, io::Error> {
    Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()
}

fn decode_utf8(
    args: &[&str],
    stream: &'static str,
    bytes: Vec<u8>,
) -> Result<String, GitDiffError> {
    String::from_utf8(bytes).map_err(|_| GitDiffError::NonUtf8Output {
        args: format_git_args(args),
        stream,
    })
}

fn format_git_args(args: &[&str]) -> String {
    args.join(" ")
}

fn filter_changed_files(output: &str, analysis_targets: &[FilePath]) -> BTreeSet<FilePath> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| path_to_forward_slashes(Path::new(line)))
        .filter(|path| matches_analysis_targets(path, analysis_targets))
        .map(FilePath::from)
        .collect()
}

fn matches_analysis_targets(path: &str, analysis_targets: &[FilePath]) -> bool {
    if analysis_targets.is_empty() {
        return true;
    }

    analysis_targets
        .iter()
        .any(|target| target_matches(path, target.as_str()))
}

fn target_matches(path: &str, target: &str) -> bool {
    let normalized = target.trim_matches('/');
    normalized.is_empty()
        || normalized == "."
        || path == normalized
        || path
            .strip_prefix(normalized)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::{GitDiffAdapter, GitDiffError};
    use crate::domains::FilePath;
    use crate::ports::diff_source::{DiffRequest, DiffSourcePort};

    #[test]
    fn git_diff_adapter_reports_changed_files_and_base_tree_hash() {
        let repo = init_repo();
        write_file(repo.path(), "src/lib.rs", "fn answer() -> i32 { 41 }\n");
        write_file(repo.path(), "docs/readme.md", "hello\n");
        commit_all(repo.path(), "initial");

        write_file(repo.path(), "src/lib.rs", "fn answer() -> i32 { 42 }\n");
        write_file(repo.path(), "docs/readme.md", "updated\n");
        commit_all(repo.path(), "update");

        let base_ref = git_stdout(repo.path(), &["rev-parse", "HEAD~1"]);
        let expected_tree =
            git_stdout(repo.path(), &["rev-parse", &format!("{base_ref}^{{tree}}")]);

        let snapshot = GitDiffAdapter
            .diff(&DiffRequest {
                workspace_root: repo.path().to_path_buf(),
                base_ref,
                analysis_targets: vec![FilePath::from("src")],
            })
            .expect("git diff should succeed");

        assert_eq!(snapshot.base_snapshot_hash, expected_tree);
        assert_eq!(
            snapshot.changed_files,
            BTreeSet::from([FilePath::from("src/lib.rs")])
        );
        assert_eq!(
            git_stdout(
                repo.path(),
                &["cat-file", "-t", &snapshot.base_snapshot_hash]
            ),
            "tree"
        );
    }

    #[test]
    fn git_diff_adapter_returns_ref_not_found_for_unknown_base_ref() {
        let repo = init_repo();
        write_file(repo.path(), "src/lib.rs", "fn main() {}\n");
        commit_all(repo.path(), "initial");

        let error = GitDiffAdapter
            .diff(&DiffRequest {
                workspace_root: repo.path().to_path_buf(),
                base_ref: "missing-ref".to_owned(),
                analysis_targets: vec![FilePath::from(".")],
            })
            .expect_err("unknown ref should fail");

        assert!(matches!(
            error,
            GitDiffError::RefNotFound { base_ref } if base_ref == "missing-ref"
        ));
    }

    fn init_repo() -> TempDir {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        run_git(temp_dir.path(), &["init"]);
        run_git(
            temp_dir.path(),
            &["config", "user.email", "kalos@example.com"],
        );
        run_git(temp_dir.path(), &["config", "user.name", "Kalos"]);
        temp_dir
    }

    fn write_file(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }
        fs::write(path, contents).expect("file should be written");
    }

    fn commit_all(root: &Path, message: &str) {
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", message]);
    }

    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git stdout should be utf-8")
            .trim()
            .to_owned()
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
