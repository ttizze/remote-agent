//! Android emulator IPC and the owned network-permission fixture.
use crate::{
    Result, args,
    supervision::{self, Child},
};
use std::{ffi::OsStr, fs, path::Path, time::Duration};
use tokio::process::Command;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
};

pub async fn console_port(path: &Path) -> Result<u16> {
    let cancel = supervision::cancellation();
    let connect = async {
        loop {
            match UnixStream::connect(path).await {
                Ok(mut connection) => {
                    let mut bytes = [0; 32];
                    let length = connection.read(&mut bytes).await?;
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                        std::str::from_utf8(&bytes[..length])?.trim().parse()?,
                    );
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => {
                    return Err(
                        format!("Emulator did not publish its console socket: {error}").into(),
                    );
                }
            }
        }
    };
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(30), connect) => result?,
        _ = supervision::cancelled(cancel.clone()) => Err(supervision::interrupted()),
    }
}

pub async fn network_permission(
    adb: &OsStr,
    serial: &str,
    log: &Path,
    server_port: &str,
) -> Result<()> {
    let cancel = supervision::cancellation();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    async fn serve(listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (mut connection, _) = listener.accept().await?;
            tokio::time::timeout(
                Duration::from_secs(5),
                connection.write_all(b"bex-os-network-ok\n"),
            )
            .await??;
        }
    }
    let mut command = Command::new(adb);
    command
        .args(args![
            "-P",
            server_port,
            "-s",
            serial,
            "shell",
            "am",
            "instrument",
            "-w",
            "-r",
            "-e",
            "networkPort",
            port.to_string(),
            "-e",
            "class",
            "dev.remoteagent.mobile.NetworkPermissionTest",
            "dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner"
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = Child::spawn(command)?;
    let output = tokio::select! {
        result = child.output(&cancel, Duration::from_secs(180)) => result?,
        result = serve(listener) => {
            child.stop(false).await?;
            result?;
            return Err("Android network fixture stopped".into());
        }
    };
    let mut bytes = output.stdout;
    bytes.extend(output.stderr);
    fs::write(log, &bytes)?;
    let output_text = String::from_utf8_lossy(&bytes);
    print!("{output_text}");
    if !output.status.success() || !output_text.lines().any(|line| line.trim() == "OK (1 test)") {
        return Err("Android tests did not all pass".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    #[tokio::test]
    async fn console_uses_the_emulators_owned_socket() {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let path = directory.path().join("console.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"5554\n").unwrap();
        });
        assert_eq!(console_port(&path).await.unwrap(), 5554);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn network_fixture_is_available_to_instrumentation_and_rejects_incomplete_results() {
        let fixture = Fixture::new().await;
        let root = fixture.project("adb").await;
        let log = root.join("network.log");
        let program = Fixture::executable(&root);
        for mode in ["pass", "failed", "missing"] {
            let result = network_permission(program.as_os_str(), "fixture", &log, mode).await;
            assert_eq!(result.is_ok(), mode == "pass");
            let output = fs::read_to_string(&log).unwrap();
            assert!(output.contains("bex-os-network-ok"));
        }
    }
}
