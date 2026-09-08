mod command;
mod ios;
mod macos;

use std::process::ExitCode;
use xtask::{Result, repository_root};

const USAGE: &str = "Usage: cargo xtask <command>\n\nCommands:\n  build-host-macos       Build and verify the signed Host executable\n  build-desktop-macos    Build and verify target/Bex.app\n  ios-e2e [TEST ...]     Run isolated Simulator E2E tests (all by default)\n  relay-e2e             Run the real Phoenix transport and encrypted Host tests\n  relay-secrets         Emit Fly secrets using REMOTE_AGENT_RELAY_TOKEN\n";

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
    std::env::set_current_dir(repository_root())?;
    match (arguments[0].as_str(), &arguments[1..]) {
        ("build-host-macos", []) => macos::build_host().await,
        ("build-desktop-macos", []) => macos::build_desktop().await,
        ("ios-e2e", tests) => ios::run(tests).await,
        ("relay-secrets", []) => relay_secrets(),
        ("relay-e2e", []) => {
            command::run(command::cargo().args([
                "test",
                "--locked",
                "--package",
                "relay-transport",
                "--package",
                "xtask",
            ]))
            .await
        }
        _ => Err(USAGE.into()),
    }
}

fn relay_secrets() -> Result<()> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::rand::SecureRandom;
    use std::io::Write;
    use zeroize::Zeroizing;

    let token = Zeroizing::new(std::env::var("REMOTE_AGENT_RELAY_TOKEN")?);
    if token.is_empty() || token.len() > 512 || token.contains(['\r', '\n']) {
        return Err("Invalid relay token".into());
    }
    let mut secret = Zeroizing::new([0u8; 64]);
    ring::rand::SystemRandom::new()
        .fill(secret.as_mut())
        .map_err(|_| "Could not generate a secret key")?;
    let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.as_ref()));
    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "REMOTE_AGENT_RELAY_TOKEN={}\nSECRET_KEY_BASE={}",
        token.as_str(),
        encoded.as_str()
    )?;
    Ok(())
}
