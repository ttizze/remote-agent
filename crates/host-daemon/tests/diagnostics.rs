use serde_json::Value;
use std::{fs, process::Stdio, time::Duration};

#[tokio::test]
async fn fatal_startup_errors_survive_process_exit_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let codex_home = directory.path().join("codex");
    fs::create_dir(&codex_home).unwrap();
    let missing_program = directory.path().join("missing-codex");
    let expected_error = agent_core::diagnostics::sanitize(
        &codex_app_server::Error::ResolveExecutable {
            program: missing_program.clone(),
            source: fs::canonicalize(&missing_program).unwrap_err(),
        }
        .to_string(),
    );
    for _ in 0..2 {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
                .args(["--key-storage", "file", "--no-relay", "--state-dir"])
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
