use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{HostRpcService, HostSession, ProjectStore};
use serde_json::{Value, json};
use std::sync::Arc;

mod codex_fixture;

async fn rpc(server: &CodexAppServer, method: &str, params: Value) -> Value {
    let response = server
        .request_raw(&json!({"id":1,"method":method,"params":params}).to_string())
        .await
        .unwrap();
    let mut response: Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response["result"].take()
}

async fn call(
    service: &HostRpcService,
    session: &mut HostSession,
    method: &str,
    mut params: Value,
) -> Value {
    if method.starts_with("host/account/") && method != "host/account/list" {
        params["provider"] = json!("codex");
    }
    let reply = service
        .dispatch(
            session.id(),
            &agent_protocol::protocol::json_boundary::call(method, params).unwrap(),
        )
        .await
        .unwrap();
    let line = agent_protocol::protocol::json_boundary::reply(method, &reply.initial)
        .unwrap()
        .to_string();
    assert!(
        !line.contains("invalid-test-signature") && !line.contains("fixture-api-key"),
        "Host leaked a credential into the mobile session"
    );
    assert!(
        !line.contains("account/chatgptAuthTokens/refresh"),
        "Host forwarded a private credential request to mobile"
    );
    let mut result: Value = serde_json::from_str(&line).unwrap();
    assert!(result.get("result").is_some() || result.get("error").is_some());
    if method == "host/session/open" && result.get("error").is_none() {
        result["result"] = result["result"]["response"].take();
    }
    result
}

async fn completed_turn(
    service: &HostRpcService,
    session: &mut HostSession,
    thread: &str,
    text: &str,
) -> String {
    let open = agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
        session: agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: thread.into(),
        },
        limit: 5,
    });
    let reply = service.dispatch(session.id(), &open).await.unwrap();
    let mut updates = reply.updates.unwrap();
    let result = call(
        service,
        session,
        "host/session/submit",
        json!({"threadId":{"provider":"codex","id":thread},"clientUserMessageId":text,"input":[{"text":{"text":text}}]}),
    )
    .await;
    assert!(result.get("error").is_none(), "{result}");
    let id = result["result"]["turnId"].as_str().unwrap().to_owned();
    loop {
        let event = agent_protocol::protocol::decode::<agent_protocol::session::SessionChange>(
            &updates.recv().await.unwrap(),
        )
        .unwrap();
        if let agent_protocol::session::SessionChange::Turn {
            turn,
            completed: true,
        } = event
            && turn.id.as_str() == id
        {
            return id;
        }
    }
}

#[tokio::test]
async fn account_switch_keeps_shared_history_and_restores_selection_without_exposing_tokens() {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("account-fixture.json"), r#"{"type":"chatgpt","email":"desktop@example.invalid","planType":"plus","accountId":"desktop"}"#).unwrap();
        let config = AppServerConfig { codex_home: Some(home.clone()), ..codex_fixture::config(&home) };
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        let accounts_dir = directory.path().join("accounts");
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session();
        let list = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(list["result"]["accounts"][0]["email"], "desktop@example.invalid");
        assert_eq!(list["result"]["selected"]["codex"], "desktop");
        assert!(list["result"]["accounts"][0]["usage"].is_null());
        std::fs::write(home.join("usage-paused"), "").unwrap();
        let usage_service = service.clone();
        let mut usage_session = service.open_session();
        let usage = tokio::spawn(async move {
            call(&usage_service, &mut usage_session, "host/account/usage", json!({"accountId":"desktop"})).await
        });
        while !home.join("usage-requested").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let selected = tokio::time::timeout(std::time::Duration::from_secs(2),
            call(&service, &mut session, "host/account/select", json!({"accountId":"desktop"})),
        ).await.expect("selection must not wait for usage");
        assert_eq!(selected["result"]["selectedId"], "desktop");
        let list = tokio::time::timeout(std::time::Duration::from_secs(2),
            call(&service, &mut session, "host/account/list", json!({})),
        ).await.expect("listing must not wait for usage");
        assert_eq!(list["result"]["accounts"][0]["id"], "desktop");
        assert!(!usage.is_finished());
        std::fs::remove_file(home.join("usage-paused")).unwrap();
        let usage = usage.await.unwrap();
        assert_eq!(usage["result"]["windows"][0]["remainingPercent"], 72);
        assert_eq!(usage["result"]["windows"][1]["remainingPercent"], 86);
        let started = call(&service, &mut session, "host/session/create", json!({"provider":"codex","cwd":home})).await;
        let thread = started["result"]["thread"]["id"]["id"].as_str().unwrap();
        completed_turn(&service, &mut session, thread, "before switch").await;
        let before = call(&service, &mut session, "host/session/open", json!({"session":{"provider":"codex","id":thread},"limit":5})).await;
        assert!(before.get("error").is_none(), "{before}");
        assert_eq!(before["result"]["thread"]["turns"].as_array().unwrap().len(), 1);
        let canceled = call(&service, &mut session, "host/account/login/start", json!({"provider":"codex"})).await;
        assert!(call(&service, &mut session, "host/account/login/cancel", json!({"loginId":canceled["result"]["loginId"]})).await.get("error").is_none());
        // Dismissing an already discarded login must still allow another attempt.
        assert!(call(&service, &mut session, "host/account/login/cancel", json!({"loginId":canceled["result"]["loginId"]})).await.get("error").is_none());
        let list = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(list["result"]["accounts"].as_array().unwrap().len(), 1);
        let login = call(&service, &mut session, "host/account/login/start", json!({"provider":"codex"})).await;
        assert_eq!(login["result"]["userCode"], "TEST-CODE");
        let status = loop {
            let status = call(&service, &mut session, "host/account/login/status", json!({"loginId":login["result"]["loginId"]})).await;
            assert!(status.get("error").is_none(), "{status}");
            if status["result"]["completed"] == true { break status; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let repeated = call(&service, &mut session, "host/account/login/status", json!({"loginId":login["result"]["loginId"]})).await;
        assert_eq!(repeated["result"], status["result"]);
        let second = status["result"]["accountId"].as_str().unwrap();
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":second})).await["result"]["selectedId"], second);
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        assert_eq!(server.initialize_response().codex_home, dunce::canonicalize(&home).unwrap(), "account selection must retain the conversation configuration home");
        let refreshed = rpc(&server, "fixture/account/refresh", json!({"previousAccountId":"desktop"})).await;
        assert_eq!(refreshed, json!({"accountId":"desktop","hasToken":true}));
        let after = call(&service, &mut session, "host/session/open", json!({"session":{"provider":"codex","id":thread},"limit":5})).await;
        assert_eq!(before["result"]["thread"], after["result"]["thread"]);
        completed_turn(&service, &mut session, thread, "after switch").await;
        let after = call(&service, &mut session, "host/session/open", json!({"session":{"provider":"codex","id":thread},"limit":5})).await;
        assert_eq!(after["result"]["thread"]["turns"].as_array().unwrap().len(), 2);
        let invalid = call(&service, &mut session, "host/account/select", json!({"accountId":"missing"})).await;
        assert!(invalid.get("error").is_some());
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        let stored = std::fs::read_to_string(accounts_dir.join("accounts.json")).unwrap();
        std::fs::remove_file(accounts_dir.join("accounts.json")).unwrap();
        std::fs::create_dir(accounts_dir.join("accounts.json")).unwrap();
        let selected = call(&service, &mut session, "host/account/select", json!({"accountId":"desktop"})).await;
        assert_eq!(selected["result"]["selectedId"], "desktop");
        assert!(selected["result"]["persistenceError"].is_string());
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "desktop");
        std::fs::remove_dir(accounts_dir.join("accounts.json")).unwrap();
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":second})).await["result"]["selectedId"], second);
        assert!(!stored.contains("authToken") && !stored.contains("invalid-test-signature"));
        drop(session); drop(service);
        server.shutdown().await.unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session();
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        assert_eq!(server.initialize_response().codex_home, dunce::canonicalize(&home).unwrap(), "account selection must retain the conversation configuration home");
        let missing = rpc(&server, "fixture/account/refresh", json!({"previousAccountId":"missing"})).await;
        assert_eq!(missing["hasToken"], false);
        let refreshed = rpc(&server, "fixture/account/refresh", json!({"previousAccountId":"desktop"})).await;
        assert_eq!(refreshed["accountId"], "desktop");
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":"desktop"})).await["result"]["selectedId"], "desktop");
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "desktop");
        drop(session); drop(service);
        server.shutdown().await.unwrap();
        std::fs::remove_file(home.join("account-fixture.json")).unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        service.enable_accounts(accounts_dir, config).await.unwrap();
        let mut session = service.open_session();
        let accounts = call(&service, &mut session, "host/account/list", json!({})).await;
        assert!(accounts["result"]["selected"]["codex"].is_null());
        assert!(accounts["result"]["error"].is_string());
        assert!(call(&service, &mut session, "host/session/list", json!({})).await.get("error").is_none());
        assert_eq!(call(&service, &mut session, "host/session/submit", json!({"threadId":{"provider":"codex","id":"any"},"clientUserMessageId":"unavailable-account","input":[{"text":{"text":"must not use a different account"}}]})).await["error"]["code"], "account_unavailable");
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":second})).await["result"]["selectedId"], second);
        let started = call(&service, &mut session, "host/session/create", json!({"provider":"codex","cwd":home})).await;
        completed_turn(&service, &mut session, started["result"]["thread"]["id"]["id"].as_str().unwrap(), "recovered account").await;
        let logged_out = call(&service, &mut session, "host/account/logout", json!({"accountId":second})).await;
        assert!(logged_out.get("error").is_none(), "{logged_out}");
        let remaining = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(remaining["result"]["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(remaining["result"]["accounts"][0]["id"], "desktop");
        assert!(remaining["result"]["selected"]["codex"].is_null(), "logout must not select another saved account");

        drop(session); drop(service);
        server.shutdown().await.unwrap();
    }).await.expect("account switching stalled");
}

#[tokio::test]
async fn helper_initialization_does_not_block_completed_turns() {
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let config = codex_fixture::config(home);
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(
            Ok(server.clone()),
            ProjectStore::new(home.join("bex-worktrees.json")),
        );
        service
            .enable_accounts(home.join("accounts"), config)
            .await
            .unwrap();
        let mut session = service.open_session();
        let started = call(
            &service,
            &mut session,
            "host/session/create",
            json!({"provider":"codex","cwd":home}),
        )
        .await;
        let thread = started["result"]["thread"]["id"]["id"].as_str().unwrap();
        let gate = home.join("initialize-release");
        std::fs::write(
            home.join("fixture-config.json"),
            serde_json::to_vec(&host_fixture::fixture::Config {
                initialize_gate: Some(gate.clone()),
                stream_delay_ms: 5,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let mut login_session = service.open_session();
        let login_service = service.clone();
        let login = tokio::spawn(async move {
            call(
                &login_service,
                &mut login_session,
                "host/account/login/start",
                json!({"provider":"codex"}),
            )
            .await
        });
        while !gate.with_extension("entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let completion = tokio::time::timeout(
            Duration::from_secs(2),
            completed_turn(&service, &mut session, thread, "while logging in"),
        )
        .await;
        std::fs::write(&gate, []).unwrap();
        assert!(login.await.unwrap().get("error").is_none());
        completion.expect("turn waited for unrelated account helper initialization");
        let read = call(
            &service,
            &mut session,
            "host/session/open",
            json!({"session":{"provider":"codex","id":thread},"limit":5}),
        )
        .await;
        assert_eq!(read["result"]["thread"]["turns"][0]["status"], "completed");
        assert!(
            read["result"]["thread"]
                .to_string()
                .contains("while logging in")
        );
        drop(session);
        drop(service);
        server.shutdown().await.unwrap();
    })
    .await
    .expect("concurrent account/turn fixture stalled");
}

#[tokio::test]
async fn native_accounts_restore_selection_and_remain_signed_out_after_logout() {
    for account in [
        json!({"type":"chatgpt","email":"desktop@example.invalid","planType":"plus","accountId":"desktop"}),
        json!({"type":"apiKey"}),
    ] {
        tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("account-fixture.json"), serde_json::to_vec(&account).unwrap()).unwrap();
        let config = AppServerConfig { codex_home: Some(home.clone()), ..codex_fixture::config(&home) };
        let accounts_dir = directory.path().join("accounts");
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session();
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["selected"]["codex"], "desktop");
        if account["type"] == "apiKey" {
            assert_eq!(listed["result"]["accounts"][0]["planType"], "API key");
            assert!(listed["result"]["accounts"][0]["email"].is_null());
            let usage = call(&service, &mut session, "host/account/usage", json!({"accountId":"desktop"})).await;
            assert_eq!(usage["result"]["windows"], json!([]));
            assert!(usage["result"]["error"].is_null());
        }
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":"desktop"})).await["result"]["selectedId"], "desktop");
        let stored = std::fs::read_to_string(accounts_dir.join("accounts.json")).unwrap();
        assert!(!stored.contains("fixture-api-key") && !stored.contains("invalid-test-signature"));
        drop(session); drop(service);
        server.shutdown().await.unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session();
        assert_eq!(call(&service, &mut session, "host/account/list", json!({})).await["result"]["selected"]["codex"], "desktop");
        let invalid = call(&service, &mut session, "host/account/logout", json!({"accountId":"missing"})).await;
        assert!(invalid.get("error").is_some());
        assert_eq!(rpc(&server, "account/read", json!({})).await["account"], account);
        // Failed persistence must leave the account and its credentials usable.
        let registry = std::fs::read(accounts_dir.join("accounts.json")).unwrap();
        std::fs::remove_file(accounts_dir.join("accounts.json")).unwrap();
        std::fs::create_dir(accounts_dir.join("accounts.json")).unwrap();
        let failed = call(&service, &mut session, "host/account/logout", json!({"accountId":"desktop"})).await;
        assert!(failed.get("error").is_some());
        assert_eq!(rpc(&server, "account/read", json!({})).await["account"], account);
        assert_eq!(call(&service, &mut session, "host/account/list", json!({})).await["result"]["selected"]["codex"], "desktop");
        std::fs::remove_dir(accounts_dir.join("accounts.json")).unwrap();
        std::fs::write(accounts_dir.join("accounts.json"), registry).unwrap();
        let logged_out = call(&service, &mut session, "host/account/logout", json!({"accountId":"desktop"})).await;
        assert!(logged_out.get("error").is_none(), "{logged_out}");
        assert!(rpc(&server, "fixture/account/current", json!({})).await["accountId"].is_null());
        assert!(!home.join("account-fixture.json").exists());
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["accounts"], json!([]));
        assert!(listed["result"]["selected"]["codex"].is_null());
        drop(session); drop(service);
        server.shutdown().await.unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(home.join("bex-worktrees.json")));
        service.enable_accounts(accounts_dir.clone(), config).await.unwrap();
        let mut session = service.open_session();
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["accounts"], json!([]));
        assert!(listed["result"]["selected"]["codex"].is_null());
        let login = call(&service, &mut session, "host/account/login/start", json!({"provider":"codex"})).await;
        let status = loop {
            let status = call(&service, &mut session, "host/account/login/status", json!({"loginId":login["result"]["loginId"]})).await;
            assert!(status.get("error").is_none(), "{status}");
            if status["result"]["completed"] == true { break status; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        let account_id = status["result"]["accountId"].as_str().unwrap();
        let selected = call(&service, &mut session, "host/account/select", json!({"accountId":account_id})).await;
        assert!(selected.get("error").is_none(), "{selected}");
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        let logged_out = call(&service, &mut session, "host/account/logout", json!({"accountId":account_id})).await;
        assert!(logged_out.get("error").is_none(), "{logged_out}");
        assert!(!accounts_dir.join(account_id).join("account-fixture.json").exists());
        assert_eq!(rpc(&server, "fixture/account/refresh", json!({"previousAccountId":"second"})).await["hasToken"], false);
        drop(session); drop(service);
        server.shutdown().await.unwrap();
    }).await.expect("logout and login stalled");
    }
}
