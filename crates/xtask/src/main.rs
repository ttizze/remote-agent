mod command;
mod ios;
mod macos;
mod quality;
mod quality_background;

use std::process::ExitCode;
use xtask::{Result, repository_root};

const USAGE: &str = "Usage: cargo xtask <command>\n\nCommands:\n  build-host-macos       Build and verify the signed Host executable\n  build-desktop-macos    Build and verify target/Bex.app\n  ios-e2e [TEST ...]     Run isolated Simulator E2E tests (all by default)\n  iroh-e2e              Run isolated iroh Host integration tests\n  quality [LANGUAGE]    Check rust, kotlin, swift (all by default)\n  quality-worker       Drain the post-commit quality queue\n  quality-status [--wait] Report quality for HEAD and worktree cleanliness\n";

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.is_empty() || matches!(arguments[0].as_str(), "--help" | "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = tokio::select! {
        result = execute(&arguments) => result,
        signal = tokio::signal::ctrl_c() => signal.map_err(Into::into).and_then(|_| Err("interrupted".into())),
    };
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
        ("quality-worker", []) => return quality_background::worker().await,
        ("quality-status", []) => return quality_background::status(false).await,
        ("quality-status", [flag]) if flag == "--wait" => {
            return quality_background::status(true).await;
        }
        _ => {}
    }
    std::env::set_current_dir(repository_root())?;
    match (arguments[0].as_str(), &arguments[1..]) {
        ("build-host-macos", []) => macos::build_host().await,
        ("build-desktop-macos", []) => macos::build_desktop().await,
        ("ios-e2e", tests) => ios::run(tests).await,
        ("quality", []) => quality::run(None).await,
        ("quality", [language]) => quality::run(Some(language)).await,
        ("iroh-e2e", []) => {
            command::run(command::cargo().args([
                "test",
                "--locked",
                "--package",
                "xtask",
                "--test",
                "iroh_host",
            ]))
            .await
        }
        _ => Err(USAGE.into()),
    }
}
