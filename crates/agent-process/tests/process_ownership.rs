#![cfg(unix)]
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn provider_file_budget_survives_gui_limits_and_respects_the_hard_cap() {
    let mut inherited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut inherited) },
        0
    );
    for (soft, hard) in [
        (256, 4096),
        (256, 512),
        (4096, 8192),
        (8192, 8192),
        (256, inherited.rlim_max),
    ] {
        let hard = hard.min(inherited.rlim_max);
        let soft = soft.min(hard);
        let mut command = agent_process::command(Path::new("/bin/sh")).unwrap();
        command
            .args(["-c", "ulimit -Sn; ulimit -Hn"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // The fixture changes only the supervisor child, never the test runner.
        unsafe {
            command.pre_exec(move || {
                let limit = libc::rlimit {
                    rlim_cur: soft,
                    rlim_max: hard,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let lifetime = child.stdin.take();
        let output = child.wait_with_output().await.unwrap();
        drop(lifetime);
        assert!(output.status.success());
        let actual: Vec<libc::rlim_t> = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .map(|value| {
                if value == "unlimited" {
                    libc::RLIM_INFINITY
                } else {
                    value.parse().unwrap()
                }
            })
            .collect();
        assert_eq!(actual[1], hard);
        if hard == libc::RLIM_INFINITY {
            assert!((4096..=1 << 20).contains(&actual[0]));
        } else {
            assert_eq!(actual[0], hard);
        }
    }
}

#[tokio::test]
async fn non_interactive_programs_get_eof_without_ending_the_owners_lifetime() {
    let mut command = agent_process::non_interactive_command(Path::new("/bin/sh")).unwrap();
    let mut child = command
        .args([
            "-c",
            "if read value; then exit 7; fi; echo eof; sleep 1; echo done",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let lifetime = child.stdin.take();
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    drop(lifetime);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"eof\ndone\n");
}

#[tokio::test]
async fn lifetime_pipe_relays_input_and_preserves_native_exit_status() {
    let mut command = agent_process::command(Path::new("/bin/sh")).unwrap();
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

// GUI/update launch threads can block signals before exec. Both native exit
// and EOF cleanup must still finish, including on macOS's SIGCHLD reaper.
#[tokio::test]
async fn blocked_launcher_signals_do_not_stall_native_exit_or_eof_cleanup() {
    for (script, expected) in [
        ("read value; echo ready; exit 7", 7),
        ("read value; echo ready; kill -TERM $$; exit 9", 1),
        ("read value; echo ready; cat >/dev/null; sleep 30", 0),
    ] {
        let mut command = agent_process::command(Path::new("/bin/sh")).unwrap();
        // SAFETY: the child callback only builds a local signal set and uses
        // async-signal-safe POSIX functions before exec.
        unsafe {
            command.pre_exec(|| {
                let mut signals = std::mem::zeroed();
                libc::sigemptyset(&mut signals);
                libc::sigaddset(&mut signals, libc::SIGCHLD);
                libc::sigaddset(&mut signals, libc::SIGTERM);
                let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
                if error == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::from_raw_os_error(error))
                }
            });
        }
        let mut child = command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(b"start\n").await.unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line, "ready\n");
        let lifetime = if expected == 0 {
            drop(input);
            None
        } else {
            Some(input)
        };
        let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("inherited signal masks must not strand the supervisor")
            .unwrap();
        assert_eq!(status.code(), Some(expected));
        drop(lifetime);
    }
}

// The subprocess is an isolated Host stand-in. Kill it without running any
// destructors, then verify its known provider and tool child both disappear.
#[test]
fn abrupt_host_exit_terminates_owned_provider_and_tool() {
    if let Some(file) = std::env::var_os("AGENT_TEST_OWNED_PID_FILE") {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut command = agent_process::command(Path::new("/bin/sh")).unwrap();
            let _child = command
                .args([
                    "-c",
                    "sleep 120 & echo \"$$ $!\" > \"$AGENT_TEST_OWNED_PID_FILE\"; wait",
                ])
                .env("AGENT_TEST_OWNED_PID_FILE", file)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("owned-pids");
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_host_exit_terminates_owned_provider_and_tool",
            "--nocapture",
        ])
        .env("AGENT_TEST_OWNED_PID_FILE", &file)
        .stdout(Stdio::null())
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
                "owned process {pid} survived Host death"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn abrupt_host_exit_terminates_owned_pty_and_shell_jobs() {
    if let Some(file) = std::env::var_os("AGENT_TEST_PTY_PID_FILE") {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut child = agent_process::terminal_command().unwrap().spawn().unwrap();
            let command = agent_process::PtyCommand::Start {
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
            let write = agent_process::PtyCommand::Write {
                id: 1,
                data: b"[ -z \"${BASH_VERSION-}\" ] || set +H\nsleep 120 & first=$!; sleep 120 & printf '%s %s %s\\n' \"$$\" \"$first\" \"$!\" > \"$AGENT_TEST_PTY_PID_FILE\"; wait\n".to_vec(),
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
        .env("AGENT_TEST_PTY_PID_FILE", &file)
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
