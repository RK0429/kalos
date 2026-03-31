use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::Args;

use crate::domains::config::{CONFIG_FILE_NAME, render_default_config};

const KALOS_DIR_ENTRY: &str = ".kalos/";

#[derive(Debug, Clone, Default, Args)]
#[command(about = "create a default configuration file")]
pub struct InitCommand {}

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
            print!("{CONFIG_FILE_NAME} already exists. Overwrite? [y/N] ");
            if let Err(error) = io::stdout().flush() {
                eprintln!("failed to flush stdout: {error}");
                return ExitCode::from(2);
            }

            let mut input = String::new();
            if let Err(error) = io::stdin().read_line(&mut input) {
                eprintln!("failed to read overwrite confirmation: {error}");
                return ExitCode::from(2);
            }

            if !matches!(input.trim(), "y" | "Y") {
                println!("aborted");
                return ExitCode::SUCCESS;
            }
        }

        if let Err(error) = fs::write(&config_path, render_default_config()) {
            eprintln!("failed to write {}: {error}", config_path.display());
            return ExitCode::from(2);
        }

        println!("created {}", config_path.display());
        if let Err(error) = ensure_gitignore_entry(&cwd) {
            eprintln!("warning: failed to update .gitignore: {error}");
        }
        ExitCode::SUCCESS
    }
}

fn ensure_gitignore_entry(cwd: &std::path::Path) -> io::Result<()> {
    let gitignore_path = cwd.join(".gitignore");

    if gitignore_path.exists() {
        let contents = fs::read_to_string(&gitignore_path)?;
        let has_kalos_entry = contents.lines().any(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && trimmed == KALOS_DIR_ENTRY
        });

        if has_kalos_entry {
            return Ok(());
        }

        let mut updated_contents = contents;
        updated_contents.push('\n');
        updated_contents.push_str(KALOS_DIR_ENTRY);
        updated_contents.push('\n');
        fs::write(&gitignore_path, updated_contents)?;
        println!("added {KALOS_DIR_ENTRY} to .gitignore");
        return Ok(());
    }

    fs::write(&gitignore_path, format!("{KALOS_DIR_ENTRY}\n"))?;
    println!("created .gitignore with {KALOS_DIR_ENTRY} entry");
    Ok(())
}
