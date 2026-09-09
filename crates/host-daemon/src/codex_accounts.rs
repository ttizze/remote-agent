use std::{collections::HashMap, path::PathBuf};

use agent_core::peer::PeerEvent;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_app_server::{AppServerConfig, CodexAppServer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    id: String,
    email: String,
    plan_type: String,
    chatgpt_account_id: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    accounts: Vec<Account>,
    selected_id: Option<String>,
}

struct Login {
    directory: tempfile::TempDir,
    server: CodexAppServer,
    events: broadcast::Receiver<PeerEvent>,
    id: String,
    completed: bool,
}

// Only these helpers have separate credential stores. All thread RPCs continue
// through the original App Server and its original CODEX_HOME.
pub(crate) struct Accounts {
    directory: PathBuf,
    default_home: PathBuf,
    config: AppServerConfig,
    registry: Registry,
    helpers: HashMap<String, CodexAppServer>,
    login: Option<Login>,
    completed_login: Option<(String, String)>,
    restoration_error: Option<String>,
}

impl Accounts {
    pub(crate) async fn load(
        directory: PathBuf,
        config: AppServerConfig,
        primary: &CodexAppServer,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&directory)
            .map_err(|_| "アカウントの保存先を作成できませんでした。")?;
        let registry = match std::fs::read(directory.join("accounts.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| "アカウント設定を読み込めませんでした。")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(_) => return Err("アカウント設定を読み込めませんでした。".into()),
        };
        let mut accounts = Self {
            directory,
            default_home: primary.initialize_response().codex_home.clone(),
            config,
            registry,
            helpers: HashMap::new(),
            login: None,
            completed_login: None,
            restoration_error: None,
        };
        if let Some(id) = accounts.registry.selected_id.clone() {
            accounts.restoration_error = accounts.select(primary, &id).await.err();
        }
        Ok(accounts)
    }

    pub(crate) fn restoration_error(&self) -> Option<&str> {
        self.restoration_error.as_deref()
    }

    async fn helper(&mut self, id: &str) -> Result<&CodexAppServer, String> {
        if !self.helpers.contains_key(id) {
            if id != "desktop"
                && !self
                    .registry
                    .accounts
                    .iter()
                    .any(|account| account.id == id)
            {
                return Err("アカウントが見つかりません。".into());
            }
            let home = if id == "desktop" {
                self.default_home.clone()
            } else {
                self.directory.join(id)
            };
            let mut config = self.config.clone();
            config.codex_home = Some(home);
            if id != "desktop" {
                config
                    .config_overrides
                    .push("cli_auth_credentials_store=\"keyring\"".into());
            }
            let helper = CodexAppServer::spawn(config)
                .await
                .map_err(|_| "Codexの認証処理を起動できませんでした。")?;
            self.helpers.insert(id.to_owned(), helper);
        }
        Ok(&self.helpers[id])
    }

    async fn discover_desktop(&mut self) -> Result<(), String> {
        if self
            .registry
            .accounts
            .iter()
            .any(|account| account.id == "desktop")
        {
            return Ok(());
        }
        let helper = self.helper("desktop").await?;
        let info = rpc(helper, "account/read", json!({"refreshToken":false})).await?;
        if info["account"]["type"] != "chatgpt" {
            return Ok(());
        }
        let auth = credentials(helper, false).await?;
        self.registry
            .accounts
            .push(account("desktop".into(), &info, &auth)?);
        self.save()
    }

    pub(crate) async fn request(
        &mut self,
        primary: &CodexAppServer,
        method: &str,
        params: &Value,
    ) -> Result<Value, String> {
        match method {
            "host/account/list" => {
                self.discover_desktop().await?;
                let selected = if self.restoration_error.is_some() {
                    None
                } else {
                    self.registry.selected_id.as_deref().or_else(|| {
                        self.registry
                            .accounts
                            .iter()
                            .any(|a| a.id == "desktop")
                            .then_some("desktop")
                    })
                };
                Ok(
                    json!({"accounts":self.registry.accounts,"selectedId":selected,"error":self.restoration_error}),
                )
            }
            "host/account/select" => {
                let id = params["accountId"]
                    .as_str()
                    .ok_or("アカウントを指定してください。")?;
                self.select(primary, id).await?;
                Ok(json!({"selectedId":id,"persistenceError":self.save().err()}))
            }
            "host/account/login/start" => {
                if self.login.is_some() {
                    self.cancel_login().await?;
                }
                let directory = tempfile::Builder::new()
                    .prefix("account-")
                    .tempdir_in(&self.directory)
                    .map_err(|_| "認証情報の保存先を作成できませんでした。")?;
                let mut config = self.config.clone();
                config.codex_home = Some(directory.path().to_owned());
                config
                    .config_overrides
                    .push("cli_auth_credentials_store=\"keyring\"".into());
                let server = CodexAppServer::spawn(config)
                    .await
                    .map_err(|_| "Codexの認証処理を起動できませんでした。")?;
                let events = server.subscribe();
                let result = rpc(
                    &server,
                    "account/login/start",
                    json!({"type":"chatgptDeviceCode"}),
                )
                .await?;
                let id = result["loginId"]
                    .as_str()
                    .ok_or("ログインを開始できませんでした。")?
                    .to_owned();
                let response = json!({"loginId":id,"userCode":result["userCode"],"verificationUrl":result["verificationUrl"]});
                self.login = Some(Login {
                    directory,
                    server,
                    events,
                    id,
                    completed: false,
                });
                self.completed_login = None;
                Ok(response)
            }
            "host/account/login/status" => self.login_status(params).await,
            "host/account/login/cancel" => {
                if self
                    .completed_login
                    .as_ref()
                    .is_some_and(|(id, _)| params["loginId"] == *id)
                {
                    return Ok(json!({}));
                }
                let login = self.login.as_ref().ok_or("ログイン手続きがありません。")?;
                if params["loginId"] != login.id {
                    return Err("ログイン手続きが一致しません。".into());
                }
                self.cancel_login().await?;
                Ok(json!({}))
            }
            _ => Err("未対応のアカウント操作です。".into()),
        }
    }

    async fn login_status(&mut self, params: &Value) -> Result<Value, String> {
        if let Some((login_id, account_id)) = &self.completed_login
            && params["loginId"] == *login_id
        {
            return Ok(json!({"completed":true,"accountId":account_id}));
        }
        let login = self.login.as_mut().ok_or("ログイン手続きがありません。")?;
        if params["loginId"] != login.id {
            return Err("ログイン手続きが一致しません。".into());
        }
        while !login.completed {
            match login.events.try_recv() {
                Ok(PeerEvent::Message(message)) => {
                    let event: Value =
                        serde_json::from_str(&message.value).map_err(|_| "認証応答が無効です。")?;
                    if event["method"] != "account/login/completed"
                        || event["params"]["loginId"] != login.id
                    {
                        continue;
                    }
                    if event["params"]["success"] != true {
                        self.login = None;
                        return Err("ログインが完了しませんでした。もう一度お試しください。".into());
                    }
                    login.completed = true;
                }
                Ok(PeerEvent::Response { .. }) => {}
                Err(broadcast::error::TryRecvError::Empty) => {
                    return Ok(json!({"completed":false}));
                }
                Ok(PeerEvent::Closed(_)) | Err(_) => {
                    self.login = None;
                    return Err("認証処理との接続が切れました。".into());
                }
            }
        }
        let info = rpc(&login.server, "account/read", json!({"refreshToken":false})).await?;
        let auth = credentials(&login.server, false).await?;
        let id = login
            .directory
            .path()
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("保存先が無効です。")?
            .to_owned();
        let entry = account(id.clone(), &info, &auth)?;
        self.registry.accounts.push(entry);
        if let Err(error) = self.save() {
            self.registry.accounts.pop();
            return Err(error);
        }
        let login = self.login.take().unwrap();
        let _ = login.directory.keep();
        self.completed_login = Some((login.id, id.clone()));
        self.helpers.insert(id.clone(), login.server);
        Ok(json!({"completed":true,"accountId":id}))
    }

    async fn select(&mut self, primary: &CodexAppServer, id: &str) -> Result<(), String> {
        if !self
            .registry
            .accounts
            .iter()
            .any(|account| account.id == id)
        {
            return Err("アカウントが見つかりません。".into());
        }
        let auth = credentials(self.helper(id).await?, false).await?;
        rpc(
            primary,
            "account/login/start",
            json!({"type":"chatgptAuthTokens","accessToken":auth.token.as_str(),
            "chatgptAccountId":auth.account_id,"chatgptPlanType":auth.plan}),
        )
        .await?;
        self.registry.selected_id = Some(id.to_owned());
        self.restoration_error = None;
        Ok(())
    }

    async fn cancel_login(&mut self) -> Result<(), String> {
        if let Some(login) = &self.login {
            rpc(
                &login.server,
                "account/login/cancel",
                json!({"loginId":login.id}),
            )
            .await?;
            // A device-code completion can race cancellation. Remove only this
            // unfinished helper's credentials before discarding its directory.
            rpc(&login.server, "account/logout", json!({})).await?;
        }
        self.login = None;
        Ok(())
    }

    pub(crate) async fn refresh(&mut self, previous: Option<&str>) -> Result<Value, String> {
        let id = match previous {
            Some(previous) => self
                .registry
                .accounts
                .iter()
                .find(|account| account.chatgpt_account_id == previous)
                .map(|account| account.id.clone()),
            None => self.registry.selected_id.clone(),
        }
        .ok_or("更新対象のアカウントが見つかりません。ログインしてください。")?;
        let auth = credentials(self.helper(&id).await?, true).await?;
        Ok(
            json!({"accessToken":auth.token.as_str(),"chatgptAccountId":auth.account_id,"chatgptPlanType":auth.plan}),
        )
    }

    fn save(&self) -> Result<(), String> {
        atomicwrites::AtomicFile::new(
            self.directory.join("accounts.json"),
            atomicwrites::AllowOverwrite,
        )
        .write_with_options(
            |file| serde_json::to_writer(file, &self.registry),
            crate::platform::private_file_options(),
        )
        .map_err(|_| "アカウント設定を保存できませんでした。".into())
    }
}

struct Credentials {
    token: Zeroizing<String>,
    account_id: String,
    plan: Option<String>,
}

async fn credentials(server: &CodexAppServer, refresh: bool) -> Result<Credentials, String> {
    let mut result = rpc(
        server,
        "getAuthStatus",
        json!({"includeToken":true,"refreshToken":refresh}),
    )
    .await?;
    if !matches!(
        result["authMethod"].as_str(),
        Some("chatgpt" | "chatgptAuthTokens")
    ) {
        return Err("ChatGPTアカウントでCodexにログインしてください。".into());
    }
    let Value::String(token) = result["authToken"].take() else {
        return Err("Codexにログインしてください。".into());
    };
    let token = Zeroizing::new(token);
    let claims = token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .ok_or("Codexの認証情報が無効です。")?;
    let auth = &claims["https://api.openai.com/auth"];
    let account_id = auth["chatgpt_account_id"]
        .as_str()
        .ok_or("ChatGPTアカウント情報がありません。")?
        .to_owned();
    Ok(Credentials {
        token,
        account_id,
        plan: auth["chatgpt_plan_type"].as_str().map(str::to_owned),
    })
}

fn account(id: String, info: &Value, auth: &Credentials) -> Result<Account, String> {
    Ok(Account {
        id,
        email: info["account"]["email"]
            .as_str()
            .ok_or("アカウント情報がありません。")?
            .to_owned(),
        plan_type: info["account"]["planType"]
            .as_str()
            .unwrap_or("")
            .to_owned(),
        chatgpt_account_id: auth.account_id.clone(),
    })
}

async fn rpc(server: &CodexAppServer, method: &str, params: Value) -> Result<Value, String> {
    let request =
        Zeroizing::new(json!({"id":"host-account","method":method,"params":params}).to_string());
    let response = Zeroizing::new(
        server
            .request_raw(&request)
            .await
            .map_err(|_| "Codexの認証処理に接続できませんでした。")?,
    );
    let mut value: Value =
        serde_json::from_str(&response).map_err(|_| "Codexの認証応答が無効です。")?;
    if value.get("error").is_some() {
        return Err("Codexの認証操作に失敗しました。ログイン状態を確認してください。".into());
    }
    value
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| "Codexの認証応答が無効です。".into())
}
