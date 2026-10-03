mod quality_background;

use std::process::ExitCode;
use xtask::Result;

const USAGE: &str = "Usage: cargo xtask <command>\n  quality-worker\n  quality-status [--wait]\n  clean-builds [--dry-run]\n  ios-e2e [--without-codex] TEST...\n  ios-markdown\n  connection-diagnostics [--log PATH] [--platform Ios|Macos] [--trace ID] [--attempt ID]\n  android-console SOCKET\n  android-network-permission SERIAL LOG ADB_PORT\n  terminal-query-probe\nBuilds and foreground checks: just --list\n";

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
        ("quality-worker", []) => quality_background::worker().await,
        ("quality-status", flags) if flags.is_empty() || flags == ["--wait"] => tokio::select! {
            result = quality_background::status(!flags.is_empty()) => result,
            signal = tokio::signal::ctrl_c() => signal.map_err(Into::into).and_then(|_| Err("interrupted".into())),
        },
        #[cfg(unix)]
        ("clean-builds", flags) if flags.is_empty() || flags == ["--dry-run"] => {
            xtask::build_cleanup::run(!flags.is_empty()).await
        }
        #[cfg(unix)]
        ("ios-markdown", []) => xtask::ios_markdown::run().await,
        #[cfg(unix)]
        ("terminal-query-probe", []) => xtask::terminal_probe::run(),
        #[cfg(unix)]
        ("ios-e2e", arguments) => {
            let without_codex = arguments
                .first()
                .is_some_and(|argument| argument == "--without-codex");
            xtask::ios_e2e::run(
                arguments[usize::from(without_codex)..].to_vec(),
                without_codex,
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
