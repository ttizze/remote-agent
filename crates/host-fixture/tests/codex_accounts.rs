use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{CodexRpcService, CodexSession, DesktopProjectStore};
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
    service: &CodexRpcService,
    session: &mut CodexSession,
    method: &str,
    params: Value,
) -> Value {
    service
        .dispatch_request(
            session.id(),
            json!({"id":42,"method":method,"params":params}).to_string(),
        )
        .await
        .unwrap();
    loop {
        let line = session.recv().await.expect("RPC session closed");
        assert!(
            !line.contains("invalid-test-signature"),
            "Host leaked a credential into the mobile session"
        );
        assert!(
            !line.contains("account/chatgptAuthTokens/refresh"),
            "Host forwarded a private credential request to mobile"
        );
        let result: Value = serde_json::from_str(&line).unwrap();
        if result["id"] == 42 {
            return result;
        }
    }
}

async fn completed_turn(
    service: &CodexRpcService,
    session: &mut CodexSession,
    thread: &str,
    text: &str,
) -> String {
    let result = call(
        service,
        session,
        "turn/start",
        json!({"threadId":thread,"input":[{"type":"text","text":text}]}),
    )
    .await;
    assert!(result.get("error").is_none(), "{result}");
    let id = result["result"]["turn"]["id"].as_str().unwrap().to_owned();
    loop {
        let event: Value = serde_json::from_str(&session.recv().await.unwrap()).unwrap();
        if event["method"] == "turn/completed" && event["params"]["turn"]["id"] == id {
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
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(home.join("projects.json")));
        let accounts_dir = directory.path().join("accounts");
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session(256);
        let list = call(&service, &mut session, "host/account/list", json!({})).await;
        assert_eq!(list["result"]["accounts"][0]["email"], "desktop@example.invalid");
        assert_eq!(list["result"]["selectedId"], "desktop");
        let started = call(&service, &mut session, "thread/start", json!({"cwd":home})).await;
        let thread = started["result"]["thread"]["id"].as_str().unwrap();
        completed_turn(&service, &mut session, thread, "before switch").await;
        let before = call(&service, &mut session, "host/thread/read", json!({"threadId":thread,"includeTurns":true})).await;
        assert!(before.get("error").is_none(), "{before}");
        assert_eq!(before["result"]["thread"]["turns"].as_array().unwrap().len(), 1);
        let canceled = call(&service, &mut session, "host/account/login/start", json!({})).await;
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
        let refreshed = call(&service, &mut session, "fixture/account/refresh", json!({"previousAccountId":"desktop"})).await;
        assert_eq!(refreshed["result"], json!({"accountId":"desktop","hasToken":true}));
        let after = call(&service, &mut session, "host/thread/read", json!({"threadId":thread,"includeTurns":true})).await;
        assert_eq!(before["result"]["thread"], after["result"]["thread"]);
        completed_turn(&service, &mut session, thread, "after switch").await;
        let after = call(&service, &mut session, "host/thread/read", json!({"threadId":thread,"includeTurns":true})).await;
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
        Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir.clone(), config.clone()).await.unwrap();
        let mut session = service.open_session(256);
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        let missing = call(&service, &mut session, "fixture/account/refresh", json!({"previousAccountId":"missing"})).await;
        assert_eq!(missing["result"]["hasToken"], false);
        let refreshed = call(&service, &mut session, "fixture/account/refresh", json!({"previousAccountId":"desktop"})).await;
        assert_eq!(refreshed["result"]["accountId"], "desktop");
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "second");
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":"desktop"})).await["result"]["selectedId"], "desktop");
        assert_eq!(rpc(&server, "fixture/account/current", json!({})).await["accountId"], "desktop");
        drop(session); drop(service);
        Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
        std::fs::remove_file(home.join("account-fixture.json")).unwrap();
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(home.join("projects.json")));
        service.enable_accounts(accounts_dir, config).await.unwrap();
        let mut session = service.open_session(256);
        let accounts = call(&service, &mut session, "host/account/list", json!({})).await;
        assert!(accounts["result"]["selectedId"].is_null());
        assert!(accounts["result"]["error"].is_string());
        assert!(call(&service, &mut session, "thread/list", json!({})).await.get("error").is_none());
        assert_eq!(call(&service, &mut session, "turn/start", json!({"threadId":"any","input":[{"type":"text","text":"must not use a different account"}]})).await["error"]["code"], "account_unavailable");
        assert_eq!(call(&service, &mut session, "host/account/select", json!({"accountId":second})).await["result"]["selectedId"], second);
        let started = call(&service, &mut session, "thread/start", json!({"cwd":home})).await;
        completed_turn(&service, &mut session, started["result"]["thread"]["id"].as_str().unwrap(), "recovered account").await;
        drop(session); drop(service);
        Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
    }).await.expect("account switching stalled");
}

#[tokio::test]
async fn fork_inherits_only_through_selected_completed_turn_and_preserves_original() {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let config = AppServerConfig {
            codex_home: Some(directory.path().to_owned()),
            ..codex_fixture::config(directory.path())
        };
        let server = Arc::new(CodexAppServer::spawn(config).await.unwrap());
        let service = CodexRpcService::new(
            server.clone(),
            DesktopProjectStore::new(directory.path().join("projects.json")),
        );
        let mut session = service.open_session(256);
        let start = call(
            &service,
            &mut session,
            "thread/start",
            json!({"cwd":directory.path()}),
        )
        .await;
        let thread = start["result"]["thread"]["id"].as_str().unwrap();
        let first = completed_turn(&service, &mut session, thread, "inherited question").await;
        completed_turn(&service, &mut session, thread, "excluded question").await;
        let original = call(
            &service,
            &mut session,
            "host/thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await;
        let fork = call(
            &service,
            &mut session,
            "thread/fork",
            json!({"threadId":thread,"lastTurnId":first,"excludeTurns":true}),
        )
        .await;
        assert!(fork.get("error").is_none(), "{fork}");
        let fork_id = fork["result"]["thread"]["id"].as_str().unwrap();
        assert_ne!(thread, fork_id);
        let fork = call(
            &service,
            &mut session,
            "host/thread/read",
            json!({"threadId":fork_id,"includeTurns":true}),
        )
        .await;
        assert!(original.get("error").is_none(), "{original}");
        assert!(fork.get("error").is_none(), "{fork}");
        let original_turns = original["result"]["thread"]["turns"].as_array().unwrap();
        assert_eq!(original_turns.len(), 2);
        assert_eq!(original_turns[0]["id"], first);
        assert_eq!(
            fork["result"]["thread"]["turns"],
            json!([original_turns[0]])
        );
        completed_turn(&service, &mut session, fork_id, "fork continuation").await;
        let unchanged = call(
            &service,
            &mut session,
            "host/thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await;
        assert_eq!(unchanged["result"]["thread"], original["result"]["thread"]);
        drop(session);
        drop(service);
        Arc::try_unwrap(server)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    })
    .await
    .expect("forking stalled");
}

#[tokio::test]
async fn helper_initialization_does_not_block_completed_turns() {
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let config = codex_fixture::config(home);
        let server = Arc::new(CodexAppServer::spawn(config.clone()).await.unwrap());
        let service = CodexRpcService::new(
            server.clone(),
            DesktopProjectStore::new(home.join("projects.json")),
        );
        service
            .enable_accounts(home.join("accounts"), config)
            .await
            .unwrap();
        let mut session = service.open_session(256);
        let started = call(&service, &mut session, "thread/start", json!({"cwd":home})).await;
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
            "host/thread/read",
            json!({"threadId":thread,"includeTurns":true}),
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
        Arc::try_unwrap(server)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    })
    .await
    .expect("concurrent account/turn fixture stalled");
}
