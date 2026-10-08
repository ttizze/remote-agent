use super::*;
use agent_domain::ThreadId;

#[test]
fn input_requires_the_displayed_tab_but_reads_and_selection_can_refresh_it() {
    for action in [
        BrowserAction::Click { x: 10.0, y: 10.0 },
        BrowserAction::Type {
            text: "input".into(),
        },
        BrowserAction::Navigate {
            url: "https://example.test".into(),
        },
        BrowserAction::Back,
        BrowserAction::Dialog {
            accept: true,
            text: String::new(),
        },
    ] {
        assert!(validate_tab("tab", "tab", &action).is_ok());
        assert!(validate_tab("tab", "old-tab", &action).is_err());
    }
    for action in [
        BrowserAction::Read,
        BrowserAction::SelectTab { id: "tab".into() },
    ] {
        assert!(validate_tab("tab", "old-tab", &action).is_ok());
    }
}

#[tokio::test]
async fn recording_completion_is_replayable_after_startup_receiver_drops() {
    let (done, startup_receiver) = tokio::sync::watch::channel::<
        Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
    >(None);
    drop(startup_receiver);
    done.send_replace(Some(Ok(agent_protocol::preview::PreviewRecordingArtifact {
        id: "browser-recording-test".into(),
        tab_id: "tab".into(),
        path: "/tmp/browser-recording-test.webm".into(),
        mime_type: "video/webm;codecs=vp9".into(),
        size_bytes: 1,
        created_at: "2026-01-01T00:00:00Z".into(),
    })));
    let mut late_receiver = done.subscribe();

    let result = wait_for_recording_completion(&mut late_receiver)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.id, "browser-recording-test");
}

#[tokio::test]
async fn stop_waits_for_startup_before_cancelling_capture() {
    let (startup, mut receiver) = tokio::sync::watch::channel(RecordingStartupState::Pending);
    let waiting = tokio::spawn(async move { wait_for_recording_startup(&mut receiver).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    startup.send_replace(RecordingStartupState::Started);
    assert_eq!(waiting.await.unwrap().unwrap(), RecordingStartupState::Started);
}

#[test]
fn duplicate_stop_requests_share_one_completion_owner() {
    let mut stopping = false;
    assert!(begin_recording_stop(&mut stopping));
    assert!(!begin_recording_stop(&mut stopping));
    assert!(stopping);
}

fn request(thread: &ThreadId, frame: &BrowserFrame, action: BrowserAction) -> BrowserRequest {
    BrowserRequest {
        thread_id: thread.clone(),
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
    let thread_a = ThreadId::new("thread-a").unwrap();
    let thread_b = ThreadId::new("thread-b").unwrap();
    let scope_a = thread_a.to_string();
    let scope_b = thread_b.to_string();
    let mut initial = browser
        .agent(
            &scope_a,
            BrowserAction::Navigate {
                url: format!("http://{address}/"),
            },
        )
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
    browser
        .request(&request(
            &thread_a,
            &initial,
            BrowserAction::Click { x: 100.0, y: 40.0 },
        ))
        .await
        .unwrap();
    let phone_input = request(
        &thread_a,
        &initial,
        BrowserAction::Type {
            text: "日本語 input".into(),
        },
    );
    let (agent, phone) = tokio::join!(
        browser.agent(
            &scope_a,
            BrowserAction::Type {
                text: "AI input".into()
            }
        ),
        browser.request(&phone_input),
    );
    agent.unwrap();
    phone.unwrap();
    let typed = browser
        .request(&request(&thread_a, &initial, BrowserAction::Read))
        .await
        .unwrap();
    assert!(
        typed.tabs.iter().any(|tab| matches!(
            tab.title.as_str(),
            "日本語 inputAI input" | "AI input日本語 input"
        )),
        "both overlapping phone and agent inputs must execute: {:?}",
        typed.tabs
    );
    let observed = browser.agent(&scope_a, BrowserAction::Read).await.unwrap();
    assert_eq!(
        observed.tabs, typed.tabs,
        "agent reads remain available after phone input"
    );
    browser
        .request(&request(
            &thread_a,
            &initial,
            BrowserAction::Click { x: 100.0, y: 110.0 },
        ))
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
    let other = browser.agent(&scope_b, BrowserAction::Read).await.unwrap();
    assert_eq!(other.tabs.len(), 1);
    let unchanged = browser
        .request(&request(&thread_b, &other, BrowserAction::Read))
        .await
        .unwrap();
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
    browser.shutdown().await;
    drop(browser);
    let browser = Browser::start(profile).await.unwrap();
    browser
        .agent(
            &scope_a,
            BrowserAction::Navigate {
                url: format!("http://{address}/verify"),
            },
        )
        .await
        .unwrap();
    let mut restored = BrowserFrame::default();
    for _ in 0..20 {
        restored = browser.agent(&scope_a, BrowserAction::Read).await.unwrap();
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
