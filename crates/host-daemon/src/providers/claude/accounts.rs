//! Credentials stay in Claude Code's own storage. Only account labels and the
//! selection are persisted here. Conversation settings and transcripts stay in
//! the native home; only secure credential storage changes with the account.
use crate::providers::{AccountCommand, AccountReply};
use agent_protocol::{
    models::Empty,
    operations::{Account, AccountLogin, AccountLoginStatus, AccountSelection},
    session::ProviderKind,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin},
    sync::oneshot,
    task::JoinHandle,
};

const EXPIRED_LOGIN: &str = "Claude のログインが期限切れになりました。もう一度お試しください。";

#[derive(Debug)]
pub(super) enum Error {
    Operation(String),
    LoginEnded(String),
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Operation(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Operation(message.into())
    }
}

impl From<Error> for crate::providers::Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::Operation(message) => Self::new("account_operation_failed", message),
            Error::LoginEnded(message) => {
                Self::new(agent_protocol::error::ACCOUNT_LOGIN_ENDED, message)
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    accounts: Vec<Account>,
    selected_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Info {
    logged_in: bool,
    auth_method: String,
    email: Option<String>,
    subscription_type: Option<String>,
}

impl Info {
    fn account(self, id: String) -> Option<Account> {
        let plan = self.subscription_type.filter(|plan| !plan.is_empty())?;
        if !self.logged_in || self.auth_method != "claude.ai" {
            return None;
        }
        Some(Account {
            id,
            provider: ProviderKind::Claude,
            email: self.email,
            plan_type: Some(plan),
            usage: None,
        })
    }

    fn logged_out(&self) -> bool {
        !self.logged_in && self.auth_method == "none"
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            selected_id: Some("claude:desktop".into()),
        }
    }
}

pub(crate) struct Accounts {
    program: PathBuf,
    directory: PathBuf,
    native_home: PathBuf,
    native_inherited: bool,
    registry: Registry,
    revision: u64,
    native_checked_at: Option<Instant>,
    login: Option<Login>,
    usage: crate::account_usage::UsageCache,
}
struct Login {
    started: Instant,
    id: String,
    home: PathBuf,
    state: String,
    process: Cli,
}
struct Cli {
    child: Child,
    input: Option<ChildStdin>,
    output: JoinHandle<Result<Vec<u8>, String>>,
}
impl Drop for Cli {
    fn drop(&mut self) {
        // EOF tells the supervisor to terminate the whole process group.
        self.input.take();
        self.output.abort();
    }
}

impl Accounts {
    pub(super) async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: PathBuf,
        native_inherited: bool,
    ) -> anyhow::Result<Self> {
        agent_local_host::create_state_directory(&directory)?;
        let registry = match tokio::fs::read(directory.join("accounts.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            program,
            directory,
            native_home,
            native_inherited,
            registry,
            revision: 0,
            native_checked_at: None,
            login: None,
            usage: Default::default(),
        })
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn selected_home(&self) -> Result<Option<PathBuf>, String> {
        match self.registry.selected_id.as_deref() {
            Some("claude:desktop") => Ok(Some(self.native_home.clone())),
            Some(id) => self.account_home(id).map(Some),
            None => Ok(None),
        }
    }

    fn account_home(&self, id: &str) -> Result<PathBuf, String> {
        if !self
            .registry
            .accounts
            .iter()
            .any(|account| account.id == id)
        {
            return Err("Claude アカウントが見つかりません。".into());
        }
        if id == "claude:desktop" {
            return Ok(self.native_home.clone());
        }
        let uuid = id
            .strip_prefix("claude:")
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or("Claude アカウントIDが無効です。")?;
        Ok(self.directory.join(uuid.to_string()))
    }

    pub(super) fn terminal(&self, id: &str) -> Result<crate::terminals::Launch, String> {
        let home = self.account_home(id)?;
        let path = std::env::var_os("PATH");
        let program = super::process::executable(&self.program, path.as_deref())?;
        // Setup and status use the same account-specific config and credentials.
        let scoped = super::process::homes(&self.native_home, self.native_inherited, &home)
            .map(|(_, home)| home);
        let environment = authentication_environment(scoped);
        Ok(crate::terminals::Launch {
            directory: home,
            arguments: [
                program.to_string_lossy().into_owned(),
                "--safe-mode".into(),
                "--tools".into(),
                String::new(),
                "--permission-mode".into(),
                "dontAsk".into(),
            ]
            .into(),
            environment,
        })
    }

    async fn save(&self) -> Result<(), String> {
        let path = self.directory.join("accounts.json");
        let value = self.registry.clone();
        tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&path, &value))
            .await
            .map_err(|_| "Claude アカウントを保存できません。")?
            .map_err(|_| "Claude アカウントを保存できません。".into())
    }

    fn info(&self, home: &Path) -> impl Future<Output = Result<Info, String>> + use<> {
        let program = self.program.clone();
        let home = super::process::homes(&self.native_home, self.native_inherited, home)
            .map(|(_, home)| home.to_owned());
        async move {
            // Authentication status does not start an agent, load project
            // settings, fetch usage, or require interactive onboarding consent.
            let mut cli = Cli::start(
                &program,
                home.as_deref(),
                &["auth", "status", "--json"],
                None,
            )?;
            let output = cli.finish().await?;
            let info: Info = serde_json::from_slice(&output.stdout)
                .map_err(|_| "Claude の認証状態を読み取れません。".to_owned())?;
            if !output.status.success() && !info.logged_out() {
                return Err("Claude の認証状態を確認できません。".into());
            }
            Ok(info)
        }
    }

    pub(super) fn native_refresh(
        &self,
    ) -> Option<(u64, impl Future<Output = Result<Info, String>> + use<>)> {
        self.native_checked_at
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
            .then(|| (self.revision, self.info(&self.native_home)))
    }

    pub(super) fn accept_native(&mut self, revision: u64, info: Info) {
        // An explicit account change wins over a discovery started before it.
        if self.revision != revision {
            return;
        }
        let native = info.account("claude:desktop".into());
        let previous = self
            .registry
            .accounts
            .iter()
            .find(|account| account.id == "claude:desktop");
        if previous.and_then(|account| account.email.as_deref())
            != native.as_ref().and_then(|account| account.email.as_deref())
        {
            self.usage.remove("claude:desktop");
        }
        self.registry
            .accounts
            .retain(|account| account.id != "claude:desktop");
        if let Some(account) = native {
            self.registry.accounts.insert(0, account);
        }
        self.native_checked_at = Some(Instant::now());
    }

    pub(crate) fn list(&self) -> (Vec<Account>, Option<String>) {
        let selected = self
            .registry
            .selected_id
            .as_ref()
            .filter(|id| {
                self.registry
                    .accounts
                    .iter()
                    .any(|account| &account.id == *id)
            })
            .cloned();
        (self.registry.accounts.clone(), selected)
    }

    pub(crate) fn usage_request(
        &mut self,
        id: &str,
    ) -> Result<
        impl std::future::Future<Output = agent_protocol::operations::AccountUsage> + use<>,
        String,
    > {
        let home = self.account_home(id)?;
        let program = self.program.clone();
        let homes = super::process::homes(&self.native_home, self.native_inherited, &home)
            .map(|(config, credentials)| (config.to_owned(), credentials.to_owned()));
        let directory = self.directory.clone();
        let cache = self.usage.entry(id.to_owned()).or_default().clone();
        Ok(async move {
            cache
                .read(async {
                    super::process::Process::probe(
                        &program,
                        homes
                            .as_ref()
                            .map(|(config, credentials)| (config.as_path(), credentials.as_path())),
                        &directory,
                    )
                    .await?
                    .usage
                    .map(|usage| super::usage::windows(&usage))
                })
                .await
        })
    }

    pub(super) async fn request(&mut self, command: AccountCommand) -> Result<AccountReply, Error> {
        match command {
            AccountCommand::Select { id } => {
                let home = self.account_home(&id)?;
                self.info(&home)
                    .await?
                    .account(id.clone())
                    .ok_or("Claude に再ログインしてください。")?;
                let previous = self.registry.selected_id.replace(id.clone());
                if let Err(error) = self.save().await {
                    self.registry.selected_id = previous;
                    return Err(error.into());
                }
                self.revision += 1;
                Ok(AccountSelection {
                    provider: ProviderKind::Claude,
                    selected_id: id,
                    persistence_error: None,
                }
                .into())
            }
            AccountCommand::Logout { id } => {
                if id == "claude:desktop" {
                    self.native_checked_at = None;
                }
                self.usage.remove(&id);
                let home = self.account_home(&id)?;
                if self.registry.selected_id.as_ref() == Some(&id) {
                    self.registry.selected_id = None;
                    if let Err(error) = self.save().await {
                        self.registry.selected_id = Some(id);
                        return Err(error.into());
                    }
                }
                self.revision += 1;
                let mut process = Cli::start(
                    &self.program,
                    super::process::homes(&self.native_home, self.native_inherited, &home)
                        .map(|(_, home)| home),
                    &["auth", "logout"],
                    None,
                )?;
                if !process.finish().await?.status.success() {
                    return Err("Claude のログアウトに失敗しました。".into());
                }
                self.registry.accounts.retain(|account| account.id != id);
                self.save().await?;
                Ok(Empty {}.into())
            }
            AccountCommand::StartLogin => {
                self.cancel().await?;
                let id = format!("claude:{}", uuid::Uuid::new_v4());
                let home = self.directory.join(id.strip_prefix("claude:").unwrap());
                agent_local_host::create_state_directory(&home)
                    .map_err(|_| "Claude の保存先を作成できません。")?;
                let (sender, url) = oneshot::channel();
                let process = Cli::start(
                    &self.program,
                    Some(&home),
                    &["auth", "login", "--claudeai"],
                    Some(sender),
                )?;
                self.login = Some(Login {
                    started: Instant::now(),
                    id: id.clone(),
                    home,
                    state: String::new(),
                    process,
                });
                let result = tokio::time::timeout(Duration::from_secs(30), url)
                    .await
                    .map_err(|_| "Claude のログイン開始がタイムアウトしました。".to_owned())
                    .and_then(|result| {
                        result.map_err(|_| "Claude のログインを開始できません。".to_owned())
                    });
                let (verification_url, state) = match result {
                    Ok(url) => url,
                    Err(error) => {
                        self.cancel().await?;
                        return Err(error.into());
                    }
                };
                self.login.as_mut().unwrap().state = state;
                Ok(AccountLogin {
                    provider: ProviderKind::Claude,
                    login_id: id,
                    user_code: String::new(),
                    verification_url,
                    requires_code_submission: true,
                }
                .into())
            }
            AccountCommand::SubmitLogin { id, code } => {
                let login = self
                    .login
                    .as_mut()
                    .filter(|login| login.id == id)
                    .ok_or_else(|| Error::LoginEnded(EXPIRED_LOGIN.into()))?;
                let code = zeroize::Zeroizing::new(code);
                let code = code.trim();
                let expected_state = login.state.as_str();
                if code.len() > 4096
                    || code.contains(['\r', '\n'])
                    || !code
                        .split_once('#')
                        .is_some_and(|(code, state)| !code.is_empty() && state == expected_state)
                {
                    return Err("認証コードが無効です。".into());
                }
                let input = login
                    .process
                    .input
                    .as_mut()
                    .ok_or("ログインが終了しました。")?;
                input
                    .write_all(code.as_bytes())
                    .await
                    .map_err(|_| "認証コードを送信できません。")?;
                input
                    .write_all(b"\n")
                    .await
                    .map_err(|_| "認証コードを送信できません。")?;
                input
                    .flush()
                    .await
                    .map_err(|_| "認証コードを送信できません。")?;
                Ok(Empty {}.into())
            }
            AccountCommand::ReadLogin { id } => {
                if self
                    .registry
                    .accounts
                    .iter()
                    .any(|account| account.id == id)
                {
                    return Ok(AccountLoginStatus {
                        completed: true,
                        account_id: Some(id),
                    }
                    .into());
                }
                let login = self
                    .login
                    .as_mut()
                    .filter(|login| login.id == id)
                    .ok_or_else(|| Error::LoginEnded(EXPIRED_LOGIN.into()))?;
                if login.started.elapsed() > Duration::from_secs(600) {
                    self.cancel().await?;
                    return Err(Error::LoginEnded(EXPIRED_LOGIN.into()));
                }
                let Some(status) = login
                    .process
                    .child
                    .try_wait()
                    .map_err(|_| "Claude の認証状態を確認できません。")?
                else {
                    return Ok(AccountLoginStatus {
                        completed: false,
                        account_id: None,
                    }
                    .into());
                };
                if !status.success() {
                    self.cancel().await?;
                    return Err(Error::LoginEnded(
                        "Claude のログインに失敗しました。もう一度お試しください。".into(),
                    ));
                }
                let home = login.home.clone();
                let account = self
                    .info(&home)
                    .await?
                    .account(id.clone())
                    .ok_or("Claude のログインが完了していません。")?;
                self.registry.accounts.push(account);
                if let Err(error) = self.save().await {
                    self.registry.accounts.pop();
                    return Err(error.into());
                }
                self.login = None;
                Ok(AccountLoginStatus {
                    completed: true,
                    account_id: Some(id),
                }
                .into())
            }
            AccountCommand::CancelLogin { id } => {
                if self.login.as_ref().is_some_and(|login| login.id == id) {
                    self.cancel().await?;
                }
                Ok(Empty {}.into())
            }
        }
    }

    pub(super) async fn cancel(&mut self) -> Result<(), String> {
        if let Some(login) = self.login.as_mut() {
            login.process.input.take();
            let _ = login.process.child.wait().await;
            let home = login.home.clone();
            // A completion can race cancellation; let the CLI remove its credentials.
            let mut process = Cli::start(&self.program, Some(&home), &["auth", "logout"], None)?;
            // A failed logout is harmless only when the CLI explicitly confirms
            // there are no credentials. Keep the login available for retry on
            // every other failure, including an unreadable status response.
            if !process.finish().await?.status.success() && !self.info(&home).await?.logged_out() {
                return Err("Claude の認証手続きを破棄できません。".into());
            }
            tokio::fs::remove_dir_all(&home)
                .await
                .map_err(|_| "Claude の一時設定を削除できません。")?;
            self.login = None;
        }
        Ok(())
    }
}

fn authentication_environment(home: Option<&Path>) -> Vec<(String, Option<String>)> {
    let mut environment: Vec<_> = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "CLAUDECODE",
    ]
    .into_iter()
    .map(|key| (key.into(), None))
    .collect();
    if let Some(home) = home {
        for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
            environment.push((key.into(), Some(home.to_string_lossy().into_owned())));
        }
    }
    environment
}

impl Cli {
    fn start(
        program: &Path,
        home: Option<&Path>,
        args: &[&str],
        url: Option<oneshot::Sender<(String, String)>>,
    ) -> Result<Self, String> {
        let path = std::env::var_os("PATH");
        let program = super::process::executable(program, path.as_deref())?;
        let mut command =
            agent_process::command(&program).map_err(|_| "Claude の認証処理を起動できません。")?;
        for (key, value) in authentication_environment(home) {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The browser is opened by the requesting client, including an iPhone.
        agent_process::suppress_browser(&mut command)
            .map_err(|_| "Claude の認証処理を起動できません。")?;
        let mut child = command
            .spawn()
            .map_err(|_| "Claude の認証処理を起動できません。")?;
        let output = tokio::spawn(read_output(child.stdout.take().unwrap(), url));
        let input = child.stdin.take();
        Ok(Self {
            child,
            input,
            output,
        })
    }

    async fn finish(&mut self) -> Result<std::process::Output, String> {
        let status = tokio::time::timeout(Duration::from_secs(30), self.child.wait())
            .await
            .map_err(|_| "Claude の認証処理がタイムアウトしました。")?
            .map_err(|_| "Claude の認証処理が終了しました。")?;
        self.input.take();
        let stdout = (&mut self.output)
            .await
            .map_err(|_| "Claude の認証応答を読み取れません。")??;
        Ok(std::process::Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }
}
async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    mut url: Option<oneshot::Sender<(String, String)>>,
) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let length = reader
            .read(&mut buffer)
            .await
            .map_err(|_| "Claude の認証応答を読み取れません。")?;
        if length == 0 {
            break;
        }
        if output.len() + length > 64 * 1024 {
            return Err("Claude の認証応答が長すぎます。".into());
        }
        output.extend_from_slice(&buffer[..length]);
        if url.is_some() {
            let text = String::from_utf8_lossy(&output);
            let complete = text
                .rfind(char::is_whitespace)
                .map_or("", |end| &text[..end]);
            for word in complete.split_whitespace() {
                if let Ok(parsed) = reqwest::Url::parse(word)
                    && parsed.scheme() == "https"
                    && matches!(
                        parsed.host_str(),
                        Some(
                            "claude.com"
                                | "claude.ai"
                                | "console.anthropic.com"
                                | "platform.claude.com"
                        )
                    )
                    && let Some((_, state)) = parsed
                        .query_pairs()
                        .find(|(key, value)| key == "state" && !value.is_empty())
                {
                    let _ = url.take().unwrap().send((word.into(), state.into_owned()));
                    break;
                }
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn loading_recovers_after_an_invalid_registry_is_corrected() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("accounts");
        std::fs::create_dir(&directory).unwrap();
        let registry = directory.join("accounts.json");
        std::fs::write(&registry, "invalid registry").unwrap();
        let load = || {
            Accounts::load(
                root.path().join("missing-cli"),
                directory.clone(),
                root.path().join("native"),
                false,
            )
        };
        let error = load().await.err().expect("invalid registry must be rejected");
        assert!(error.is::<serde_json::Error>());
        std::fs::write(&registry, r#"{"accounts":[],"selectedId":null}"#).unwrap();
        let accounts = load().await.unwrap();
        assert!(accounts.registry.accounts.is_empty());
        assert_eq!(accounts.selected_home().unwrap(), None);
    }

    #[test]
    fn cli_status_requires_a_logged_in_subscription_and_explicit_logout() {
        for (value, subscribed, logged_out) in [
            (
                json!({"loggedIn":true,"authMethod":"claude.ai","email":"account@example.invalid","subscriptionType":"max"}),
                true,
                false,
            ),
            (
                json!({"loggedIn":false,"authMethod":"none","subscriptionType":"max"}),
                false,
                true,
            ),
            (
                json!({"loggedIn":true,"authMethod":"api_key","subscriptionType":"max"}),
                false,
                false,
            ),
            // A reported auth method is not an explicit logout confirmation.
            (
                json!({"loggedIn":false,"authMethod":"claude.ai"}),
                false,
                false,
            ),
            (
                json!({"loggedIn":true,"authMethod":"claude.ai"}),
                false,
                false,
            ),
            (
                json!({"loggedIn":true,"authMethod":"claude.ai","subscriptionType":""}),
                false,
                false,
            ),
        ] {
            let info: Info = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(info.logged_out(), logged_out, "{value}");
            let account = info.account("claude:test".into());
            assert_eq!(account.is_some(), subscribed, "{value}");
            if let Some(account) = account {
                assert_eq!(account.email.as_deref(), value["email"].as_str());
                assert_eq!(account.plan_type.as_deref(), Some("max"));
            }
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({}),
            json!({"loggedIn":true}),
            json!({"loggedIn":"true","authMethod":"none"}),
        ] {
            assert!(serde_json::from_value::<Info>(invalid).is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn account_status_is_independent_of_sdk_onboarding_and_usage() {
        use std::os::unix::fs::PermissionsExt;
        tokio::time::timeout(Duration::from_secs(30), async {
            let directory = tempfile::tempdir().unwrap();
            let native = directory.path().join("native");
            std::fs::create_dir(&native).unwrap();
            let program = directory.path().join("claude.mjs");
            std::fs::write(&program, r#"#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
const home = process.env.CLAUDE_CONFIG_DIR;
assert.equal(home, process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR);
if (process.argv[2] !== 'auth') {
  console.error('[ACTION REQUIRED] Run claude to review the updated Consumer Terms.');
  process.exit(1);
}
assert.deepEqual(process.argv.slice(2), ['auth', 'status', '--json']);
appendFileSync(join(home, 'operations'), 'auth status\n');
while (existsSync(join(home, 'status-paused'))) await new Promise(resolve => setTimeout(resolve, 10));
console.log(existsSync(join(home, 'invalid-account')) ? 'null' : JSON.stringify({
  loggedIn: true, authMethod: 'claude.ai', email: 'status@example.invalid', subscriptionType: 'max'
}));
"#).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut accounts = Accounts::load(program.clone(), directory.path().join("accounts"), native.clone(), false).await.unwrap();
            let account = accounts.info(&native).await.unwrap().account("claude:desktop".into()).unwrap();
            assert_eq!(account.email.as_deref(), Some("status@example.invalid"));
            assert_eq!(std::fs::read_to_string(native.join("operations")).unwrap(), "auth status\n");
            accounts.accept_native(0, accounts.info(&native).await.unwrap());
            let usage = accounts.usage_request("claude:desktop").unwrap().await;
            assert!(usage.error.is_some(), "SDK setup failures stay scoped to usage");
            assert_eq!(accounts.list().0, vec![account.clone()]);

            std::fs::write(native.join("status-paused"), "").unwrap();
            std::fs::remove_file(native.join("operations")).unwrap();
            let claude = super::super::Claude::load(program, directory.path().join("adapter"), Some(native.clone())).await.unwrap();
            claude.models.read(native.clone(), || async { Ok(Vec::new()) }).await.unwrap();
            let mut listing = Box::pin(crate::providers::Identity::list(&claude));
            tokio::select! {
                _ = &mut listing => panic!("status returned before release"),
                _ = async {
                    while !native.join("operations").exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {}
            }
            let mut models = Box::pin(claude.models());
            assert!(matches!(futures_util::poll!(&mut models), std::task::Poll::Ready(Ok(value)) if value.is_empty()),
                "a native identity check must not block cached model reads");
            std::fs::remove_file(native.join("status-paused")).unwrap();
            assert_eq!(listing.await.unwrap().accounts, vec![account]);
            std::fs::write(native.join("invalid-account"), "").unwrap();
            assert!(accounts.info(&native).await.is_err());
        }).await.unwrap();
    }

    #[tokio::test]
    async fn failed_native_discovery_returns_saved_accounts_with_a_scoped_error() {
        let directory = tempfile::tempdir().unwrap();
        let native = directory.path().join("native");
        let claude = super::super::Claude::load(
            directory.path().join("missing-claude"),
            directory.path().join("adapter"),
            Some(native),
        )
        .await
        .unwrap();
        let saved = Account {
            id: "claude:desktop".into(),
            provider: ProviderKind::Claude,
            email: Some("native@example.invalid".into()),
            plan_type: Some("max".into()),
            usage: None,
        };
        claude
            .accounts
            .lock()
            .await
            .registry
            .accounts
            .push(saved.clone());
        let listed = crate::providers::Identity::list(&claude).await.unwrap();
        assert_eq!(listed.accounts, vec![saved]);
        assert_eq!(
            listed
                .selected
                .get(&ProviderKind::Claude)
                .map(String::as_str),
            Some("claude:desktop")
        );
        assert!(listed.errors[&ProviderKind::Claude].contains("executable is missing"));
        assert!(claude.accounts.lock().await.native_checked_at.is_none());
    }

    #[tokio::test]
    async fn repeated_listing_reuses_native_identity_but_expiration_rechecks_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut accounts = Accounts::load(
            directory.path().join("missing-claude"),
            directory.path().join("accounts"),
            directory.path().join("native"),
            false,
        )
        .await
        .unwrap();
        accounts.registry.accounts.push(Account {
            id: "claude:desktop".into(),
            provider: ProviderKind::Claude,
            email: Some("native@example.invalid".into()),
            plan_type: None,
            usage: None,
        });
        accounts.native_checked_at = Some(Instant::now());
        assert!(accounts.native_refresh().is_none());
        let (listed, selected) = accounts.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(selected.as_deref(), Some("claude:desktop"));
        accounts.native_checked_at = Some(Instant::now() - Duration::from_secs(61));
        assert!(accounts.native_refresh().unwrap().1.await.is_err());
        accounts.native_checked_at = Some(Instant::now());
        assert!(
            accounts
                .request(AccountCommand::Logout {
                    id: "claude:desktop".into()
                })
                .await
                .is_err()
        );
        assert!(accounts.native_checked_at.is_none());
    }

    #[tokio::test]
    async fn discovery_started_before_logout_cannot_restore_the_old_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut accounts = Accounts::load(
            directory.path().join("missing-claude"),
            directory.path().join("accounts"),
            directory.path().join("native"),
            false,
        )
        .await
        .unwrap();
        accounts.registry.accounts.push(Account {
            id: "claude:desktop".into(),
            provider: ProviderKind::Claude,
            email: Some("before@example.invalid".into()),
            plan_type: None,
            usage: None,
        });
        let revision = accounts.native_refresh().unwrap().0;
        assert!(
            accounts
                .request(AccountCommand::Logout {
                    id: "claude:desktop".into()
                })
                .await
                .is_err()
        );
        accounts.accept_native(
            revision,
            Info {
                email: Some("stale@example.invalid".into()),
                subscription_type: Some("max".into()),
                logged_in: true,
                auth_method: "claude.ai".into(),
            },
        );
        assert!(accounts.registry.selected_id.is_none());
        assert!(accounts.native_checked_at.is_none());
        assert_eq!(
            accounts.registry.accounts[0].email.as_deref(),
            Some("before@example.invalid")
        );
    }
}
