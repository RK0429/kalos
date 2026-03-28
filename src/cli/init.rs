use std::env;
use std::fs;
use std::process::ExitCode;

use clap::Args;

use crate::domains::config::{CONFIG_FILE_NAME, render_default_config};

#[derive(Debug, Clone, Default, Args)]
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
            println!(
                "{} already exists. Overwrite prompt is not implemented yet.",
                CONFIG_FILE_NAME
            );
            return ExitCode::SUCCESS;
        }

        if let Err(error) = fs::write(&config_path, render_default_config()) {
            eprintln!("failed to write {}: {error}", config_path.display());
            return ExitCode::from(2);
        }

        println!("created {}", config_path.display());
        ExitCode::SUCCESS
    }
}
