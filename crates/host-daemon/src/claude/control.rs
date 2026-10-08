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
const OWNED_ENVIRONMENT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CODE_SDK_READS_SESSION_STATE",
    "NODE_OPTIONS",
    "DEBUG_CLAUDE_AGENT_SDK",
];

fn claude_environment(
    source: impl IntoIterator<Item = (String, String)>,
    config_home: &Path,
    credentials_home: &Path,
) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = source
        .into_iter()
        .filter(|(key, _)| {
            !OWNED_ENVIRONMENT
                .iter()
                .any(|name| name.eq_ignore_ascii_case(key))
        })
        .collect();
    environment.insert(
        "CLAUDE_CONFIG_DIR".into(),
        config_home.to_string_lossy().into_owned(),
    );
    environment.insert(
        "CLAUDE_SECURESTORAGE_CONFIG_DIR".into(),
        credentials_home.to_string_lossy().into_owned(),
    );
    environment.insert("CLAUDE_CODE_SDK_READS_SESSION_STATE".into(), "1".into());
    environment
}

#[derive(Debug, Clone)]
pub(crate) struct ClaudeProgram {
    pub(crate) program: PathBuf,
    /// Settings and transcripts; credentials come from the selected account.
    pub(crate) config_home: PathBuf,
    /// Provider-instance environment, layered into the SDK process.
    pub(crate) environment: BTreeMap<String, String>,
    /// Provider-specific arguments passed to the Claude CLI by the SDK bridge
    /// and by one-shot text generation.
    pub(crate) launch_args: Vec<String>,
}

impl ClaudeProgram {
    /// The SDK's process environment for the selected account's credentials.
    pub(crate) fn environment(&self, credentials_home: &Path) -> BTreeMap<String, String> {
        let mut source: BTreeMap<String, String> = std::env::vars().collect();
        source.extend(self.environment.clone());
        claude_environment(source, &self.config_home, credentials_home)
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
            .args(&self.launch_args)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_account_paths_replace_case_variants_and_injected_options() {
        let source = [
            ("PATH", "fixture-path"),
            ("Anthropic_Api_Key", "fixture-credential"),
            ("claude_code_oauth_token", "fixture-token"),
            ("Node_Options", "--require fixture"),
            ("Debug_Claude_Agent_Sdk", "1"),
            ("Claude_Config_Dir", "wrong-config"),
            ("Claude_Securestorage_Config_Dir", "wrong-account"),
            ("claude_code_sdk_reads_session_state", "0"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()));
        assert_eq!(
            claude_environment(source, Path::new("config"), Path::new("account")),
            BTreeMap::from([
                ("PATH".into(), "fixture-path".into()),
                ("CLAUDE_CONFIG_DIR".into(), "config".into()),
                ("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), "account".into()),
                ("CLAUDE_CODE_SDK_READS_SESSION_STATE".into(), "1".into()),
            ]),
        );
    #[test]
    fn instance_environment_overrides_safe_values_and_protects_session_paths() {
        let program = ClaudeProgram {
            program: PathBuf::from("claude"),
            config_home: PathBuf::from("/tmp/provider-config"),
            environment: BTreeMap::from([
                ("PROVIDER_MODE".into(), "work".into()),
                ("CLAUDE_CONFIG_DIR".into(), "/tmp/ignored".into()),
                ("NODE_OPTIONS".into(), "--require=ignored".into()),
            ]),
            launch_args: vec!["--verbose".into()],
        };
        let environment = program.environment(PathBuf::from("/tmp/provider-credentials").as_path());
        assert_eq!(environment.get("PROVIDER_MODE"), Some(&"work".to_owned()));
        assert_eq!(
            environment.get("CLAUDE_CONFIG_DIR"),
            Some(&"/tmp/provider-config".to_owned())
        );
        assert_eq!(
            environment.get("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Some(&"/tmp/provider-credentials".to_owned())
        );
        assert!(!environment.contains_key("NODE_OPTIONS"));
    }
}
