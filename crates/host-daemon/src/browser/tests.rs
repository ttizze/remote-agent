use super::*;

#[test]
fn control_is_exclusive_and_stale_input_cannot_cross_handoffs() {
    let mut page = Page {
        active: "tab".into(),
        ..Default::default()
    };
    let token = page.token.clone();
    let click = BrowserAction::Click { x: 10.0, y: 10.0 };
    assert!(authorize(&page, "phone", &token, "tab", &click).is_err());
    assert!(authorize(&page, "phone", &token, "tab", &BrowserAction::TakeControl).is_ok());
    page.owner = Some("phone".into());
    page.token = "new".into();
    assert!(authorize(&page, "phone", &token, "tab", &click).is_err());
    assert!(authorize(&page, "other", "new", "tab", &BrowserAction::TakeControl).is_err());
    assert!(authorize(&page, "other", "new", "tab", &click).is_err());
    assert!(authorize(&page, "phone", "new", "old-tab", &click).is_err());
    assert!(authorize(&page, "phone", "new", "tab", &click).is_ok());
    page.owner = None;
    page.token = "released".into();
    assert!(authorize(&page, "phone", "new", "tab", &click).is_err());
}

fn request(thread: &str, frame: &BrowserFrame, action: BrowserAction) -> BrowserRequest {
    BrowserRequest {
        thread_id: agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: thread.into(),
        },
        control_token: frame.control_token.clone(),
        tab_id: frame.tab_id.clone(),
        image_id: frame.image_id.clone(),
        action,
    }
}

/// Exercises only a temporary BEX profile and a local, deterministic web fixture.
#[tokio::test]
#[ignore = "requires Chrome/Chromium and bex-provider-supervisor"]
async fn shared_browser_live() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut bytes = [0; 8192];
                let length = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..length]);
                let body = if request.starts_with("GET /popup ") {
                    "<title>Popup</title>Popup"
                } else if request.starts_with("GET /verify ") {
                    "<script>document.title=document.cookie+' '+localStorage.getItem('bex-fixture')</script>"
                } else {
                    r#"<body style="margin:0;height:3000px"><input style="position:absolute;left:20px;top:20px;width:300px;height:40px" oninput="document.title=this.value"><button style="position:absolute;left:20px;top:90px;width:200px;height:40px" onclick="window.open('/popup')">Popup</button><script>document.cookie='bex_fixture=persisted; Max-Age=3600; SameSite=Lax';localStorage.setItem('bex-fixture','stored');document.title='Fixture'</script>"#
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
    let initial = browser
        .agent(
            "thread-a",
            BrowserAction::Navigate {
                url: format!("http://{address}/"),
            },
            false,
        )
        .await
        .unwrap();
    assert!(initial.image.starts_with(&[0xff, 0xd8]));
    let controlled = browser
        .request(
            "phone",
            &request("thread-a", &initial, BrowserAction::TakeControl),
        )
        .await
        .unwrap();
    assert_eq!(controlled.control, BrowserControl::Yours);
    assert!(
        browser
            .request(
                "other",
                &request("thread-a", &controlled, BrowserAction::TakeControl)
            )
            .await
            .is_err()
    );
    let waiting = tokio::spawn({
        let browser = browser.clone();
        async move { browser.agent("thread-a", BrowserAction::Read, false).await }
    });
    let stale_input = tokio::spawn({
        let browser = browser.clone();
        async move {
            browser
                .agent(
                    "thread-a",
                    BrowserAction::Type {
                        text: "stale input".into(),
                    },
                    false,
                )
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!stale_input.is_finished());
    assert!(
        !waiting.is_finished(),
        "agent cannot read or operate while human owns the browser"
    );
    browser
        .request(
            "phone",
            &request(
                "thread-a",
                &controlled,
                BrowserAction::Click { x: 100.0, y: 40.0 },
            ),
        )
        .await
        .unwrap();
    browser
        .request(
            "phone",
            &request(
                "thread-a",
                &controlled,
                BrowserAction::Type {
                    text: "日本語 input".into(),
                },
            ),
        )
        .await
        .unwrap();
    let typed = browser
        .request(
            "phone",
            &request("thread-a", &controlled, BrowserAction::Read),
        )
        .await
        .unwrap();
    assert!(typed.tabs.iter().any(|tab| tab.title == "日本語 input"));
    browser
        .request(
            "phone",
            &request(
                "thread-a",
                &controlled,
                BrowserAction::Click { x: 100.0, y: 110.0 },
            ),
        )
        .await
        .unwrap();
    let mut popup = typed;
    for _ in 0..20 {
        popup = browser
            .request("phone", &request("thread-a", &popup, BrowserAction::Read))
            .await
            .unwrap();
        if popup.tabs.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(popup.tabs.len(), 2);
    assert_ne!(popup.tab_id, controlled.tab_id);
    assert!(
        browser
            .request(
                "phone",
                &request(
                    "thread-a",
                    &controlled,
                    BrowserAction::Click { x: 20.0, y: 20.0 }
                )
            )
            .await
            .is_err(),
        "stale tab input must fail"
    );
    let other = browser
        .agent("thread-b", BrowserAction::Read, false)
        .await
        .unwrap();
    assert_eq!(other.tabs.len(), 1);
    let unchanged = browser
        .request("phone", &request("thread-b", &other, BrowserAction::Read))
        .await
        .unwrap();
    assert!(
        unchanged.image.is_empty(),
        "unchanged images must not consume network bandwidth"
    );
    assert!(
        browser
            .agent(
                "thread-b",
                BrowserAction::SelectTab {
                    id: popup.tab_id.clone()
                },
                false
            )
            .await
            .is_err()
    );
    browser
        .request(
            "phone",
            &request("thread-a", &popup, BrowserAction::ReleaseControl),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        browser
            .request(
                "phone",
                &request(
                    "thread-a",
                    &controlled,
                    BrowserAction::Type {
                        text: "late".into()
                    }
                )
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), stale_input)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "a suspended input must require a fresh screenshot after handoff"
    );
    let waiting = tokio::spawn({
        let browser = browser.clone();
        async move { browser.agent("thread-a", BrowserAction::Read, true).await }
    });
    let mut frame = BrowserFrame::default();
    for _ in 0..20 {
        frame = browser
            .request("phone", &request("thread-a", &frame, BrowserAction::Read))
            .await
            .unwrap();
        if frame.control == BrowserControl::AwaitingHuman {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(frame.control, BrowserControl::AwaitingHuman);
    let frame = browser
        .request(
            "phone",
            &request("thread-a", &frame, BrowserAction::TakeControl),
        )
        .await
        .unwrap();
    browser
        .request(
            "phone",
            &request("thread-a", &frame, BrowserAction::ReleaseControl),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    let frame = browser
        .request("phone", &request("thread-a", &frame, BrowserAction::Read))
        .await
        .unwrap();
    browser
        .request(
            "phone",
            &request("thread-a", &frame, BrowserAction::TakeControl),
        )
        .await
        .unwrap();
    browser.revoke_device("phone").await;
    let frame = browser
        .request(
            "replacement",
            &request("thread-a", &frame, BrowserAction::Read),
        )
        .await
        .unwrap();
    assert_eq!(frame.control, BrowserControl::AwaitingHuman);
    let frame = browser
        .request(
            "replacement",
            &request("thread-a", &frame, BrowserAction::TakeControl),
        )
        .await
        .unwrap();
    browser
        .request(
            "replacement",
            &request("thread-a", &frame, BrowserAction::ReleaseControl),
        )
        .await
        .unwrap();
    browser.shutdown().await;
    drop(browser);
    let browser = Browser::start(profile).await.unwrap();
    browser
        .agent(
            "thread-a",
            BrowserAction::Navigate {
                url: format!("http://{address}/verify"),
            },
            false,
        )
        .await
        .unwrap();
    let mut restored = BrowserFrame::default();
    for _ in 0..20 {
        restored = browser
            .agent("thread-a", BrowserAction::Read, false)
            .await
            .unwrap();
        if restored
            .tabs
            .iter()
            .any(|tab| tab.title.contains("persisted stored"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        restored
            .tabs
            .iter()
            .any(|tab| tab.title.contains("persisted stored")),
        "cookies and local storage survive browser restart"
    );
    browser.shutdown().await;
    server.abort();
}
