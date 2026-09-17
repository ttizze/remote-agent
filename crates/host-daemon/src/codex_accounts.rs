use crate::host_rpc::AccountRequest;
use std::{collections::HashMap, path::PathBuf};

use agent_core::{
    client::{AccountLogin, AccountLoginStatus},
    models::Empty,
    peer::PeerEvent,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_app_server::{AppServerConfig, CodexAppServer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Account {
    id: String,
    email: String,
    plan_type: String,
    chatgpt_account_id: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    accounts: Vec<Account>,
    selected_id: Option<String>,
    #[serde(default)]
    signed_out: bool,
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
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
}

#[derive(Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub(crate) enum AccountResponse<'a> {
    List {
        accounts: &'a [Account],
        selected_id: Option<&'a str>,
        error: Option<String>,
    },
    Selected {
        provider: agent_core::session::ProviderKind,
        selected_id: String,
        persistence_error: Option<String>,
    },
    Login(AccountLogin),
    Status(AccountLoginStatus),
    Empty(Empty),
}

impl Accounts {
    pub(crate) async fn load(
        directory: PathBuf,
        config: AppServerConfig,
        primary: &CodexAppServer,
        restoration_error: tokio::sync::watch::Sender<Option<String>>,
    ) -> Result<Self, String> {
        let registry_directory = directory.clone();
        let registry = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&registry_directory)
                .map_err(|_| "アカウントの保存先を作成できませんでした。")?;
            match std::fs::read(registry_directory.join("accounts.json")) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|_| "アカウント設定を読み込めませんでした。"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Registry::default())
                }
                Err(_) => Err("アカウント設定を読み込めませんでした。"),
            }
        })
        .await
        .map_err(|error| error.to_string())??;
        let mut accounts = Self {
            directory,
            default_home: primary.initialize_response().codex_home.clone(),
            config,
            registry,
            helpers: HashMap::new(),
            login: None,
            completed_login: None,
            restoration_error,
        };
        if accounts.registry.signed_out {
            rpc(primary, "account/logout", json!({})).await?;
            accounts
                .restoration_error
                .send_replace(Some("ログインするアカウントを選択してください。".into()));
        } else if let Some(id) = accounts.registry.selected_id.clone() {
            let error = accounts.select(primary, &id).await.err();
            accounts.restoration_error.send_replace(error);
        }
        Ok(accounts)
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
        self.save().await
    }

    pub(crate) async fn request(
        &mut self,
        primary: &CodexAppServer,
        request: AccountRequest,
    ) -> Result<AccountResponse<'_>, String> {
        match request {
            AccountRequest::List(_) => {
                self.discover_desktop().await?;
                let selected = if self.restoration_error.borrow().is_some() {
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
                Ok(AccountResponse::List {
                    accounts: &self.registry.accounts,
                    selected_id: selected,
                    error: self.restoration_error.borrow().clone(),
                })
            }
            AccountRequest::Select(params) => {
                self.select(primary, &params.id).await?;
                Ok(AccountResponse::Selected {
                    provider: agent_core::session::ProviderKind::Codex,
                    selected_id: params.id,
                    persistence_error: self.save().await.err(),
                })
            }
            AccountRequest::Logout(params) => {
                if !self
                    .registry
                    .accounts
                    .iter()
                    .any(|account| account.id == params.id)
                {
                    return Err("アカウントが見つかりません。".into());
                }
                let selected = self
                    .registry
                    .selected_id
                    .as_deref()
                    .or((!self.registry.signed_out).then_some("desktop"))
                    == Some(params.id.as_str());
                if selected {
                    // Persist the explicit signed-out state before touching credentials,
                    // so a restart cannot silently select another saved account.
                    let was_signed_out = self.registry.signed_out;
                    self.registry.signed_out = true;
                    if let Err(error) = self.save().await {
                        self.registry.signed_out = was_signed_out;
                        return Err(error);
                    }
                    self.restoration_error
                        .send_replace(Some("ログインするアカウントを選択してください。".into()));
                    rpc(primary, "account/logout", json!({})).await?;
                }
                rpc(self.helper(&params.id).await?, "account/logout", json!({})).await?;
                self.helpers.remove(&params.id);
                if self
                    .completed_login
                    .as_ref()
                    .is_some_and(|(_, id)| id == &params.id)
                {
                    self.completed_login = None;
                }
                self.registry
                    .accounts
                    .retain(|account| account.id != params.id);
                if selected {
                    self.registry.selected_id = None;
                }
                self.save().await?;
                Ok(AccountResponse::Empty(Empty {}))
            }
            AccountRequest::LoginStart(_) => {
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
                let mut result = result;
                result["requiresCodeSubmission"] = false.into();
                let response: AccountLogin = serde_json::from_value(result)
                    .map_err(|_| "ログインを開始できませんでした。")?;
                self.login = Some(Login {
                    directory,
                    server,
                    events,
                    id,
                    completed: false,
                });
                self.completed_login = None;
                Ok(AccountResponse::Login(response))
            }
            AccountRequest::LoginSubmit(_) => {
                Err("Codexのコードはブラウザで入力してください。".into())
            }
            AccountRequest::LoginStatus(params) => self
                .login_status(&params.id)
                .await
                .map(AccountResponse::Status),
            AccountRequest::LoginCancel(params) => {
                if self
                    .completed_login
                    .as_ref()
                    .is_some_and(|(id, _)| params.id == *id)
                {
                    return Ok(AccountResponse::Empty(Empty {}));
                }
                // A failed/expired status read may already have discarded the helper.
                // Let clients dismiss that login and start again.
                let Some(login) = self.login.as_ref() else {
                    return Ok(AccountResponse::Empty(Empty {}));
                };
                if params.id != login.id {
                    return Err("ログイン手続きが一致しません。".into());
                }
                self.cancel_login().await?;
                Ok(AccountResponse::Empty(Empty {}))
            }
        }
    }

    async fn login_status(&mut self, requested_id: &str) -> Result<AccountLoginStatus, String> {
        if let Some((login_id, account_id)) = &self.completed_login
            && requested_id == login_id
        {
            return Ok(AccountLoginStatus {
                completed: true,
                account_id: Some(account_id.clone()),
                extra: Default::default(),
            });
        }
        let login = self.login.as_mut().ok_or("ログイン手続きがありません。")?;
        if requested_id != login.id {
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
                    return Ok(AccountLoginStatus {
                        completed: false,
                        account_id: None,
                        extra: Default::default(),
                    });
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
        if let Err(error) = self.save().await {
            self.registry.accounts.pop();
            return Err(error);
        }
        let login = self.login.take().unwrap();
        let _ = login.directory.keep();
        self.completed_login = Some((login.id, id.clone()));
        self.helpers.insert(id.clone(), login.server);
        Ok(AccountLoginStatus {
            completed: true,
            account_id: Some(id),
            extra: Default::default(),
        })
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
        self.registry.signed_out = false;
        self.restoration_error.send_replace(None);
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

    pub(crate) async fn refresh(&mut self, previous: Option<&str>) -> Result<Credentials, String> {
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
        credentials(self.helper(&id).await?, true).await
    }

    async fn save(&self) -> Result<(), String> {
        let path = self.directory.join("accounts.json");
        let registry = self.registry.clone();
        tokio::task::spawn_blocking(move || {
            crate::platform::save_private_json(&path, &registry)
                .map_err(|_| "アカウント設定を保存できませんでした。".to_owned())
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

#[derive(Serialize)]
pub(crate) struct Credentials {
    #[serde(rename = "accessToken", serialize_with = "serialize_token")]
    token: Zeroizing<String>,
    #[serde(rename = "chatgptAccountId")]
    account_id: String,
    #[serde(rename = "chatgptPlanType")]
    plan: Option<String>,
}
fn serialize_token<S: serde::Serializer>(
    token: &Zeroizing<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(token)
}

pub(crate) async fn access_token(
    server: &CodexAppServer,
    refresh: bool,
) -> Result<Zeroizing<String>, String> {
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
    Ok(Zeroizing::new(token))
}

pub(crate) fn token_claims(token: &str) -> Option<Value> {
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

async fn credentials(server: &CodexAppServer, refresh: bool) -> Result<Credentials, String> {
    let token = access_token(server, refresh).await?;
    let claims = token_claims(&token).ok_or("Codexの認証情報が無効です。")?;
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
