use super::server::Context;
use crate::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, rc::Rc};

pub(super) struct Accounts {
    path: PathBuf,
    current: Value,
}

impl Accounts {
    pub(super) fn load(home: &std::path::Path) -> Result<Self> {
        let path = home.join("account-fixture.json");
        let current = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, current })
    }

    // This is the provider boundary. Tokens are deliberately invalid and never
    // accepted by a real service; persistence remains inside the test directory.
    pub(super) fn request(
        &mut self,
        context: &Rc<Context>,
        id: &Value,
        method: &str,
        params: &Value,
    ) -> Result<()> {
        match method {
            "account/read" => context.respond(
                id,
                &json!({"account":self.current,"requiresOpenaiAuth":true}),
            ),
            "getAuthStatus" => {
                let token = if self.current.is_null() {
                    None
                } else {
                    let claims = json!({"https://api.openai.com/auth":{"chatgpt_account_id":self.current["accountId"],"chatgpt_plan_type":self.current["planType"]}});
                    Some(format!(
                        "fixture.{}.invalid-test-signature",
                        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
                    ))
                };
                context.respond(id, &json!({"authMethod":if token.is_some() { Some("chatgpt") } else { None },"authToken":token}))
            }
            "account/login/start" if params["type"] == "chatgptAuthTokens" => {
                let payload = params["accessToken"]
                    .as_str()
                    .and_then(|token| token.split('.').nth(1))
                    .ok_or("missing fixture access token")?;
                let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
                let account_id = &claims["https://api.openai.com/auth"]["chatgpt_account_id"];
                if account_id != &params["chatgptAccountId"] {
                    return context.error(id, -32602, "account mismatch");
                }
                self.current = json!({"type":"chatgpt","email":format!("{}@example.invalid",account_id.as_str().unwrap()),"planType":params["chatgptPlanType"],"accountId":account_id});
                context.respond(id, &json!({"type":"chatgptAuthTokens"}))
            }
            "account/login/start" => {
                context.respond(id, &json!({"type":"chatgptDeviceCode","loginId":"fixture-login","userCode":"TEST-CODE","verificationUrl":"https://example.invalid/login"}))?;
                self.current = json!({"type":"chatgpt","email":"second@example.invalid","planType":"pro","accountId":"second"});
                fs::write(&self.path, serde_json::to_vec(&self.current)?)?;
                let context = context.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(context.config.delay()).await;
                    let _ = context.notify(
                        "account/login/completed",
                        &json!({"loginId":"fixture-login","success":true,"error":null}),
                    );
                });
                Ok(())
            }
            "account/login/cancel" => context.respond(id, &json!({"status":"canceled"})),
            "account/logout" => {
                self.current = Value::Null;
                match fs::remove_file(&self.path) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                context.respond(id, &json!({}))
            }
            "fixture/account/current" => {
                context.respond(id, &json!({"accountId":self.current["accountId"]}))
            }
            "fixture/account/refresh" => {
                let refresh_id = "fixture-auth-refresh";
                let (sender, receiver) = tokio::sync::oneshot::channel();
                context
                    .pending
                    .borrow_mut()
                    .insert(refresh_id.into(), sender);
                context.request(refresh_id, "account/chatgptAuthTokens/refresh", &json!({"reason":"unauthorized","previousAccountId":params["previousAccountId"]}))?;
                let context = context.clone();
                let id = id.clone();
                tokio::task::spawn_local(async move {
                    match tokio::time::timeout(std::time::Duration::from_secs(10), receiver).await {
                        Ok(Ok(result)) => {
                            let _ = context.respond(&id, &json!({"accountId":result["chatgptAccountId"],"hasToken":result["accessToken"].as_str().is_some_and(|token| !token.is_empty())}));
                        }
                        _ => {
                            let _ = context.error(&id, -32603, "refresh response missing");
                        }
                    }
                });
                Ok(())
            }
            _ => context.error(id, -32601, "unknown account fixture method"),
        }
    }
}
