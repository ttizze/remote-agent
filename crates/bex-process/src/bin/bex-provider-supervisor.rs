#[path = "../pty.rs"]
mod pty;
// No history or provider protocol parsing: only owned process lifetimes.
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use std::process::Stdio;
use tokio::process::Command;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let code = if std::env::args().nth(1).as_deref() == Some("--bex-pty") {
        pty::run().await
    } else {
        run().await
    }
    .unwrap_or(1);
    // Tokio's stdin reader may still be blocked after a native process exits.
    // All owned children have already been killed/reaped before reaching here.
    std::process::exit(code);
}

async fn run() -> std::io::Result<i32> {
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or_else(|| std::io::Error::other("provider program is required"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    #[cfg(not(any(unix, windows)))]
    return Err(std::io::Error::other(
        "provider process ownership is unsupported on this OS",
    ));
    let mut child = command.wrap(KillOnDrop).spawn()?;
    let mut input = child
        .stdin()
        .take()
        .ok_or_else(|| std::io::Error::other("provider stdin unavailable"))?;
    let mut source = tokio::io::stdin();
    let finished = tokio::select! {
        result = child.wait() => Some(result),
        _ = tokio::io::copy(&mut source, &mut input) => None,
    };
    drop(input);
    let result = match finished {
        Some(result) => result.map(|status| status.code().unwrap_or(1)),
        None => {
            match tokio::time::timeout(std::time::Duration::from_millis(250), child.wait()).await {
                Ok(result) => result.map(|status| status.code().unwrap_or(1)),
                Err(_) => Ok(0),
            }
        }
    };
    // Also terminate remaining tool descendants after the main CLI exits.
    let _ = std::pin::Pin::from(child.kill()).await;
    result
}
