//! Claude CLI launches: the process environment and short control queries.
use agent_domain::{InteractionMode, RuntimeMode};
use agent_providers::{ClaudeLaunch, claude_environment, claude_runtime_query_policy};
use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
};

/// Credentials in the environment would override the selected account.
const CREDENTIAL_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
];

#[derive(Debug, Clone)]
pub(crate) struct ClaudeProgram {
    pub(crate) program: PathBuf,
    /// Settings and transcripts; credentials come from the selected account.
    pub(crate) config_home: PathBuf,
}

impl ClaudeProgram {
    /// The SDK's process environment for the selected account's credentials.
    pub(crate) fn environment(&self, credentials_home: &Path) -> BTreeMap<String, String> {
        let mut source: BTreeMap<String, String> = std::env::vars()
            .filter(|(key, _)| !CREDENTIAL_VARIABLES.contains(&key.as_str()))
            .collect();
        source.insert(
            "CLAUDE_CONFIG_DIR".into(),
            self.config_home.to_string_lossy().into_owned(),
        );
        source.insert(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
            credentials_home.to_string_lossy().into_owned(),
        );
        source.insert("CLAUDE_CODE_SDK_READS_SESSION_STATE".into(), "1".into());
        claude_environment(&source)
    }

    pub(crate) fn command(
        &self,
        args: &[String],
        credentials_home: &Path,
        cwd: &Path,
    ) -> std::io::Result<tokio::process::Command> {
        let mut command = bex_process::command(&self.program)?;
        command
            .args(args)
            .env_clear()
            .envs(self.environment(credentials_home))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }

    /// A short-lived native control query for provider-owned metadata; no
    /// inference input is sent.
    pub(crate) async fn query_control(
        &self,
        credentials_home: &Path,
        cwd: &Path,
        request: Option<(&str, Value)>,
    ) -> Result<Value, String> {
        let launch = ClaudeLaunch {
            model: "default".into(),
            policy: claude_runtime_query_policy(
                RuntimeMode::ApprovalRequired,
                InteractionMode::Default,
                None,
                None,
                false,
            ),
            native_session: None,
            new_session: Some(uuid::Uuid::new_v4().to_string()),
            resume_at: None,
            fork: false,
            additional_directories: vec![],
            effort: None,
            disallowed_tools: vec![],
            mcp_servers: BTreeMap::new(),
            settings: None,
            extra_args: BTreeMap::new(),
        };
        let mut child = self
            .command(&launch.args(), credentials_home, cwd)
            .map_err(|error| error.to_string())?
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut writer = JsonlWriter::new(child.stdin.take().ok_or("Claude stdin missing")?);
        let mut reader = JsonlReader::new(child.stdout.take().ok_or("Claude stdout missing")?);
        let error = |error: &dyn std::fmt::Display| error.to_string();
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let initialize = json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize"}});
            writer
                .write_line(&initialize.to_string())
                .await
                .map_err(|e| error(&e))?;
            let mut expected = "initialize";
            while let Some(line) = reader.read_line().await.map_err(|e| error(&e))? {
                let frame: Value = serde_json::from_str(&line).map_err(|e| error(&e))?;
                if frame["type"] != "control_response"
                    || frame["response"]["request_id"] != expected
                {
                    continue;
                }
                if frame["response"]["subtype"] != "success" {
                    return Err("Claude control query rejected".to_owned());
                }
                if expected == "initialize"
                    && let Some((id, body)) = &request
                {
                    let next = json!({"type":"control_request","request_id":id,"request":body});
                    writer
                        .write_line(&next.to_string())
                        .await
                        .map_err(|e| error(&e))?;
                    expected = id;
                    continue;
                }
                return Ok(frame["response"]["response"].clone());
            }
            Err("Claude closed before returning metadata".to_owned())
        })
        .await
        .map_err(|_| "Claude control query timed out".to_owned());
        drop(writer);
        drop(reader);
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        result?
    }
}
