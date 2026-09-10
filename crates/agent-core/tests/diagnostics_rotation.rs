#![cfg(unix)]

use agent_core::diagnostics;
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};

#[tokio::test]
async fn rotation_failure_does_not_deadlock_and_logging_recovers() {
    const FIXTURE: &str = "BEX_DIAGNOSTICS_ROTATION_FIXTURE";
    if let Some(directory) = std::env::var_os(FIXTURE) {
        let directory = std::path::PathBuf::from(directory);
        diagnostics::initialize(&directory, diagnostics::Component::Desktop, "rotation-test")
            .unwrap();
        let logs = directory.join("logs");
        let path = logs.join("desktop.jsonl");
        fs::write(&path, "{}\n".repeat(5 * 1024 * 1024 / 3 + 1)).unwrap();
        // Existing files remain writable, but file-rotate's directory scan fails.
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o300)).unwrap();
        let _restore = scopeguard::guard(logs.clone(), |logs| {
            fs::set_permissions(logs, fs::Permissions::from_mode(0o700)).unwrap();
        });
        assert!(
            fs::read_dir(&logs).is_err(),
            "fixture requires an unprivileged user"
        );
        diagnostics::error("rotation-failure", "must not hang the application");
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o700)).unwrap();
        diagnostics::error("recovered", "logging resumed");
        let record: serde_json::Value =
            serde_json::from_str(fs::read_to_string(path).unwrap().trim()).unwrap();
        assert_eq!(record["operation"], "recovered");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rotation_failure_does_not_deadlock_and_logging_recovers",
                "--nocapture",
            ])
            .env(FIXTURE, directory.path())
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    // A timed-out child may have been killed while the fixture denied listing.
    if directory.path().join("logs").exists() {
        fs::set_permissions(
            directory.path().join("logs"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let output = output
        .expect("logging must not deadlock during rotation failure")
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn closed_stderr_does_not_turn_a_log_failure_into_a_panic() {
    const FIXTURE: &str = "BEX_DIAGNOSTICS_STDERR_FIXTURE";
    if let Some(directory) = std::env::var_os(FIXTURE) {
        let directory = std::path::PathBuf::from(directory);
        diagnostics::initialize(&directory, diagnostics::Component::Desktop, "stderr-test")
            .unwrap();
        let path = directory.join("logs/desktop.jsonl");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        diagnostics::error("write-failure", "stderr is unavailable too");
        fs::remove_dir(&path).unwrap();
        diagnostics::error("recovered", "logging resumed");
        let record: serde_json::Value =
            serde_json::from_str(fs::read_to_string(path).unwrap().trim()).unwrap();
        assert_eq!(record["operation"], "recovered");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "closed_stderr_does_not_turn_a_log_failure_into_a_panic",
                "--nocapture",
            ])
            .env(FIXTURE, directory.path())
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(writer)))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout)
    );
}
