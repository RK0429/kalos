fn main() -> std::process::ExitCode {
    kalos::platform::telemetry::init();
    kalos::cli::run()
}
