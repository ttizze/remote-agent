use super::Config;
use agent_protocol::operations::CodexCliStatus;
use std::{process::Stdio, sync::Arc, time::Duration};

pub(super) struct Cli {
    config: Config,
    state: Arc<tokio::sync::Mutex<CodexCliStatus>>,
}

impl Cli {
    pub(super) async fn generate_title(
        &self,
        prompt: &str,
        mcp_servers: &[String],
    ) -> Result<String, crate::providers::Failure> {
        use crate::providers::{Failure, text_generation};
        let directory =
            tempfile::tempdir().map_err(|error| Failure::new("title_generation_failed", error))?;
        let schema = directory.path().join("schema.json");
        let output = directory.path().join("title.json");
        std::fs::write(&schema, text_generation::schema().to_string())
            .map_err(|error| Failure::new("title_generation_failed", error))?;
        let program = codex_app_server::resolve_executable(&self.config.program)?;
        let mut command = agent_process::command(&program)
            .map_err(|error| Failure::new("title_generation_failed", error))?;
        if let Some(home) = &self.config.codex_home {
            command.env("CODEX_HOME", home);
        }
        // An empty cwd keeps repository instructions out of this text-only task.
        // Seed disabled entries before launch overrides so their transport
        // definitions survive. An empty table would merge with user config.
        let mcp_off = (!mcp_servers.is_empty()).then(|| {
            let entries: Vec<_> = mcp_servers
                .iter()
                .map(|name| {
                    format!(
                        "{}={{enabled=false}}",
                        serde_json::to_string(name).expect("name serializes")
                    )
                })
                .collect();
            format!("mcp_servers={{{}}}", entries.join(","))
        });
        if let Some(off) = &mcp_off {
            command.arg("-c").arg(off);
        }
        for value in &self.config.config_overrides {
            command.arg("-c").arg(value);
        }
        if self.config.config_overrides.iter().any(|value| {
            value
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "mcp_servers")
        }) && let Some(off) = &mcp_off
        {
            command.arg("-c").arg(off);
        }
        // Codex splits dotted keys literally. The table above handles names
        // containing dots or equals signs; ordinary entries can be disabled last.
        for name in mcp_servers.iter().filter(|name| !name.contains(['.', '='])) {
            command
                .arg("-c")
                .arg(format!("mcp_servers.{name}.enabled=false"));
        }
        command
            .current_dir(directory.path())
            .args([
                "exec",
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--model",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                "features.plugins=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.shell_tool=false",
                "--output-schema",
            ])
            .arg(schema)
            .arg("--output-last-message")
            .arg(&output);
        text_generation::run(command, prompt).await?;
        let output = std::fs::read_to_string(output).map_err(|_| {
            Failure::new(
                "title_generation_failed",
                "Title output file is unavailable",
            )
        })?;
        text_generation::title(&output)
    }

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
    let mut command = agent_process::command(&program).map_err(|error| error.to_string())?;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn title_cli_uses_structured_ephemeral_inference_and_keeps_its_supervisor_alive() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("codex");
        std::fs::write(&program, r#"#!/usr/bin/env node
const fs = require('fs');
const args = process.argv.slice(2);
const value = flag => args[args.indexOf(flag) + 1];
if (!args.includes('exec') || !args.includes('--ephemeral') || value('--sandbox') !== 'read-only') process.exit(2);
if (value('--model') !== 'gpt-6-luna' || !process.env.CODEX_HOME.endsWith('account-home')) process.exit(3);
if (!args.includes('features.plugins=false') || !args.includes('features.apps=false') || !args.includes('custom="preserved"')) process.exit(4);
if (!args.includes('mcp_servers={"my.server"={enabled=false},"normal"={enabled=false}}') || args.lastIndexOf('mcp_servers.normal.enabled=false') < args.indexOf('mcp_servers.normal.enabled=true')) process.exit(5);
if (args.at(-1) !== 'Japanese task request') process.exit(6);
const schema = JSON.parse(fs.readFileSync(value('--output-schema'), 'utf8'));
if (schema.additionalProperties !== false || schema.required[0] !== 'title') process.exit(7);
setTimeout(() => { fs.writeFileSync(value('--output-last-message'), JSON.stringify({title:'内容からタスク名を生成'})); }, 400);
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cli = Cli::new(Config {
            program,
            codex_home: Some(directory.path().join("account-home")),
            config_overrides: vec![
                "custom=\"preserved\"".into(),
                "mcp_servers.normal.enabled=true".into(),
            ],
        });
        assert_eq!(
            cli.generate_title(
                "Japanese task request",
                &["my.server".into(), "normal".into()]
            )
            .await
            .unwrap(),
            "内容からタスク名を生成"
        );
    }

    fn shell(script: &str) -> tokio::process::Command {
        #[cfg(windows)]
        {
            let program = std::env::var_os("COMSPEC").unwrap();
            let mut command = agent_process::command(std::path::Path::new(&program)).unwrap();
            command.args(["/D", "/C", script]);
            command
        }
        #[cfg(unix)]
        {
            let mut command = agent_process::command(std::path::Path::new("/bin/sh")).unwrap();
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
            let mut command = agent_process::command(&system.join("System32/ping.exe")).unwrap();
            // GUI-launched Hosts may not inherit a PATH containing System32.
            // The timeout fixture must still start rather than fail immediately.
            command.env("PATH", "");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        };
        #[cfg(unix)]
        let slow = shell("sleep 30");
        assert_eq!(
            collect(slow, Duration::from_millis(100)).await.unwrap_err(),
            "CLI command timed out"
        );
    }

    #[tokio::test]
    async fn missing_executable_finishes_with_error_and_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let updater = Cli::new(Config {
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
        let updater = Cli::new(Config {
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
        let updater = Cli::new(Config::default());
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
