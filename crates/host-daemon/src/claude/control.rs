//! Claude CLI launches: the process environment and short control queries.
use agent_domain::{InteractionMode, RuntimeMode};
use agent_providers::{ClaudeLaunch, claude_runtime_query_policy};
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
        source.remove("NODE_OPTIONS");
        source
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
        request: Option<Value>,
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
        let mut requests = vec![json!({"subtype":"initialize", "options":launch.sdk_options()})];
        if let Some(request) = request {
            requests.push(request);
        }
        self.sdk_requests(credentials_home, cwd, requests)
            .await
            .map_err(|error| error.to_string())
    }
}
