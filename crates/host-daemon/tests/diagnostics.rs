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
