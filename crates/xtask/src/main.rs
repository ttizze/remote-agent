mod quality_background;

use std::process::ExitCode;
use xtask::Result;

const USAGE: &str = "Usage: cargo xtask <quality-worker|quality-status [--wait]>\nBuilds and foreground checks: just --list\n";

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
        ("quality-worker", []) => quality_background::worker().await,
        ("quality-status", []) => quality_background::status(false).await,
        ("quality-status", [flag]) if flag == "--wait" => quality_background::status(true).await,
        _ => Err(USAGE.into()),
    }
}
