use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use clap::Args;

use crate::domains::config::{CONFIG_FILE_NAME, render_default_config};

pub(super) const KALOS_DIR_ENTRY: &str = ".kalos/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitignoreUpdate {
    Created,
    Added,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitignoreStatus {
    Missing,
    EntryPresent,
    EntryAbsent,
}

#[derive(Debug, Clone, Default, Args)]
#[command(
    about = "create a default configuration file",
    long_about = "create a default configuration file.\n\
                  \n\
                  writes a default .kalos.toml to the current directory and ensures \
                  .gitignore contains a .kalos/ entry, creating .gitignore if absent. \
                  if .kalos.toml already exists, prompts for confirmation before overwriting \
                  on interactive stdin; any response other than `y` or `Y` preserves the \
                  existing file. pass --force (aliases -f, --yes, -y) to overwrite without \
                  prompting, which is also required on non-interactive stdin."
)]
pub struct InitCommand {
    #[arg(
        long,
        short = 'f',
        visible_alias = "yes",
        visible_short_alias = 'y',
        help = "overwrite existing configuration without prompting"
    )]
    pub force: bool,
}

impl InitCommand {
    pub fn execute(&self) -> ExitCode {
        let cwd = match env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                eprintln!("failed to determine current directory: {error}");
                return ExitCode::from(2);
            }
        };

        let config_path = cwd.join(CONFIG_FILE_NAME);
        if config_path.exists() {
            if self.force {
                // Skip prompting and continue to overwrite below.
            } else if !io::stdin().is_terminal() {
                eprintln!(
                    "{CONFIG_FILE_NAME} already exists; pass --force to overwrite (refusing to prompt on non-interactive stdin)"
                );
                return ExitCode::from(2);
            } else {
                let stdin = io::stdin();
                let mut stdin = stdin.lock();
                let stdout = io::stdout();
                let mut stdout = stdout.lock();
                let confirmed = match confirm_overwrite(&mut stdin, &mut stdout, CONFIG_FILE_NAME) {
                    Ok(confirmed) => confirmed,
                    Err(error) => {
                        eprintln!("failed to confirm overwrite: {error}");
                        return ExitCode::from(2);
                    }
                };

                if !confirmed {
                    println!("aborted");
                    return ExitCode::SUCCESS;
                }
            }
        }

        if let Err(error) = fs::write(&config_path, render_default_config()) {
            eprintln!("failed to write {}: {error}", config_path.display());
            return ExitCode::from(2);
        }

        println!("created {}", config_path.display());
        match ensure_gitignore_entry(&cwd) {
            Ok(GitignoreUpdate::Created) => {
                println!("created .gitignore with {KALOS_DIR_ENTRY} entry");
            }
            Ok(GitignoreUpdate::Added) => {
                println!("added {KALOS_DIR_ENTRY} to .gitignore");
            }
            Ok(GitignoreUpdate::Unchanged) => {}
            Err(error) => {
                eprintln!("warning: failed to update .gitignore: {error}");
            }
        }
        ExitCode::SUCCESS
    }
}

fn confirm_overwrite<R: BufRead, W: Write>(
    stdin: &mut R,
    stdout: &mut W,
    prompt_name: &str,
) -> io::Result<bool> {
    write!(stdout, "{prompt_name} already exists. Overwrite? [y/N] ")?;
    stdout.flush()?;

    let mut input = String::new();
    stdin.read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y"))
}

pub(super) fn ensure_gitignore_entry(cwd: &std::path::Path) -> io::Result<GitignoreUpdate> {
    let gitignore_path = cwd.join(".gitignore");

    if gitignore_path.exists() {
        let contents = fs::read_to_string(&gitignore_path)?;
        if contains_kalos_entry(&contents) {
            return Ok(GitignoreUpdate::Unchanged);
        }

        let mut updated_contents = contents;
        updated_contents.push('\n');
        updated_contents.push_str(KALOS_DIR_ENTRY);
        updated_contents.push('\n');
        fs::write(&gitignore_path, updated_contents)?;
        return Ok(GitignoreUpdate::Added);
    }

    fs::write(&gitignore_path, format!("{KALOS_DIR_ENTRY}\n"))?;
    Ok(GitignoreUpdate::Created)
}

pub(super) fn gitignore_entry_status(cwd: &std::path::Path) -> io::Result<GitignoreStatus> {
    let gitignore_path = cwd.join(".gitignore");

    if !gitignore_path.exists() {
        return Ok(GitignoreStatus::Missing);
    }

    let contents = fs::read_to_string(&gitignore_path)?;
    if contains_kalos_entry(&contents) {
        Ok(GitignoreStatus::EntryPresent)
    } else {
        Ok(GitignoreStatus::EntryAbsent)
    }
}

fn contains_kalos_entry(contents: &str) -> bool {
    contents.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.starts_with('#') && trimmed == KALOS_DIR_ENTRY
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::{GitignoreUpdate, KALOS_DIR_ENTRY, confirm_overwrite, ensure_gitignore_entry};

    #[test]
    fn adds_kalos_entry_to_existing_gitignore() {
        let temp = TempDir::new().unwrap();
        let gitignore_path = temp.path().join(".gitignore");
        fs::write(&gitignore_path, "target/\n").unwrap();

        let update = ensure_gitignore_entry(temp.path()).unwrap();

        let contents = fs::read_to_string(gitignore_path).unwrap();
        assert_eq!(update, GitignoreUpdate::Added);
        assert!(contents.lines().any(|line| line.trim() == KALOS_DIR_ENTRY));
        assert_eq!(kalos_entry_count(&contents), 1);
    }

    #[test]
    fn creates_gitignore_with_kalos_entry_when_missing() {
        let temp = TempDir::new().unwrap();

        let update = ensure_gitignore_entry(temp.path()).unwrap();

        let contents = fs::read_to_string(temp.path().join(".gitignore")).unwrap();
        assert_eq!(update, GitignoreUpdate::Created);
        assert_eq!(contents, format!("{KALOS_DIR_ENTRY}\n"));
    }

    #[test]
    fn updates_gitignore_at_specified_path_not_subdirectory() {
        let temp = TempDir::new().unwrap();
        let subdirectory = temp.path().join("nested");
        fs::create_dir(&subdirectory).unwrap();

        let update = ensure_gitignore_entry(temp.path()).unwrap();

        let root_gitignore = temp.path().join(".gitignore");
        let contents = fs::read_to_string(&root_gitignore).unwrap();
        assert_eq!(update, GitignoreUpdate::Created);
        assert_eq!(contents, format!("{KALOS_DIR_ENTRY}\n"));
        assert!(!subdirectory.join(".gitignore").exists());
    }

    #[test]
    fn does_not_duplicate_existing_kalos_entry() {
        let temp = TempDir::new().unwrap();
        let gitignore_path = temp.path().join(".gitignore");
        fs::write(&gitignore_path, format!("target/\n{KALOS_DIR_ENTRY}\n")).unwrap();

        let update = ensure_gitignore_entry(temp.path()).unwrap();

        let contents = fs::read_to_string(gitignore_path).unwrap();
        assert_eq!(update, GitignoreUpdate::Unchanged);
        assert_eq!(kalos_entry_count(&contents), 1);
    }

    #[test]
    fn ensure_gitignore_entry_is_idempotent() {
        let temp = TempDir::new().unwrap();
        let gitignore_path = temp.path().join(".gitignore");
        fs::write(&gitignore_path, "target/\n").unwrap();

        let first = ensure_gitignore_entry(temp.path()).unwrap();
        let second = ensure_gitignore_entry(temp.path()).unwrap();

        let contents = fs::read_to_string(gitignore_path).unwrap();
        assert_eq!(first, GitignoreUpdate::Added);
        assert_eq!(second, GitignoreUpdate::Unchanged);
        assert_eq!(kalos_entry_count(&contents), 1);
    }

    #[test]
    fn confirm_overwrite_accepts_lowercase_y() {
        let mut stdin = Cursor::new(b"y\n");
        let mut stdout = Vec::new();

        let confirmed = confirm_overwrite(&mut stdin, &mut stdout, ".kalos.toml").unwrap();

        assert!(confirmed);
    }

    #[test]
    fn confirm_overwrite_accepts_uppercase_y() {
        let mut stdin = Cursor::new(b"Y\n");
        let mut stdout = Vec::new();

        let confirmed = confirm_overwrite(&mut stdin, &mut stdout, ".kalos.toml").unwrap();

        assert!(confirmed);
    }

    #[test]
    fn confirm_overwrite_rejects_n() {
        let mut stdin = Cursor::new(b"n\n");
        let mut stdout = Vec::new();

        let confirmed = confirm_overwrite(&mut stdin, &mut stdout, ".kalos.toml").unwrap();

        assert!(!confirmed);
    }

    #[test]
    fn confirm_overwrite_rejects_empty_input() {
        let mut stdin = Cursor::new(b"\n");
        let mut stdout = Vec::new();

        let confirmed = confirm_overwrite(&mut stdin, &mut stdout, ".kalos.toml").unwrap();

        assert!(!confirmed);
    }

    #[test]
    fn confirm_overwrite_writes_prompt_text() {
        let mut stdin = Cursor::new(b"y\n");
        let mut stdout = Vec::new();

        confirm_overwrite(&mut stdin, &mut stdout, ".kalos.toml").unwrap();

        let prompt = String::from_utf8(stdout).unwrap();
        assert!(prompt.contains("already exists. Overwrite? [y/N]"));
    }

    fn kalos_entry_count(contents: &str) -> usize {
        contents
            .lines()
            .filter(|line| line.trim() == KALOS_DIR_ENTRY)
            .count()
    }
}
