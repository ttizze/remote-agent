#![cfg(target_os = "macos")]
//! Explicit native acceptance. Never runs in unit-tests or touches installed Bex.
use agent_protocol::{
    browser::*,
    session::{ProviderKind, SessionRef},
};
use host_daemon::browser::Browser;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn action(
    browser: &Browser,
    thread: &SessionRef,
    action: BrowserAction,
) -> Result<BrowserFrame, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let current = loop {
            let result = browser
                .request(&BrowserRequest {
                    thread_id: thread.clone(),
                    tab_id: String::new(),
                    image_id: String::new(),
                    generation: 0,
                    renderer_id: String::new(),
                    action: BrowserAction::Read,
                })
                .await;
            match result {
                Ok(frame) => break frame,
                Err(error)
                    if error.contains("ページが変わりました")
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        };
        if matches!(action, BrowserAction::Read) {
            return Ok(current);
        }
        let result = browser
            .request(&BrowserRequest {
                thread_id: thread.clone(),
                tab_id: current.tab_id,
                image_id: String::new(),
                generation: current.generation,
                renderer_id: String::new(),
                action: action.clone(),
            })
            .await;
        match result {
            // This exact guard rejects the request before dispatch. An error
            // during capture may follow an applied action and must not replay it.
            Err(error)
                if error == "表示中のページが変わりました。最新の画面で再試行してください。"
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            result => return result,
        }
    }
}
async fn evaluate(browser: &Browser, thread: &SessionRef, expression: &str) -> Value {
    let frame = action(
        browser,
        thread,
        BrowserAction::Evaluate {
            expression: expression.into(),
        },
    )
    .await
    .unwrap();
    serde_json::from_str(&frame.value).unwrap()
}
async fn wait(browser: &Browser, thread: &SessionRef, expression: &str, expected: Value) {
    let mut observed = String::new();
    for _ in 0..100 {
        let result = action(
            browser,
            thread,
            BrowserAction::Evaluate {
                expression: expression.into(),
            },
        )
        .await;
        match result {
            Ok(frame) => {
                if serde_json::from_str::<Value>(&frame.value).ok().as_ref() == Some(&expected) {
                    return;
                }
                observed = frame.value;
            }
            Err(error) => observed = error,
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let diagnostic = if expected == json!("Popup") {
        Some(evaluate(browser,thread,"({clicks:window.popupClicks,child:!!window.child,activation:navigator.userActivation.isActive,rect:document.querySelector('#popup')?.getBoundingClientRect().toJSON(),height:innerHeight,scroll:scrollY,focus:document.activeElement.id,events:window.mouseEvents})").await)
    } else {
        None
    };
    panic!(
        "native page did not reach expected state: {expression}; observed {observed}; diagnostic {diagnostic:?}"
    );
}

#[tokio::test]
#[ignore = "explicit native UI check; build agent-desktop first"]
async fn shared_browser_live() {
    let target = agent_process::runtime_directory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut desktop = tokio::process::Command::new(target.join("agent-desktop"))
        .env("AGENT_STATE_DIR", root.path())
        .env("AGENT_ISOLATED_HOST", "1")
        .env("AGENT_HOST_DAEMON", "/usr/bin/false")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if agent_local_host::browser::read_renderer(root.path()).is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        desktop.try_wait().unwrap().is_none(),
        "native desktop exited during launch"
    );
    agent_local_host::browser::read_renderer(root.path()).unwrap();
    let browser = Browser::start(root.path().join("browser")).await.unwrap();
    let a = SessionRef::new(ProviderKind::Codex, "native-a".into()).unwrap();
    let b = SessionRef::new(ProviderKind::Codex, "native-b".into()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut bytes = [0; 8192];
                let n = stream.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                seen.lock().unwrap().push(path.clone());
                if path == "/slow" {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                }
                let body = if path == "/imported" {
                    if request.contains("fixture_import=imported") {
                        "<title>Imported</title>"
                    } else {
                        "<title>Missing</title>"
                    }
                } else if path == "/authenticated" {
                    if request.contains("login=native-session") {
                        "<title>Authenticated</title>"
                    } else {
                        "<title>Logged out</title>"
                    }
                } else if path == "/popup" {
                    "<title>Popup</title>Embedded popup"
                } else {
                    r#"<!doctype html><title>Fixture</title><body style="margin:0;height:3000px"><input id="field" style="position:absolute;left:20px;top:20px;width:300px;height:44px" oninput="window.trusted=event.isTrusted"><input id="secret" type="password" value="private-fixture-password"><button id="popup" style="position:absolute;left:20px;top:100px;width:220px;height:44px" onclick="window.popupClicks=(window.popupClicks||0)+1;window.child=window.open('/popup')">Popup</button><select id="options"><option value="a">A</option><option value="b">B</option></select><script>window.addEventListener('message',e=>window.received=e.data);window.mouseEvents=[];for(const type of ['mousedown','mouseup','click'])document.addEventListener(type,e=>window.mouseEvents.push([type,e.target.id,e.clientX,e.clientY,e.isTrusted]));</script>"#
                };
                let cookie = if path == "/session" {
                    "Set-Cookie: login=native-session; HttpOnly; SameSite=Lax; Path=/; Max-Age=3600\r\n"
                } else {
                    ""
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{cookie}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    let url = format!("http://{address}");
    let empty = action(&browser, &a, BrowserAction::Read).await.unwrap();
    assert!(empty.tabs.is_empty());
    let renderer = agent_local_host::browser::read_renderer(root.path()).unwrap();
    use agent_protocol::browser_native::{BrowserCookie, Capture, NativeAction, NativeRequest};
    let imported = agent_transport::native_browser::request(
        &renderer.socket,
        &NativeRequest {
            scope: a.to_string(),
            displayed: None,
            image_id: String::new(),
            renderer_id: String::new(),
            capture: Capture::None,
            action: NativeAction::ImportCookies(vec![BrowserCookie {
                domain: "127.0.0.1".into(),
                name: "fixture_import".into(),
                value: "imported".to_string().into(),
                path: "/".into(),
                secure: false,
                http_only: true,
                same_site: 1,
                expires: Some(
                    (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 3600) as i64,
                ),
            }]),
        },
    )
    .await
    .unwrap();
    assert!(
        imported.tabs.is_empty(),
        "importing login state must not create a tab"
    );
    action(
        &browser,
        &a,
        BrowserAction::Navigate {
            url: format!("{url}/"),
        },
    )
    .await
    .unwrap();
    wait(
        &browser,
        &a,
        "document.readyState==='complete' && !!document.getElementById('field')",
        json!(true),
    )
    .await;
    assert_eq!(
        evaluate(&browser, &a, "fetch('/imported').then(r=>r.text())").await,
        "<title>Imported</title>"
    );
    assert!(
        !evaluate(&browser, &a, "document.cookie")
            .await
            .as_str()
            .unwrap()
            .contains("fixture_import")
    );
    let identity=evaluate(&browser,&a,"({ua:navigator.userAgent,webdriver:navigator.webdriver,node:typeof require,ipc:typeof window.webkit?.messageHandlers?.ipc,native:typeof window.webkit?.messageHandlers?.bexInput})").await;
    assert!(!identity["ua"].as_str().unwrap().contains("HeadlessChrome"));
    assert_eq!(identity["webdriver"], false);
    assert_eq!(identity["node"], "undefined");
    assert_eq!(identity["ipc"], "undefined");
    assert_eq!(identity["native"], "undefined");
    let snapshot = action(&browser, &a, BrowserAction::Snapshot).await.unwrap();
    assert!(snapshot.image.starts_with(&[0xff, 0xd8]));
    let dimensions = image::ImageReader::new(std::io::Cursor::new(&snapshot.image))
        .with_guessed_format()
        .unwrap()
        .into_dimensions()
        .unwrap();
    assert_eq!(
        dimensions,
        (snapshot.width, snapshot.height),
        "phone input uses CSS viewport coordinates"
    );
    assert_eq!(snapshot.input_kind, None);
    assert!(!snapshot.page_text.contains("private-fixture-password"));
    assert!(snapshot.page_text.contains("ref="));
    let filled = action(
        &browser,
        &a,
        BrowserAction::Fill {
            selector: "#field".into(),
            text: "phone-tool".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(filled.input_kind, Some(BrowserInputKind::Text));
    assert_eq!(
        evaluate(&browser, &a, "document.querySelector('#field').value").await,
        "phone-tool"
    );
    assert_eq!(evaluate(&browser, &a, "window.trusted").await, true);
    let dialog_browser = browser.clone();
    let dialog_thread = a.clone();
    let dialog = tokio::spawn(async move {
        action(
            &dialog_browser,
            &dialog_thread,
            BrowserAction::Evaluate {
                expression: "confirm('native confirmation')".into(),
            },
        )
        .await
    });
    let mut shown = false;
    for _ in 0..100 {
        if action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .dialog
            .is_some_and(|d| d.message.contains("native confirmation"))
        {
            shown = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(shown, "the native dialog must reach the shared browser UI");
    action(
        &browser,
        &a,
        BrowserAction::Dialog {
            accept: false,
            text: String::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&dialog.await.unwrap().unwrap().value).unwrap(),
        false
    );
    action(
        &browser,
        &a,
        BrowserAction::Key {
            key: BrowserKey::Backspace,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        evaluate(&browser, &a, "document.querySelector('#field').value").await,
        "phone-too"
    );
    action(
        &browser,
        &a,
        BrowserAction::Key {
            key: BrowserKey::SelectAll,
        },
    )
    .await
    .unwrap();
    let typed = action(
        &browser,
        &a,
        BrowserAction::Type {
            text: "スマホから入力🙂".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(typed.input_kind, Some(BrowserInputKind::Text));
    assert_eq!(
        evaluate(&browser, &a, "document.querySelector('#field').value").await,
        "スマホから入力🙂"
    );
    evaluate(&browser, &a, "document.querySelector('#secret').focus()").await;
    assert_eq!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .input_kind,
        Some(BrowserInputKind::Password)
    );
    evaluate(&browser, &a, "document.activeElement.blur()").await;
    assert_eq!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .input_kind,
        None
    );
    action(
        &browser,
        &a,
        BrowserAction::SelectOption {
            selector: "#options".into(),
            value: "b".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        evaluate(&browser, &a, "document.querySelector('#options').value").await,
        "b"
    );
    evaluate(&browser, &a, "localStorage.setItem('shared','yes')").await;
    action(
        &browser,
        &b,
        BrowserAction::Navigate {
            url: format!("{url}/session"),
        },
    )
    .await
    .unwrap();
    wait(&browser, &b, "document.readyState", json!("complete")).await;
    assert_eq!(
        evaluate(&browser, &b, "localStorage.getItem('shared')").await,
        "yes"
    );
    action(
        &browser,
        &a,
        BrowserAction::OpenTab {
            url: format!("{url}/authenticated"),
        },
    )
    .await
    .unwrap();
    wait(&browser, &a, "document.title", json!("Authenticated")).await;
    assert!(
        !evaluate(&browser, &a, "document.cookie")
            .await
            .as_str()
            .unwrap()
            .contains("native-session")
    );
    assert!(
        action(
            &browser,
            &b,
            BrowserAction::SelectTab {
                id: snapshot.tab_id.clone()
            }
        )
        .await
        .is_err()
    );
    action(
        &browser,
        &a,
        BrowserAction::SelectTab {
            id: snapshot.tab_id.clone(),
        },
    )
    .await
    .unwrap();
    // Repeated transitions exercise popup capture races while every click stays single.
    for expected_clicks in 1..=5 {
        let popup_result = action(
            &browser,
            &a,
            BrowserAction::ClickElement {
                selector: "#popup".into(),
            },
        )
        .await;
        if let Err(error) = popup_result {
            let diagnostic = evaluate(
                &browser,
                &a,
                "({title:document.title,ready:document.readyState,clicks:window.popupClicks,openerClicks:window.opener?.popupClicks,hasChild:!!window.child})",
            ).await;
            panic!("popup action failed: {error}; diagnostic {diagnostic}");
        }
        wait(&browser, &a, "document.title", json!("Popup")).await;
        assert_eq!(evaluate(&browser, &a, "!!window.opener").await, true);
        assert_eq!(
            evaluate(&browser, &a, "window.opener.popupClicks").await,
            json!(expected_clicks)
        );
        evaluate(
            &browser,
            &a,
            "window.opener.postMessage('retained-opener',location.origin)",
        )
        .await;
        let _ = action(
            &browser,
            &a,
            BrowserAction::Evaluate {
                expression: "window.close()".into(),
            },
        )
        .await;
        wait(&browser, &a, "window.received", json!("retained-opener")).await;
    }
    let kept = action(&browser, &a, BrowserAction::Read).await.unwrap();
    let _ = action(
        &browser,
        &a,
        BrowserAction::Navigate {
            url: format!("{url}/slow"),
        },
    )
    .await;
    wait(
        &browser,
        &a,
        "location.pathname==='/slow' && document.readyState==='complete'",
        json!(true),
    )
    .await;
    assert_eq!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .tabs,
        kept.tabs
            .iter()
            .map(|t| BrowserTab {
                id: t.id.clone(),
                title: if t.id == kept.tab_id {
                    "Fixture".into()
                } else {
                    t.title.clone()
                },
                url: if t.id == kept.tab_id {
                    format!("{url}/slow")
                } else {
                    t.url.clone()
                }
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.as_str() == "/slow")
            .count(),
        1
    );
    let error=action(&browser,&a,BrowserAction::Evaluate{expression:"new Promise(resolve=>setTimeout(()=>{window.once=(window.once||0)+1;resolve(window.once)},9000))".into()}).await.unwrap_err();
    assert!(error.contains("タイムアウト"));
    wait(&browser, &a, "window.once", json!(1)).await;
    assert_eq!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .tabs
            .len(),
        kept.tabs.len()
    );
    let before_restart = action(&browser, &a, BrowserAction::Read).await.unwrap();
    let owner_id = before_restart.renderer_id.clone();
    browser.shutdown().await;
    let browser = Browser::start(root.path().join("browser")).await.unwrap();
    assert_eq!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .tabs,
        before_restart.tabs,
        "Host restart must not recreate native tabs"
    );
    // Restart only this isolated desktop. Restore the same tab IDs and the
    // persistent shared login profile rather than creating a new conversation.
    desktop.kill().await.unwrap();
    desktop = tokio::process::Command::new(target.join("agent-desktop"))
        .env("AGENT_STATE_DIR", root.path())
        .env("AGENT_ISOLATED_HOST", "1")
        .env("AGENT_HOST_DAEMON", "/usr/bin/false")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if agent_local_host::browser::read_renderer(root.path()).is_ok_and(|r| r.id != owner_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let restored = action(&browser, &a, BrowserAction::Read).await.unwrap();
    assert_ne!(restored.renderer_id, owner_id);
    assert_eq!(restored.tab_id, before_restart.tab_id);
    assert_eq!(
        restored.tabs.iter().map(|t| &t.id).collect::<Vec<_>>(),
        before_restart
            .tabs
            .iter()
            .map(|t| &t.id)
            .collect::<Vec<_>>()
    );
    action(
        &browser,
        &b,
        BrowserAction::Navigate {
            url: format!("{url}/authenticated"),
        },
    )
    .await
    .unwrap();
    wait(&browser, &b, "document.title", json!("Authenticated")).await;
    // The real stdio MCP bridge reads the same conversation as the phone RPC.
    let config = browser.provider_config(&a.to_string()).unwrap();
    let mut mcp = tokio::process::Command::new(env!("CARGO_BIN_EXE_host-daemon"))
        .args(
            config["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap()),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    use agent_transport::peer::{JsonlReader, JsonlWriter};
    let mut input = JsonlWriter::new(mcp.stdin.take().unwrap());
    let mut output = JsonlReader::new(mcp.stdout.take().unwrap());
    input.write_line(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"preview_status","arguments":{}}}).to_string()).await.unwrap();
    let line = tokio::time::timeout(Duration::from_secs(8), output.read_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["active_tab"],
        restored.tab_id
    );
    drop(input);
    assert!(mcp.wait().await.unwrap().success());
    let current = action(&browser, &a, BrowserAction::Read).await.unwrap();
    for tab in current.tabs {
        action(&browser, &a, BrowserAction::CloseTab { id: tab.id })
            .await
            .unwrap();
    }
    assert!(
        action(&browser, &a, BrowserAction::Read)
            .await
            .unwrap()
            .tabs
            .is_empty()
    );
    assert!(
        !action(&browser, &b, BrowserAction::Read)
            .await
            .unwrap()
            .tabs
            .is_empty()
    );
    desktop.kill().await.unwrap();
    browser.shutdown().await;
    server.abort();
}
