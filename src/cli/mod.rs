pub mod check;
pub mod init;

use clap::{Parser, Subcommand};

use self::check::CheckCommand;
use self::init::InitCommand;

#[derive(Debug, Parser)]
#[command(
    name = "kalos",
    version,
    about = "Kalos CLI",
    long_about = None,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Check(CheckCommand),
    Init(InitCommand),
}

pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check(command) => command.execute(),
        Command::Init(command) => command.execute(),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::check::{MinimumSeverity, OutputFormat, RequestedLevel};
    use super::{Cli, Command};

    #[test]
    fn check_without_paths_defaults_to_workspace_root() {
        let cli = Cli::try_parse_from(["kalos", "check"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert_eq!(
            command.requested_paths(),
            vec![std::path::PathBuf::from(".")]
        );
        assert!(!command.targets_explicitly_specified());
        assert_eq!(command.format, OutputFormat::Human);
        assert_eq!(command.level, RequestedLevel::All);
    }

    #[test]
    fn check_with_explicit_paths_marks_targets_as_explicit() {
        let cli = Cli::try_parse_from(["kalos", "check", "src/", "tests/"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert_eq!(
            command.requested_paths(),
            vec![
                std::path::PathBuf::from("src/"),
                std::path::PathBuf::from("tests/")
            ]
        );
        assert!(command.targets_explicitly_specified());
    }

    #[test]
    fn check_parses_all_supported_flags() {
        let cli = Cli::try_parse_from([
            "kalos",
            "check",
            "src",
            "--format",
            "json",
            "--level",
            "module",
            "--config",
            "config/.kalos.toml",
            "--exclude",
            "vendor/**",
            "--exclude",
            "generated/**",
            "--severity",
            "warning",
            "--diff",
            "origin/main",
            "--llm",
            "--strict",
        ])
        .unwrap();

        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert_eq!(command.format, OutputFormat::Json);
        assert_eq!(command.level, RequestedLevel::Module);
        assert_eq!(
            command.config,
            Some(std::path::PathBuf::from("config/.kalos.toml"))
        );
        assert_eq!(
            command.exclude,
            vec!["vendor/**".to_owned(), "generated/**".to_owned()]
        );
        assert_eq!(command.severity, Some(MinimumSeverity::Warning));
        assert_eq!(command.diff.as_deref(), Some("origin/main"));
        assert!(command.llm);
        assert!(command.strict);
    }

    #[test]
    fn init_parses() {
        let cli = Cli::try_parse_from(["kalos", "init"]).unwrap();
        assert!(matches!(cli.command, Command::Init(_)));
    }
}
