use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{DesktopProjectStore, HostRpcService, HostSession};
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
    params: Value,
) -> Value {
    let reply = service
        .dispatch(
            session.id(),
            &agent_core::protocol::json_boundary::call(method, params).unwrap(),
        )
        .await
        .unwrap();
    let line = agent_core::protocol::json_boundary::reply(method, &reply.initial)
        .unwrap()
        .to_string();
    assert!(
        !line.contains("invalid-test-signature"),
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
    let open = agent_core::protocol::Call::OpenSession(agent_core::session::OpenSession {
        session: agent_core::session::SessionRef {
            provider: agent_core::session::ProviderKind::Codex,
            id: thread.into(),
        },
        limit: 5,
    });
    let reply = service.dispatch(session.id(), &open).await.unwrap();
    let mut updates = reply.updates.unwrap();
    let result = call(
        service,
        session,
        "turn/start",
        json!({"threadId":thread,"clientUserMessageId":text,"input":[{"type":"text","text":text}]}),
    )
    .await;
    assert!(result.get("error").is_none(), "{result}");
    let id = result["result"]["turn"]["id"].as_str().unwrap().to_owned();
    loop {
        let event = agent_core::protocol::decode::<agent_core::session::SessionChange>(
            &updates.recv().await.unwrap(),
        )
        .unwrap();
        if let agent_core::session::SessionChange::Turn {
            turn,
            completed: true,
        } = event
            && turn.id == id
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
        let service = HostRpcService::new(Ok(server.clone()), DesktopProjectStore::new(home.join("projects.json")));
        let accounts_dir = directory.path().join("accounts");
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session(256);
        let list = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(list["result"]["accounts"][0]["email"], "desktop@example.invalid");
        assert_eq!(list["result"]["selectedId"], "desktop");
        let started = call(&service, &mut session, "host/thread/start", json!({"cwd":home})).await;
        let thread = started["result"]["thread"]["id"].as_str().unwrap();
        completed_turn(&service, &mut session, thread, "before switch").await;
        let before = call(&service, &mut session, "host/session/open", json!({"session":{"provider":"codex","id":thread},"limit":5})).await;
        assert!(before.get("error").is_none(), "{before}");
        assert_eq!(before["result"]["thread"]["turns"].as_array().unwrap().len(), 1);
        let canceled = call(&service, &mut session, "host/account/login/start", json!({})).await;
        assert!(call(&service, &mut session, "host/account/login/cancel", json!({"loginId":canceled["result"]["loginId"]})).await.get("error").is_none());
        // Dismissing an already discarded login must still allow another attempt.
        assert!(call(&service, &mut session, "host/account/login/cancel", json!({"loginId":canceled["result"]["loginId"]})).await.get("error").is_none());
        let list = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(list["result"]["accounts"].as_array().unwrap().len(), 1);
        let login = call(&service, &mut session, "host/account/login/start", json!({})).await;
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
        let service = HostRpcService::new(Ok(server.clone()), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session(256);
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
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
        let service = HostRpcService::new(Ok(server.clone()), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir, config).await.unwrap();
        let mut session = service.open_session(256);
        let accounts = call(&service, &mut session, "host/account/list", json!({})).await;
        assert!(accounts["result"]["selectedId"].is_null());
        assert!(accounts["result"]["error"].is_string());
        assert!(call(&service, &mut session, "thread/list", json!({})).await.get("error").is_none());
        assert_eq!(call(&service, &mut session, "turn/start", json!({"threadId":"any","clientUserMessageId":"unavailable-account","input":[{"type":"text","text":"must not use a different account"}]})).await["error"]["code"], "account_unavailable");
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":second})).await["result"]["selectedId"], second);
        let started = call(&service, &mut session, "host/thread/start", json!({"cwd":home})).await;
        completed_turn(&service, &mut session, started["result"]["thread"]["id"].as_str().unwrap(), "recovered account").await;
        let logged_out = call(&service, &mut session, "host/account/logout", json!({"accountId":second})).await;
        assert!(logged_out.get("error").is_none(), "{logged_out}");
        let remaining = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(remaining["result"]["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(remaining["result"]["accounts"][0]["id"], "desktop");
        assert!(remaining["result"]["selectedId"].is_null(), "logout must not select another saved account");

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
            DesktopProjectStore::new(home.join("projects.json")),
        );
        service
            .enable_accounts(home.join("accounts"), config)
            .await
            .unwrap();
        let mut session = service.open_session(256);
        let started = call(
            &service,
            &mut session,
            "host/thread/start",
            json!({"cwd":home}),
        )
        .await;
        let thread = started["result"]["thread"]["id"].as_str().unwrap();
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
        let mut login_session = service.open_session(256);
        let login_service = service.clone();
        let login = tokio::spawn(async move {
            call(
                &login_service,
                &mut login_session,
                "host/account/login/start",
                json!({}),
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
async fn logout_removes_credentials_survives_restart_and_allows_login_again() {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("account-fixture.json"), r#"{"type":"chatgpt","email":"desktop@example.invalid","planType":"plus","accountId":"desktop"}"#).unwrap();
        let config = AppServerConfig { codex_home: Some(home.clone()), ..codex_fixture::config(&home) };
        let accounts_dir = directory.path().join("accounts");
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session(256);
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["selectedId"], "desktop");
        let invalid = call(&service, &mut session, "host/account/logout", json!({"accountId":"missing"})).await;
        assert!(invalid.get("error").is_some());
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "desktop");
        // Failed persistence must leave the account and its credentials usable.
        let registry = std::fs::read(accounts_dir.join("accounts.json")).unwrap();
        std::fs::remove_file(accounts_dir.join("accounts.json")).unwrap();
        std::fs::create_dir(accounts_dir.join("accounts.json")).unwrap();
        let failed = call(&service, &mut session, "host/account/logout", json!({"accountId":"desktop"})).await;
        assert!(failed.get("error").is_some());
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "desktop");
        assert_eq!(call(&service, &mut session, "host/account/list", json!({})).await["result"]["selectedId"], "desktop");
        std::fs::remove_dir(accounts_dir.join("accounts.json")).unwrap();
        std::fs::write(accounts_dir.join("accounts.json"), registry).unwrap();
        let logged_out = call(&service, &mut session, "host/account/logout", json!({"accountId":"desktop"})).await;
        assert!(logged_out.get("error").is_none(), "{logged_out}");
        assert!(rpc(&server, "fixture/account/current", json!({})).await["accountId"].is_null());
        assert!(!home.join("account-fixture.json").exists());
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["accounts"], json!([]));
        assert!(listed["result"]["selectedId"].is_null());
        drop(session); drop(service);
        server.shutdown().await.unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir.clone(), config).await.unwrap();
        let mut session = service.open_session(256);
        let listed = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(listed["result"]["accounts"], json!([]));
        assert!(listed["result"]["selectedId"].is_null());
        let login = call(&service, &mut session, "host/account/login/start", json!({})).await;
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
