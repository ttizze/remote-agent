use serde_json::Value;
use std::{fs, process::Stdio, time::Duration};

#[tokio::test]
async fn fatal_startup_errors_survive_process_exit_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("trust.json"), b"invalid trust record").unwrap();
    let codex_home = directory.path().join("codex");
    fs::create_dir(&codex_home).unwrap();
    let missing_program = directory.path().join("missing-codex");
    let expected_error = "cannot load Host credentials: saved Host trust is invalid: expected value at line 1 column 1";
    for _ in 0..2 {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
                .args(["--isolated", "--no-relay", "--state-dir"])
                .arg(&state)
                .arg("--codex-home")
                .arg(&codex_home)
                .arg("--codex")
                .arg(&missing_program)
                .current_dir(directory.path())
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains(expected_error)
        );
    }
    let lines = fs::read_to_string(state.join("logs/host.jsonl")).unwrap();
    let records: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let failures: Vec<_> = records
        .iter()
        .filter(|v| v["operation"] == "host.runtime")
        .collect();
    assert_eq!(failures.len(), 2);
    assert_ne!(failures[0]["pid"], failures[1]["pid"]);
    assert!(
        failures
            .iter()
            .all(|v| v["level"] == "error" && v["message"] == expected_error)
    );
    assert_eq!(
        records
            .iter()
            .filter(|v| v["operation"] == "startup")
            .count(),
        2
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_event_stream_logs_the_stop_cause_without_conversation_payloads() {
    use std::os::unix::fs::PermissionsExt;

    tokio::time::timeout(Duration::from_secs(60), async {
        for (scenario, cause, detail) in [
            ("eof", "peer_closed", "JSONL stream reached EOF"),
            ("lag", "event_stream_lagged", "missed_events="),
            (
                "event",
                "event_processing_failed",
                "method=thread/name/updated error=renamed session ID is missing",
            ),
            ("shutdown", "peer_closed", "peer closed"),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let state = directory.path().join("state");
            let codex_home = directory.path().join("codex");
            fs::create_dir(&codex_home).unwrap();
            let program = directory.path().join("codex-fixture");
            // The lag scenario fills the real peer queue before replying to
            // initialize, so no Host consumer can drain the burst early.
            fs::write(
                &program,
                format!(
                    r#"#!/bin/sh
read -r initialize
if [ {} = lag ]; then
    awk 'BEGIN {{ for (i=0; i<10000; i++) print "{{\"method\":\"fixture/ignored\",\"params\":{{\"text\":\"PRIVATE_CONVERSATION_SENTINEL\"}}}}" }}'
fi
printf '%s\n' '{{"id":1,"result":{{"userAgent":"diagnostic-fixture","platformFamily":"unix","platformOs":"test","codexHome":"{}"}}}}'
read -r initialized
case {} in
    eof) exit 0 ;;
    event)
        printf '%s\n' '{{"method":"thread/name/updated","params":{{"text":"PRIVATE_CONVERSATION_SENTINEL"}}}}'
        ;;
esac
while read -r line; do :; done
"#,
                    scenario,
                    codex_home.display(),
                    scenario,
                ),
            )
            .unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            let mut host = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
                .args(["--isolated", "--no-relay", "--state-dir"])
                .arg(&state)
                .arg("--codex")
                .arg(&program)
                .arg("--claude")
                .arg(directory.path().join("missing-claude"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let host_pid = host.id().unwrap();
            let log = state.join("logs/host.jsonl");
            loop {
                assert!(host.try_wait().unwrap().is_none(), "Host exited: {scenario}");
                if scenario == "shutdown" {
                    if state.join("host.ticket").exists() {
                        assert!(
                            tokio::process::Command::new("kill")
                                .args(["-INT", &host_pid.to_string()])
                                .status()
                                .await
                                .unwrap()
                                .success()
                        );
                        assert!(host.wait().await.unwrap().success());
                        break;
                    }
                } else if fs::read_to_string(&log)
                    .unwrap_or_default()
                    .contains("host.codex.event_stream_stopped")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let lines = fs::read_to_string(&log).unwrap();
            assert!(!lines.contains("PRIVATE_CONVERSATION_SENTINEL"));
            let records: Vec<Value> = lines
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|record: &Value| record["operation"] == "host.codex.event_stream_stopped")
                .collect();
            assert_eq!(records.len(), 1, "{scenario}: {lines}");
            let record = &records[0];
            assert_eq!(record["pid"], host_pid);
            assert_eq!(record["level"], if scenario == "shutdown" { "info" } else { "error" });
            let message = record["message"].as_str().unwrap();
            assert!(message.contains(&format!("cause={cause} ")), "{message}");
            assert!(message.contains(detail), "{message}");
            assert!(message.contains("instance="), "{message}");
            let expected_sequence = match scenario {
                "lag" => 0,
                _ => 1,
            };
            assert!(message.contains(&format!("last_processed_sequence={expected_sequence} ")), "{message}");
            if scenario == "lag" {
                let missed: u64 = message.split("missed_events=").nth(1).unwrap().parse().unwrap();
                assert!(missed > 0, "{message}");
            }
            assert!(message.contains(&format!("shutdown_requested={}", scenario == "shutdown")), "{message}");
            if scenario != "shutdown" {
                host.kill().await.unwrap();
                host.wait().await.unwrap();
            }
        }
    })
    .await
    .expect("Codex stop diagnostic deadline");
}
