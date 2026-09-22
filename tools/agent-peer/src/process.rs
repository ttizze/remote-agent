use anyhow::{Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{io, process::Stdio, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    signal::unix::{SignalKind, signal},
    time::timeout,
};

pub async fn invoke(mut command: Command, prompt: &str, limit: Duration) -> Result<(String, bool)> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut child = command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let group = Pid::from_raw(child.id().expect("new child has a PID") as i32);
    let mut stdin = child.stdin.take().expect("piped stdin");
    let exchange = async {
        let send = async {
            let result = stdin.write_all(prompt.as_bytes()).await;
            drop(stdin);
            match result {
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
                other => other,
            }
        };
        let (_, output) = tokio::try_join!(send, child.wait_with_output())?;
        Ok::<_, io::Error>(output)
    };
    tokio::pin!(exchange);
    let reason = tokio::select! {
        result = timeout(limit, &mut exchange) => match result {
            Ok(Ok(output)) => return Ok((String::from_utf8(output.stdout)?, output.status.success())),
            Ok(Err(error)) => {
                let _ = killpg(group, Signal::SIGKILL);
                return Err(error.into());
            }
            Err(_) => "Timed out; the provider may have saved a partial turn. No automatic retry.",
        },
        _ = interrupt.recv() => "Interrupted; no automatic retry.",
        _ = terminate.recv() => "Interrupted; no automatic retry.",
    };
    let _ = killpg(group, Signal::SIGTERM);
    let finished = timeout(Duration::from_secs(5), &mut exchange).await.is_ok();
    let _ = killpg(group, Signal::SIGKILL);
    if !finished {
        let _ = exchange.await;
    }
    bail!(reason)
}
