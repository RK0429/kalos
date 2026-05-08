pub mod check;
pub mod init;

use clap::{Parser, Subcommand};

use self::check::CheckCommand;
use self::init::InitCommand;

#[derive(Debug, Parser)]
#[command(
    name = "kalos",
    version,
    about = "CPG-based code quality analysis tool",
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
    use clap::{Parser, error::ErrorKind};

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
        assert_eq!(command.output, None);
        assert_eq!(command.min_risk, None);
        assert!(!command.verbose);
        assert!(!command.quiet);
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
            "--output",
            "result.json",
            "--level",
            "module",
            "--config",
            "config/.kalos.toml",
            "--workspace-root",
            "workspace",
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
            "--min-risk",
            "0.5",
            "--codeql-ram",
            "4096",
            "--codeql-total-timeout",
            "600",
        ])
        .unwrap();

        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert_eq!(command.format, OutputFormat::Json);
        assert_eq!(
            command.output,
            Some(std::path::PathBuf::from("result.json"))
        );
        assert_eq!(command.level, RequestedLevel::Module);
        assert_eq!(
            command.config,
            Some(std::path::PathBuf::from("config/.kalos.toml"))
        );
        assert_eq!(
            command.workspace_root,
            Some(std::path::PathBuf::from("workspace"))
        );
        assert_eq!(
            command.exclude,
            vec!["vendor/**".to_owned(), "generated/**".to_owned()]
        );
        assert_eq!(command.severity, Some(MinimumSeverity::Warning));
        assert_eq!(command.diff.as_deref(), Some("origin/main"));
        assert_eq!(command.min_risk, Some(0.5));
        assert_eq!(command.codeql_ram, Some(4096));
        assert_eq!(command.codeql_total_timeout, Some(600));
        assert!(command.llm);
        assert!(command.strict);
        assert!(!command.update_gitignore);
    }

    #[test]
    fn check_parses_output_short_flag() {
        let cli = Cli::try_parse_from(["kalos", "check", "-o", "report.sarif"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert_eq!(
            command.output,
            Some(std::path::PathBuf::from("report.sarif"))
        );
    }

    #[test]
    fn check_parses_verbose_flag() {
        let cli = Cli::try_parse_from(["kalos", "check", "--verbose"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert!(command.verbose);
    }

    #[test]
    fn check_parses_quiet_flag_long() {
        let cli = Cli::try_parse_from(["kalos", "check", "--quiet"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert!(command.quiet);
    }

    #[test]
    fn check_parses_quiet_flag_short() {
        let cli = Cli::try_parse_from(["kalos", "check", "-q"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert!(command.quiet);
    }

    #[test]
    fn check_parses_update_gitignore_flag() {
        let cli = Cli::try_parse_from(["kalos", "check", "--update-gitignore"]).unwrap();
        let Command::Check(command) = cli.command else {
            panic!("expected check command");
        };

        assert!(command.update_gitignore);
    }

    #[test]
    fn init_parses() {
        let cli = Cli::try_parse_from(["kalos", "init"]).unwrap();
        assert!(matches!(cli.command, Command::Init(_)));
    }

    #[test]
    fn root_help_shows_tool_about_text() {
        let help = render_help(["kalos", "--help"]);

        assert!(help.contains("CPG-based code quality analysis tool"));
    }

    #[test]
    fn check_help_shows_about_and_argument_help_text() {
        let help = render_help(["kalos", "check", "--help"]);

        assert!(help.contains("run code quality analysis"));
        assert!(
            help.contains("target files or directories to analyze (defaults to workspace root)")
        );
        assert!(help.contains("output format"));
        assert!(help.contains("[default: human]"));
        assert!(help.contains("analysis granularity level"));
        assert!(help.contains("[default: all]"));
        assert!(help.contains("path to configuration file (.kalos.toml)"));
        assert!(help.contains("workspace root to resolve target paths against"));
        assert!(help.contains("glob patterns to exclude from analysis (repeatable)"));
        assert!(help.contains("minimum severity threshold for diagnostics (omit to show all)"));
        assert!(help.contains("git base ref for differential analysis"));
        assert!(help.contains("enable llm-assisted analysis"));
        assert!(help.contains("treat warnings as errors"));
        assert!(help.contains("KAL-PAT001 (god unit)"));
        assert!(help.contains("overflow ratio"));
        assert!(help.contains(".kalos.toml"));
        assert!(help.contains("show per-scope metrics in human output"));
        assert!(help.contains("maximum RAM in MiB passed to CodeQL"));
        assert!(help.contains("filter verbose metrics list by scope risk"));
        assert!(help.contains("write output to a file instead of stdout"));
        assert!(help.contains("suppress the stderr acknowledgment printed on --output success"));
        assert!(help.contains("add .kalos/ to .gitignore when missing"));
        assert!(help.contains("default: warn only"));
        assert!(
            help.contains("include test files in module-level diagnostics (KAL-M001, KAL-M003)")
        );
    }

    #[test]
    fn check_help_explains_default_suppression_for_include_tests() {
        let help = render_help(["kalos", "check", "--help"]);

        assert!(
            help.contains("include test files in module-level diagnostics (KAL-M001, KAL-M003)")
        );
        assert!(help.contains("KAL-M001 (module fan-out)"));
        assert!(help.contains("KAL-M003 (instability)"));
        assert!(help.contains("Test files are NOT excluded from analysis"));
        assert!(help.contains("Pass --include-tests to re-enable"));
        assert!(help.contains("no observable effect"));
        assert!(help.contains("Test files are detected by path"));
        assert!(help.contains("tests/**"));
        assert!(help.contains("__tests__/**"));
        assert!(help.contains("*_test.*"));
        assert!(help.contains("*.test.*"));
        assert!(help.contains("*.spec.*"));
        assert!(help.contains("test_*.py"));
    }

    #[test]
    fn init_help_shows_about_text() {
        let help = render_help(["kalos", "init", "--help"]);

        assert!(help.contains("create a default configuration file"));
        assert!(help.contains("--force"));
        assert!(help.contains("-f"));
        assert!(help.contains("--yes"));
        assert!(help.contains("-y"));
        assert!(help.contains("overwrite existing configuration without prompting"));
        assert!(help.contains(".kalos.toml"));
        assert!(help.contains(".gitignore"));
        assert!(help.contains(".kalos/"));
        assert!(help.to_lowercase().contains("overwrit"));
    }

    #[test]
    fn check_long_help_describes_min_risk_default() {
        let help = render_help(["kalos", "check", "--help"]);

        assert!(help.contains("Default: hide scopes with risk=0."));
        assert!(help.contains("Pass --min-risk 0 to include all scopes."));
    }

    fn render_help<const N: usize>(args: [&str; N]) -> String {
        let error = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        error.to_string()
    }
}
