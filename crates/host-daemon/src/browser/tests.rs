use super::*;
use agent_protocol::session::{ProviderKind, SessionRef};

fn request(thread: &SessionRef, frame: &BrowserFrame, action: BrowserAction) -> BrowserRequest {
    BrowserRequest {
        thread_id: thread.clone(),
        tab_id: frame.tab_id.clone(),
        image_id: frame.image_id.clone(),
        renderer_id: String::new(),
        generation: frame.generation,
        action,
    }
}

/// Exercises only a temporary BEX profile and a local, deterministic web fixture.
#[tokio::test]
#[ignore = "requires Chrome/Chromium and agent-provider-supervisor"]
async fn shared_browser_live() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut bytes = [0; 8192];
                let length = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..length]);
                if request.starts_with("GET /slow ") {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                let body = if request.starts_with("GET /popup ") {
                    "<title>Popup</title>Popup"
                } else if request.starts_with("GET /authenticated ") {
                    if request.lines().any(|line| {
                        line.to_ascii_lowercase().starts_with("cookie:")
                            && line.contains("imported_session=fixture-login")
                    }) {
                        "<title>Logged in</title>"
                    } else {
                        "<title>Logged out</title>"
                    }
                } else if request.starts_with("GET /verify ") {
                    "<script>document.title=document.cookie+' '+localStorage.getItem('agent-fixture')</script>"
                } else {
                    r#"<body style="margin:0;height:3000px"><input style="position:absolute;left:20px;top:20px;width:300px;height:40px" oninput="document.title=this.value"><button style="position:absolute;left:20px;top:90px;width:200px;height:40px" onclick="window.open('/popup')">Popup</button><script>document.cookie='agent_fixture=persisted; Max-Age=3600; SameSite=Lax';localStorage.setItem('agent-fixture','stored');document.title='Fixture'</script>"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("profile");
    let browser = Browser::start(profile.clone()).await.unwrap();
    let thread_a = SessionRef::new(ProviderKind::Codex, "thread-a".into()).unwrap();
    let thread_b = SessionRef::new(ProviderKind::Codex, "thread-b".into()).unwrap();
    let scope_a = thread_a.to_string();
    let scope_b = thread_b.to_string();
    browser
        .agent(
            &scope_a,
            BrowserAction::Navigate {
                url: format!("http://{address}/"),
            },
        )
        .await
        .unwrap();
    let mut initial = browser
        .agent(&scope_a, BrowserAction::Snapshot)
        .await
        .unwrap();
    assert!(initial.image.starts_with(&[0xff, 0xd8]));
    for _ in 0..20 {
        if initial.tabs.iter().any(|tab| tab.title == "Fixture") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        initial = browser
            .request(&request(&thread_a, &initial, BrowserAction::Read))
            .await
            .unwrap();
    }
    assert!(initial.tabs.iter().any(|tab| tab.title == "Fixture"));
    {
        let mut state = browser.state.lock().await;
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome.attach(&initial.tab_id).await.unwrap();
        let user_agent = chrome
            .evaluate(&session, "navigator.userAgent")
            .await
            .unwrap();
        assert!(user_agent.as_str().unwrap().contains("Chrome/"));
        assert!(!user_agent.as_str().unwrap().contains("HeadlessChrome"));
        let window = chrome
            .call(
                None,
                "Browser.getWindowForTarget",
                serde_json::json!({"targetId":initial.tab_id}),
            )
            .await
            .unwrap();
        assert!(
            window["windowId"].as_i64().is_some(),
            "tabs must belong to a normal Chrome window"
        );
    }
    let clicked = browser
        .request(&request(
            &thread_a,
            &initial,
            BrowserAction::Click { x: 100., y: 40. },
        ))
        .await
        .unwrap();
    let typed = browser
        .request(&request(
            &thread_a,
            &clicked,
            BrowserAction::Type {
                text: "日本語 input".into(),
            },
        ))
        .await
        .unwrap();
    browser
        .agent(
            &scope_a,
            BrowserAction::Type {
                text: "AI input".into(),
            },
        )
        .await
        .unwrap();
    let observed = browser
        .agent(&scope_a, BrowserAction::Snapshot)
        .await
        .unwrap();
    assert_eq!(observed.tab_id, typed.tab_id);
    assert_eq!(observed.tabs[0].title, "日本語 inputAI input");
    assert!(
        observed.page_text.contains("textbox"),
        "snapshot includes the page's input controls"
    );
    let other_viewer = browser
        .request(&request(&thread_a, &typed, BrowserAction::Read))
        .await
        .unwrap();
    assert_eq!(other_viewer.tabs, observed.tabs);
    browser
        .request(&request(
            &thread_a,
            &other_viewer,
            BrowserAction::Type {
                text: "other".into(),
            },
        ))
        .await
        .unwrap();
    browser
        .agent(
            &scope_a,
            BrowserAction::Fill {
                selector: "input".into(),
                text: "AI input".into(),
            },
        )
        .await
        .unwrap();
    browser
        .agent(
            &scope_a,
            BrowserAction::ClickElement {
                selector: "button".into(),
            },
        )
        .await
        .unwrap();
    let mut popup = typed;
    for _ in 0..20 {
        popup = browser
            .request(&request(&thread_a, &popup, BrowserAction::Read))
            .await
            .unwrap();
        if popup.tabs.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(popup.tabs.len(), 2);
    assert_ne!(popup.tab_id, initial.tab_id);
    assert!(
        browser
            .request(&request(
                &thread_a,
                &initial,
                BrowserAction::Click { x: 20.0, y: 20.0 }
            ))
            .await
            .is_err(),
        "stale tab input must fail"
    );
    let status = browser.agent(&scope_b, BrowserAction::Read).await.unwrap();
    let mut other = browser
        .request(&request(&thread_b, &status, BrowserAction::Read))
        .await
        .unwrap();
    assert_eq!(other.tabs.len(), 1);
    // The initial screenshot can be replaced by a screencast frame encoded at
    // a different JPEG quality. Only an identical image ID suppresses its bytes.
    let unchanged = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let current = browser
                .request(&request(&thread_b, &other, BrowserAction::Read))
                .await
                .unwrap();
            if current.image_id == other.image_id {
                break current;
            }
            assert!(!current.image.is_empty(), "new images must be delivered");
            other = current;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the idle blank page must settle on an unchanged image");
    assert!(
        unchanged.image.is_empty(),
        "unchanged images must not consume network bandwidth"
    );
    assert!(
        browser
            .agent(
                &scope_b,
                BrowserAction::SelectTab {
                    id: popup.tab_id.clone()
                },
            )
            .await
            .is_err()
    );
    browser
        .agent(
            &scope_b,
            BrowserAction::Navigate {
                url: format!("http://{address}/verify"),
            },
        )
        .await
        .unwrap();
    let mut isolated = BrowserFrame::default();
    for _ in 0..20 {
        isolated = browser.agent(&scope_b, BrowserAction::Read).await.unwrap();
        if isolated
            .tabs
            .iter()
            .any(|tab| tab.title.ends_with("stored"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        isolated
            .tabs
            .iter()
            .any(|tab| tab.title.contains("agent_fixture=persisted")
                && tab.title.ends_with("stored")),
        "login state is shared while tab ownership stays scoped to a conversation"
    );
    let delayed = browser
        .agent(
            &scope_a,
            BrowserAction::Navigate {
                url: format!("http://{address}/slow"),
            },
        )
        .await;
    assert!(delayed.err().unwrap().contains("タイムアウト"));
    let recovered = browser
        .request(&request(&thread_a, &popup, BrowserAction::Read))
        .await
        .unwrap();
    assert_eq!(recovered.tab_id, popup.tab_id);
    assert_eq!(
        recovered.tabs.iter().map(|tab| &tab.id).collect::<Vec<_>>(),
        popup.tabs.iter().map(|tab| &tab.id).collect::<Vec<_>>()
    );
    let other = browser.agent(&scope_b, BrowserAction::Read).await.unwrap();
    assert_eq!(
        other.tabs, isolated.tabs,
        "a slow page must preserve every conversation's tabs"
    );
    {
        let mut state = browser.state.lock().await;
        Browser::credential_action(
            &mut state,
            &scope_a,
            credentials::Prepared::Cookies {
                cookies: vec![credentials::Cookie {
                    domain: address.ip().to_string(),
                    name: "imported_session".into(),
                    value: zeroize::Zeroizing::new("fixture-login".into()),
                    path: "/".into(),
                    secure: false,
                    http_only: true,
                    same_site: 1,
                    expires: None,
                }],
                skipped: 0,
            },
        )
        .await
        .unwrap();
    }
    for (scope, expected) in [(&scope_a, "Logged in"), (&scope_b, "Logged in")] {
        browser
            .agent(
                scope,
                BrowserAction::Navigate {
                    url: format!("http://{address}/authenticated"),
                },
            )
            .await
            .unwrap();
        let mut authenticated = BrowserFrame::default();
        for _ in 0..40 {
            authenticated = browser.agent(scope, BrowserAction::Read).await.unwrap();
            if authenticated.tabs.iter().any(|tab| tab.title == expected) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            authenticated.tabs.iter().any(|tab| tab.title == expected),
            "imported authentication stays in its shared conversation"
        );
        let visible = browser
            .agent(
                scope,
                BrowserAction::Evaluate {
                    expression: "document.cookie".into(),
                },
            )
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&visible.value).unwrap();
        let cookies = value.as_str().unwrap();
        assert!(cookies.contains("agent_fixture=persisted"));
        assert!(
            !cookies.contains("imported_session"),
            "HttpOnly cookies stay unavailable to page scripts"
        );
    }
    browser.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn credential_import_and_vault_access_are_explicit_client_actions() {
    let root = tempfile::tempdir().unwrap();
    // Agent credential requests must fail before starting Chrome or the MCP bridge.
    let browser = Browser {
        profile: root.path().join("profile"),
        executable: "unused-browser".into(),
        state: Default::default(),
        stop: Default::default(),
        #[cfg(unix)]
        bridge_directory: root,
        #[cfg(windows)]
        bridge_pipe: r"\\.\pipe\agent-browser-unused".into(),
    };
    for action in [
        BrowserAction::Credentials,
        BrowserAction::ImportCookies {
            profile: "Chrome/Default".into(),
        },
        BrowserAction::ImportPasswords {
            profile: "Chrome/Default".into(),
        },
        BrowserAction::FillPassword {
            origin: "https://example.test".into(),
            username: "fixture-user".into(),
        },
        BrowserAction::DeletePasswords {
            origin: "https://example.test".into(),
        },
    ] {
        assert!(
            agent_protocol::browser::validate_displayed_page("new-tab", 2, "old-tab", 2, &action)
                .is_err()
        );
        assert!(
            agent_protocol::browser::validate_displayed_page("tab", 2, "tab", 1, &action).is_err()
        );
        assert!(
            agent_protocol::browser::validate_displayed_page("tab", 2, "tab", 2, &action).is_ok()
        );
        let error = browser.agent("thread", action).await.err().unwrap();
        assert!(error.contains("ブラウザのメニュー"));
    }
    let state = browser.state.lock().await;
    assert!(state.chrome.is_none());
    assert!(state.pages.is_empty());
}

#[tokio::test]
async fn empty_cookie_import_does_not_reload_or_create_a_browser() {
    let mut state = State::default();
    let result = Browser::credential_action(
        &mut state,
        "not-opened",
        credentials::Prepared::Cookies {
            cookies: Vec::new(),
            skipped: 3,
        },
    )
    .await
    .unwrap();
    assert!(result.value.contains("取り込めるCookieがありません"));
    assert!(state.chrome.is_none());
    assert!(state.pages.is_empty());
}

#[test]
fn passwords_only_target_the_displayed_https_origin() {
    let current = Some("https://example.test");
    assert!(validate_password_origin(current, "https://example.test").is_ok());
    assert!(validate_password_origin(None, "https://example.test").is_err());
    for requested in [
        "https://sub.example.test",
        "https://example.test:8443",
        "http://example.test",
        "https://example.test.attacker.test",
    ] {
        assert!(validate_password_origin(current, requested).is_err());
    }
}
