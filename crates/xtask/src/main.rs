use std::process::ExitCode;
use xtask::Result;

const USAGE: &str = "Usage: cargo xtask <command>\n  ios-e2e [--build-only|--prebuilt|--await-build] [--maestro] [--without-codex] TEST...\n  nsh-acceptance [SCENARIO-FILTER...]\n  nsh-parity [--compare RUN] [STATE-FILTER...]\n  connection-diagnostics [--log PATH] [--platform Ios|Macos] [--trace ID] [--attempt ID]\n  android-console SOCKET\n  android-network-permission SERIAL LOG ADB_PORT\n  terminal-query-probe\nBuilds and manual checks: just --list\n";

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.is_empty() || matches!(arguments[0].as_str(), "--help" | "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = execute(&arguments).await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn execute(arguments: &[String]) -> Result<()> {
    match (arguments[0].as_str(), &arguments[1..]) {
        #[cfg(unix)]
        ("terminal-query-probe", []) => xtask::terminal_probe::run(),
        #[cfg(unix)]
        ("nsh-acceptance", filters) => xtask::nsh_acceptance::run(filters).await,
        #[cfg(unix)]
        ("nsh-parity", arguments) => xtask::nsh_parity::run(arguments).await,
        #[cfg(unix)]
        ("ios-e2e", arguments) => {
            let (build_mode, arguments) = match arguments.first().map(String::as_str) {
                Some("--build-only") => (xtask::ios_e2e::BuildMode::BuildOnly, &arguments[1..]),
                Some("--prebuilt") => (xtask::ios_e2e::BuildMode::Prebuilt, &arguments[1..]),
                Some("--await-build") => (xtask::ios_e2e::BuildMode::AwaitBuild, &arguments[1..]),
                _ => (xtask::ios_e2e::BuildMode::BuildAndTest, arguments),
            };
            let (driver, arguments) = if arguments
                .first()
                .is_some_and(|argument| argument == "--maestro")
            {
                (xtask::ios_e2e::TestDriver::Maestro, &arguments[1..])
            } else {
                (xtask::ios_e2e::TestDriver::XCTest, arguments)
            };
            let without_codex = arguments
                .first()
                .is_some_and(|argument| argument == "--without-codex");
            xtask::ios_e2e::run(
                arguments[usize::from(without_codex)..].to_vec(),
                without_codex,
                driver,
                build_mode,
            )
            .await
        }
        ("connection-diagnostics", arguments) => xtask::connection_diagnostics::run(arguments),
        #[cfg(unix)]
        ("android-console", [path]) => {
            println!(
                "{}",
                xtask::android_e2e::console_port(std::path::Path::new(path)).await?
            );
            Ok(())
        }
        #[cfg(unix)]
        ("android-network-permission", [serial, log, port]) => {
            xtask::android_e2e::network_permission(
                std::ffi::OsStr::new("adb"),
                serial,
                std::path::Path::new(log),
                port,
            )
            .await
        }
        _ => Err(USAGE.into()),
    }
}
