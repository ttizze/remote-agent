#![cfg(unix)]
use agent_transport::peer::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};

#[tokio::test]
async fn stdio_browser_tool_is_typed_and_does_not_expose_generic_execution() {
    let root = tempfile::tempdir().unwrap();
    let browser = host_daemon::browser::Browser::start(root.path().join("profile"))
        .await
        .unwrap();
    let config = browser.provider_config("test-thread").unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
        .args(
            config["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = JsonlWriter::new(child.stdin.take().unwrap());
    let mut output = JsonlReader::new(child.stdout.take().unwrap());
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}),
        ),
        (2, "tools/list", json!({})),
        (
            3,
            "tools/call",
            json!({"name":"bex_browser","arguments":{"action":"evaluate","expression":"1+1"}}),
        ),
    ] {
        input
            .write_line(
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
            )
            .await
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(5), output.read_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        match id {
            1 => assert_eq!(response["result"]["protocolVersion"], "2025-06-18"),
            2 => {
                assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 1);
                assert_eq!(response["result"]["tools"][0]["name"], "bex_browser");
            }
            _ => assert_eq!(response["result"]["isError"], true),
        }
    }
    drop(input);
    assert!(child.wait().await.unwrap().success());
    browser.shutdown().await;
}

#[tokio::test]
#[ignore = "requires installed Codex, Chrome and the process supervisor; no model turn is submitted"]
async fn installed_codex_exposes_the_same_browser_as_the_phone() {
    let root = tempfile::tempdir().unwrap();
    let browser = host_daemon::browser::Browser::start(root.path().join("profile"))
        .await
        .unwrap();
    let mut mcp = browser.provider_config("startup-scope").unwrap();
    mcp["command"] = env!("CARGO_BIN_EXE_host-daemon").into();
    mcp["tool_timeout_sec"] = 1800.into();
    std::fs::create_dir_all(root.path().join("codex")).unwrap();
    let server = codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig {
        program: std::env::var_os("BEX_CODEX_TEST_PROGRAM")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/Applications/ChatGPT.app/Contents/Resources/codex".into()),
        codex_home: Some(root.path().join("codex")),
        ..Default::default()
    })
    .await
    .unwrap();
    let started = server
        .request::<_, Value>(
            "thread/start",
            &json!({"cwd":root.path(),"config":{"mcp_servers.bex_browser":mcp}}),
        )
        .await
        .unwrap()
        .outcome
        .unwrap();
    let thread = started["thread"]["id"].as_str().unwrap();
    browser
        .bind_scope("startup-scope".into(), thread.into())
        .await;
    let mut found = false;
    for _ in 0..20 {
        let status = server
            .request::<_, Value>("mcpServerStatus/list", &json!({"threadId":thread}))
            .await
            .unwrap()
            .outcome
            .unwrap();
        found = status["data"].as_array().is_some_and(|servers| {
            servers
                .iter()
                .any(|s| s["name"] == "bex_browser" && s["tools"].get("bex_browser").is_some())
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        found,
        "Codex must discover the conversation-scoped MCP tool"
    );
    let result=server.request::<_,Value>("mcpServer/tool/call",&json!({"threadId":thread,"server":"bex_browser","tool":"bex_browser","arguments":{"action":"screenshot"}})).await.unwrap().outcome.unwrap();
    assert!(
        result.to_string().contains("image/jpeg"),
        "Codex receives the Host screenshot"
    );
    let phone = browser
        .request(
            "phone",
            &agent_protocol::browser::BrowserRequest {
                thread_id: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: thread.into(),
                },
                control_token: String::new(),
                tab_id: String::new(),
                image_id: String::new(),
                action: agent_protocol::browser::BrowserAction::Read,
            },
        )
        .await
        .unwrap();
    assert_eq!(phone.tabs.len(), 1, "phone and Codex must share one tab");
    assert!(result.to_string().contains(&phone.tab_id));
    server.shutdown().await.unwrap();
    browser.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Chrome and the process supervisor"]
async fn cancelled_mcp_wait_keeps_the_bridge_responsive() {
    use agent_protocol::browser::{BrowserAction, BrowserControl, BrowserFrame, BrowserRequest};
    let root = tempfile::tempdir().unwrap();
    let browser = host_daemon::browser::Browser::start(root.path().join("profile"))
        .await
        .unwrap();
    let config = browser.provider_config("cancel-thread").unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
        .args(
            config["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = JsonlWriter::new(child.stdin.take().unwrap());
    let mut output = JsonlReader::new(child.stdout.take().unwrap());
    input.write_line(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bex_browser","arguments":{"action":"wait_for_user"}}}).to_string()).await.unwrap();
    let request = |frame: &BrowserFrame, action| BrowserRequest {
        thread_id: agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "cancel-thread".into(),
        },
        tab_id: frame.tab_id.clone(),
        control_token: frame.control_token.clone(),
        image_id: frame.image_id.clone(),
        action,
    };
    let mut frame = BrowserFrame::default();
    for _ in 0..30 {
        frame = browser
            .request("phone", &request(&frame, BrowserAction::Read))
            .await
            .unwrap();
        if frame.control == BrowserControl::AwaitingHuman {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(frame.control, BrowserControl::AwaitingHuman);
    input
        .write_line(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}})
                .to_string(),
        )
        .await
        .unwrap();
    input
        .write_line(&json!({"jsonrpc":"2.0","id":2,"method":"ping"}).to_string())
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), output.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&response).unwrap()["id"], 2);
    let frame = browser
        .request("phone", &request(&frame, BrowserAction::TakeControl))
        .await
        .unwrap();
    browser
        .request("phone", &request(&frame, BrowserAction::ReleaseControl))
        .await
        .unwrap();
    input.write_line(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bex_browser","arguments":{"action":"screenshot"}}}).to_string()).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), output.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 3);
    assert_eq!(response["result"]["isError"], false);
    drop(input);
    assert!(child.wait().await.unwrap().success());
    browser.shutdown().await;
}
