use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn startup_errors_report_the_operation_and_io_cause() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    for (arguments, operation) in [
        (
            vec!["--stdio", missing.to_str().unwrap(), "list"],
            "cannot start stdio provider",
        ),
        (
            vec![
                "--ticket",
                "unused",
                "--identity-file",
                missing.to_str().unwrap(),
                "list",
            ],
            "cannot read client identity file",
        ),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-cli"))
                .args(arguments)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!("Error: {operation}: ")),
            "{stderr}"
        );
        assert!(
            stderr.contains("os error"),
            "underlying I/O cause is missing: {stderr}"
        );
    }
}
