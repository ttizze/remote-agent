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
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"));
    command.args(
        config["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap()),
    );
    for (key, value) in config["env"].as_object().unwrap() {
        command.env(key, value.as_str().unwrap());
    }
    let mut child = command
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
        // Loading the Host executable from build storage can exceed five seconds
        // on macOS before main runs. Keep subsequent IPC deadlines short.
        let deadline = Duration::from_secs(if id == 1 { 30 } else { 5 });
        let line = tokio::time::timeout(deadline, output.read_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        match id {
            1 => assert_eq!(response["result"]["protocolVersion"], "2025-06-18"),
            2 => {
                let tools = response["result"]["tools"].as_array().unwrap();
                assert_eq!(tools.len(), 5);
                let names = tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    names,
                    [
                        "bex_browser",
                        "preview_close",
                        "preview_list",
                        "preview_recording_start",
                        "preview_recording_stop",
                    ]
                    .into_iter()
                    .collect()
                );
                let tool = tools
                    .iter()
                    .find(|tool| tool["name"] == "bex_browser")
                    .unwrap();
                assert_eq!(tool["name"], "bex_browser");
                assert!(
                    !tool["inputSchema"]["properties"]["action"]["enum"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|action| action == "wait_for_user")
                );
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
    let session = agent_domain::ThreadId::new("browser-test").unwrap();
    let mut mcp = browser.provider_config(session.as_str()).unwrap();
    mcp["command"] = env!("CARGO_BIN_EXE_host-daemon").into();
    std::fs::create_dir_all(root.path().join("codex")).unwrap();
    let server = codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig {
        program: std::env::var_os("BEX_CODEX_TEST_PROGRAM")
            .map(PathBuf::from)
            .unwrap_or_else(|| codex_app_server::AppServerConfig::default().program),
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
        .request(&agent_protocol::browser::BrowserRequest {
            thread_id: session,
            tab_id: String::new(),
            image_id: String::new(),
            action: agent_protocol::browser::BrowserAction::Read,
        })
        .await
        .unwrap();
    assert_eq!(phone.tabs.len(), 1, "phone and Codex must share one tab");
    assert!(result.to_string().contains(&phone.tab_id));
    server.shutdown().await.unwrap();
    browser.shutdown().await;
}

#[tokio::test]
async fn cancelled_mcp_call_keeps_the_bridge_responsive() {
    use agent_protocol::browser::BrowserFrame;
    let root = tempfile::Builder::new()
        .prefix("bex-cancel-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("bridge.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
        .arg("browser-mcp")
        .arg("--socket")
        .arg(&socket)
        .arg("--thread")
        .arg("cancel-thread")
        .env("AGENT_TOOLS_TOKEN", "test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = JsonlWriter::new(child.stdin.take().unwrap());
    let mut output = JsonlReader::new(child.stdout.take().unwrap());
    // Complete MCP initialization before timing bridge cancellation.
    input.write_line(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cancel-test","version":"1"}}}).to_string()).await.unwrap();
    let initialized = tokio::time::timeout(Duration::from_secs(30), output.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(serde_json::from_str::<Value>(&initialized).unwrap()["result"].is_object());
    input.write_line(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bex_browser","arguments":{"action":"screenshot"}}}).to_string()).await.unwrap();
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut pending = JsonlReader::new(connection);
    pending.read_line().await.unwrap().unwrap();
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
    assert!(
        tokio::time::timeout(Duration::from_secs(3), pending.read_line())
            .await
            .unwrap()
            .unwrap()
            .is_none(),
        "cancellation must close the in-flight Host bridge connection"
    );
    input.write_line(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bex_browser","arguments":{"action":"screenshot"}}}).to_string()).await.unwrap();
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (read, write) = connection.into_split();
    JsonlReader::new(read).read_line().await.unwrap().unwrap();
    JsonlWriter::new(write)
        .write_line(&serde_json::to_string(&Ok::<_, String>(BrowserFrame::default())).unwrap())
        .await
        .unwrap();
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
}
