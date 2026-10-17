use super::Config;
use agent_protocol::operations::CodexCliStatus;
use std::{process::Stdio, sync::Arc, time::Duration};

pub(super) struct Updater {
    config: Config,
    state: Arc<tokio::sync::Mutex<CodexCliStatus>>,
}

impl Updater {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            state: Arc::default(),
        }
    }

    // Return immediately so installation survives RPC deadlines and reconnects.
    // The shared state also prevents simultaneous updates from different clients.
    pub(super) async fn request(&self, update: bool) -> CodexCliStatus {
        let mut state = self.state.lock().await;
        if state.running {
            return state.clone();
        }
        if !update {
            // A bounded version read holds the lock so an update arriving during
            // the initial check is not accidentally treated as a duplicate update.
            // Refresh external CLI updates too, but preserve a failed update's
            // error until the user retries it (clients poll through this call).
            if state.error.is_none() {
                match execute(&self.config, false).await {
                    Ok(version) => state.version = Some(version),
                    Err(error) => state.error = Some(error),
                }
            }
            return state.clone();
        }
        state.running = true;
        state.error = None;
        let result = state.clone();
        let state = self.state.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let result = execute(&config, update).await;
            let mut state = state.lock().await;
            state.running = false;
            match result {
                Ok(version) => {
                    state.version = Some(version);
                    state.restart_required |= update;
                }
                Err(error) => state.error = Some(error),
            }
        });
        result
    }
}

async fn execute(config: &Config, update: bool) -> Result<String, String> {
    if update {
        run(config, "update", Duration::from_secs(300)).await.map_err(|error| {
            format!("Codex CLIの更新に失敗しました: {error}\nこのCLIが自動更新に対応していない場合は、インストール元の方法で更新してください。")
        })?;
    }
    run(config, "--version", Duration::from_secs(10)).await
}

async fn run(config: &Config, argument: &str, timeout: Duration) -> Result<String, String> {
    // Re-resolve after update: a standalone installation can repoint its launcher.
    let program =
        codex_app_server::resolve_executable(&config.program).map_err(|error| error.to_string())?;
    let mut command = bex_process::command(&program).map_err(|error| error.to_string())?;
    if let Some(home) = &config.codex_home {
        command.env("CODEX_HOME", home);
    }
    command.arg(argument);
    let result = collect(command, timeout).await?;
    if argument == "--version" && !result.starts_with("codex") {
        return Err("Codex CLI version could not be verified".into());
    }
    Ok(result)
}

async fn collect(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    // The supervisor's stdin is its lifetime pipe; keep it open while waiting.
    // Dropping it on timeout terminates the entire owned process tree.
    let lifetime = child.stdin.take();
    let output = tokio::time::timeout(timeout, child.wait_with_output()).await;
    drop(lifetime);
    let output = output
        .map_err(|_| "CLI command timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = if detail.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout)
        } else {
            detail
        };
        return Err(format!(
            "{}: {}",
            output.status,
            detail.chars().take(2000).collect::<String>().trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> tokio::process::Command {
        #[cfg(windows)]
        {
            let program = std::env::var_os("COMSPEC").unwrap();
            let mut command = bex_process::command(std::path::Path::new(&program)).unwrap();
            command.args(["/D", "/C", script]);
            command
        }
        #[cfg(unix)]
        {
            let mut command = bex_process::command(std::path::Path::new("/bin/sh")).unwrap();
            command.args(["-c", script]);
            command
        }
    }

    #[tokio::test]
    async fn supervised_commands_retain_lifetime_report_errors_and_time_out() {
        assert_eq!(
            collect(shell("echo codex-cli fixture"), Duration::from_secs(10))
                .await
                .unwrap(),
            "codex-cli fixture"
        );
        #[cfg(windows)]
        let failed = "echo update-denied 1>&2 & exit /b 7";
        #[cfg(unix)]
        let failed = "echo update-denied >&2; exit 7";
        let error = collect(shell(failed), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert!(error.contains("update-denied"), "{error}");
        #[cfg(windows)]
        let slow = {
            let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            let mut command = bex_process::command(&system.join("System32/ping.exe")).unwrap();
            command.args(["-n", "30", "127.0.0.1"]);
            command
        };
        #[cfg(unix)]
        let slow = shell("sleep 30");
        assert_eq!(
            collect(slow, Duration::from_millis(100))
                .await
                .unwrap_err(),
            "CLI command timed out"
        );
    }

    #[tokio::test]
    async fn missing_executable_finishes_with_error_and_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let updater = Updater::new(Config {
            program: root.path().join("missing-codex"),
            ..Default::default()
        });
        let (_, update) = tokio::join!(updater.request(false), updater.request(true));
        assert!(update.running);
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = updater.request(false).await;
                if !state.running {
                    break state;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            finished
                .error
                .as_deref()
                .unwrap()
                .contains("Codex CLIの更新に失敗")
        );
        assert!(!finished.restart_required);
        assert!(finished.version.is_none());
        assert!(updater.request(true).await.running);
    }

    #[tokio::test]
    async fn status_rechecks_the_executable_even_with_a_cached_version() {
        let root = tempfile::tempdir().unwrap();
        let updater = Updater::new(Config {
            program: root.path().join("missing-codex"),
            ..Default::default()
        });
        updater.state.lock().await.version = Some("codex-cli cached".into());
        let status = updater.request(false).await;
        assert!(status.error.is_some(), "must probe the current executable");
        assert!(!status.running);
        assert!(!status.restart_required);
    }

    #[tokio::test]
    async fn concurrent_request_does_not_restart_running_update() {
        let updater = Updater::new(Config::default());
        *updater.state.lock().await = CodexCliStatus {
            running: true,
            version: Some("codex-cli old".into()),
            ..Default::default()
        };
        let (first, second) = tokio::join!(updater.request(true), updater.request(true));
        assert_eq!(first, second);
        assert!(first.running);
        assert_eq!(first.version.as_deref(), Some("codex-cli old"));
    }
}
