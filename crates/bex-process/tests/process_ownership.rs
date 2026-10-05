#![cfg(unix)]
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn lifetime_pipe_relays_input_and_preserves_native_exit_status() {
    let mut command = bex_process::command(Path::new("/bin/sh")).unwrap();
    let mut child = command
        .args(["-c", "read value; echo \"$value\"; exit 7"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"native-input\n")
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    assert_eq!(line, "native-input\n");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .code(),
        Some(7)
    );
}

// The subprocess is an isolated Host stand-in. Kill it without running any
// destructors, then verify its known provider and tool child both disappear.
#[test]
fn abrupt_host_exit_terminates_owned_provider_and_tool() {
    if let Some(file) = std::env::var_os("BEX_TEST_OWNED_PID_FILE") {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut command = bex_process::command(Path::new("/bin/sh")).unwrap();
            let _child = command
                .args([
                    "-c",
                    "sleep 120 & echo \"$$ $!\" > \"$BEX_TEST_OWNED_PID_FILE\"; wait",
                ])
                .env("BEX_TEST_OWNED_PID_FILE", file)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        return;
    }
    provider_cleanup_after_host_exit(None);
}

#[test]
fn host_process_group_interrupt_preserves_supervisor_cleanup() {
    provider_cleanup_after_host_exit(Some(libc::SIGINT));
}

fn provider_cleanup_after_host_exit(signal: Option<i32>) {
    use std::os::unix::process::CommandExt;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("owned-pids");
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_host_exit_terminates_owned_provider_and_tool",
            "--nocapture",
        ])
        .env("BEX_TEST_OWNED_PID_FILE", &file)
        .stdout(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let started = Instant::now();
    let pids = loop {
        if let Ok(text) = std::fs::read_to_string(&file)
            && text.split_whitespace().count() == 2
        {
            break text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "owned provider did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    if let Some(signal) = signal {
        assert_eq!(unsafe { libc::kill(-(host.id() as i32), signal) }, 0);
    } else {
        host.kill().unwrap();
    }
    host.wait().unwrap();
    for pid in pids.split_whitespace() {
        loop {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "owned process {pid} survived Host death"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn abrupt_host_exit_terminates_owned_pty_and_shell_jobs() {
    if let Some(file) = std::env::var_os("BEX_TEST_PTY_PID_FILE") {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut child = bex_process::terminal_command().unwrap().spawn().unwrap();
            let command = bex_process::PtyCommand::Start {
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "exec \"${SHELL:-/bin/sh}\" -l".into(),
                ],
                cwd: Path::new(&file)
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                rows: 24,
                cols: 80,
            };
            let mut input = child.stdin().take().unwrap();
            let mut bytes = serde_json::to_vec(&command).unwrap();
            bytes.push(b'\n');
            input.write_all(&bytes).await.unwrap();
            let write = bex_process::PtyCommand::Write {
                id: 1,
                data: b"[ -z \"${BASH_VERSION-}\" ] || set +H\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > \"$BEX_TEST_PTY_PID_FILE\"; wait\n".to_vec(),
            };
            let mut bytes = serde_json::to_vec(&write).unwrap();
            bytes.push(b'\n');
            input.write_all(&bytes).await.unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
            drop(input);
        });
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("pty-pids");
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_host_exit_terminates_owned_pty_and_shell_jobs",
            "--nocapture",
        ])
        .env("BEX_TEST_PTY_PID_FILE", &file)
        .env("SHELL", "/bin/sh")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let pids = loop {
        if let Ok(text) = std::fs::read_to_string(&file)
            && text.split_whitespace().count() == 3
        {
            break text;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "PTY shell did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut groups = pids.split_whitespace().map(|pid| {
        let output = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", pid])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    });
    let shell_group = groups.next().unwrap();
    for group in groups {
        assert!(!group.is_empty());
        assert_ne!(
            shell_group, group,
            "fixture must use separate job-control groups"
        );
    }
    host.kill().unwrap();
    host.wait().unwrap();
    for pid in pids.split_whitespace() {
        loop {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "PTY process {pid} survived Host death"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
