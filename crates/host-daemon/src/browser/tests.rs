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

#[test]
fn owner_frame_selection_does_not_follow_another_preview_owner() {
    let mut page = Page {
        tabs: vec!["shared".into(), "owner-a".into(), "owner-b".into()],
        active: "owner-b".into(),
        active_by_owner: HashMap::new(),
        viewports: HashMap::new(),
        preview_tabs: HashSet::from(["owner-a".into(), "owner-b".into()]),
        preview_profiles: HashMap::new(),
        preview_profile_owners: HashMap::from([
            ("owner-a".into(), "client-a".into()),
            ("owner-b".into(), "client-b".into()),
        ]),
        preview_settings: HashMap::new(),
    };
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("client-a"), None).unwrap(),
        "owner-a"
    );
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("client-b"), Some("owner-b")).unwrap(),
        "owner-b"
    );
    page.active = "shared".into();
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("client-a"), None).unwrap(),
        "owner-a"
    );
    // The provider browser is the collaborative surface for this thread, so
    // its local owner can operate the human-visible shared tab alongside the
    // authenticated Preview owners.
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("local"), None).unwrap(),
        "shared"
    );
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("local"), Some("owner-a")).unwrap(),
        "owner-a"
    );
    assert_eq!(
        Browser::activate_tab_for_owner(&mut page, Some("client-b"), None).unwrap(),
        "owner-b"
    );
    assert_eq!(
        page.active_by_owner.get("client-a").map(String::as_str),
        Some("owner-a")
    );
    assert_eq!(
        page.active_by_owner.get("client-b").map(String::as_str),
        Some("owner-b")
    );
    assert_eq!(
        page.active_by_owner.get("local").map(String::as_str),
        Some("owner-a")
    );
}

#[tokio::test]
async fn recording_completion_is_replayable_after_startup_receiver_drops() {
    let (done, startup_receiver) = tokio::sync::watch::channel::<
        Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
    >(None);
    drop(startup_receiver);
    done.send_replace(Some(Ok(
        agent_protocol::preview::PreviewRecordingArtifact {
            id: "browser-recording-test".into(),
            recording_id: "browser-recording-test".into(),
            tab_id: "tab".into(),
            path: "/tmp/browser-recording-test.webm".into(),
            mime_type: "video/webm;codecs=vp9".into(),
            size_bytes: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
        },
    )));
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
    assert_eq!(
        waiting.await.unwrap().unwrap(),
        RecordingStartupState::Started
    );
}

#[tokio::test]
async fn cancelled_start_discards_an_artifact_already_offered_by_the_monitor() {
    let root = tempfile::tempdir().unwrap();
    let browser = Browser::start(root.path().join("profile")).await.unwrap();
    let key = ("thread".to_owned(), "tab".to_owned());
    let path = root.path().join("recording.webm");
    let previous = root.path().join("previous.webm");
    std::fs::write(&path, b"cancelled").unwrap();
    std::fs::write(&previous, b"previous").unwrap();
    browser.recording_artifacts.lock().await.insert(
        key.clone(),
        vec![
            agent_protocol::preview::PreviewRecordingArtifact {
                id: "previous".into(),
                recording_id: "previous".into(),
                tab_id: key.1.clone(),
                path: previous.to_string_lossy().into_owned(),
                mime_type: "video/webm;codecs=vp9".into(),
                size_bytes: 8,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            agent_protocol::preview::PreviewRecordingArtifact {
                id: "cancelled".into(),
                recording_id: "cancelled".into(),
                tab_id: key.1.clone(),
                path: path.to_string_lossy().into_owned(),
                mime_type: "video/webm;codecs=vp9".into(),
                size_bytes: 9,
                created_at: "2026-01-01T00:00:01Z".into(),
            },
        ],
    );

    browser.discard_recording_artifact_path(&key, &path).await;

    assert!(!path.exists());
    assert!(previous.exists());
    assert_eq!(
        browser.completed_recording(&key, None).await.unwrap().id,
        "previous"
    );
    let _ = std::fs::remove_file(previous);
    browser.shutdown().await;
}

#[test]
fn completed_recording_retention_is_global_across_open_preview_tabs() {
    let mut artifacts = std::collections::HashMap::new();
    for index in 0..6 {
        let tab = format!("tab-{index}");
        artifacts.insert(
            ("thread".to_owned(), tab.clone()),
            vec![agent_protocol::preview::PreviewRecordingArtifact {
                id: format!("recording-{index}"),
                recording_id: format!("recording-{index}"),
                tab_id: tab,
                path: format!("/tmp/recording-{index}.webm"),
                mime_type: "video/webm;codecs=vp9".into(),
                size_bytes: 1,
                created_at: format!("2026-01-01T00:00:0{index}Z"),
            }],
        );
    }

    let removed = prune_completed_recordings(&mut artifacts, MAX_RETAINED_RECORDINGS);

    assert_eq!(artifacts.len(), MAX_RETAINED_RECORDINGS);
    assert_eq!(removed.len(), 2);
    assert_eq!(
        removed[0].1,
        std::path::PathBuf::from("/tmp/recording-0.webm")
    );
    assert_eq!(
        removed[1].1,
        std::path::PathBuf::from("/tmp/recording-1.webm")
    );
    assert_eq!(removed[0].0, ("thread".into(), "tab-0".into()));
    assert_eq!(removed[1].0, ("thread".into(), "tab-1".into()));
}

#[test]
fn detached_preview_target_cleanup_removes_host_page_metadata() {
    let mut state = State::default();
    state.pages.insert(
        "thread".into(),
        Page {
            tabs: vec!["other".into(), "tab".into()],
            active: "tab".into(),
            active_by_owner: HashMap::new(),
            viewports: [("tab".into(), (800, 600))].into_iter().collect(),
            preview_tabs: ["tab".into()].into_iter().collect(),
            preview_profiles: HashMap::new(),
            preview_profile_owners: HashMap::new(),
            preview_settings: [("tab".into(), (PreviewAppearance::System, PreviewZoom::X100))]
                .into_iter()
                .collect(),
        },
    );

    assert!(forget_detached_preview_target(&mut state, "thread", "tab"));
    let page = state.pages.get("thread").unwrap();
    assert_eq!(page.tabs, ["other"]);
    assert_eq!(page.active, "other");
    assert!(page.viewports.is_empty());
    assert!(page.preview_tabs.is_empty());
    assert!(page.preview_settings.is_empty());
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
        .agent_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
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
            .request_for_owner(
                COLLABORATIVE_BROWSER_OWNER,
                &request(&thread_a, &initial, BrowserAction::Read),
            )
            .await
            .unwrap();
    }
    assert!(initial.tabs.iter().any(|tab| tab.title == "Fixture"));
    browser
        .request_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &request(
                &thread_a,
                &initial,
                BrowserAction::Click { x: 100.0, y: 40.0 },
            ),
        )
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
        browser.agent_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &scope_a,
            BrowserAction::Type {
                text: "AI input".into()
            }
        ),
        browser.request_for_owner(COLLABORATIVE_BROWSER_OWNER, &phone_input),
    );
    agent.unwrap();
    phone.unwrap();
    let typed = browser
        .request_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &request(&thread_a, &initial, BrowserAction::Read),
        )
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
    let observed = browser
        .agent_for_owner(COLLABORATIVE_BROWSER_OWNER, &scope_a, BrowserAction::Read)
        .await
        .unwrap();
    assert_eq!(
        observed.tabs, typed.tabs,
        "agent reads remain available after phone input"
    );
    browser
        .request_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &request(
                &thread_a,
                &initial,
                BrowserAction::Click { x: 100.0, y: 110.0 },
            ),
        )
        .await
        .unwrap();
    let mut popup = typed;
    for _ in 0..20 {
        popup = browser
            .request_for_owner(
                COLLABORATIVE_BROWSER_OWNER,
                &request(&thread_a, &popup, BrowserAction::Read),
            )
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
            .request_for_owner(
                COLLABORATIVE_BROWSER_OWNER,
                &request(
                    &thread_a,
                    &initial,
                    BrowserAction::Click { x: 20.0, y: 20.0 }
                )
            )
            .await
            .is_err(),
        "stale tab input must fail"
    );
    let other = browser
        .agent_for_owner(COLLABORATIVE_BROWSER_OWNER, &scope_b, BrowserAction::Read)
        .await
        .unwrap();
    assert_eq!(other.tabs.len(), 1);
    let unchanged = browser
        .request_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &request(&thread_b, &other, BrowserAction::Read),
        )
        .await
        .unwrap();
    assert!(
        unchanged.image.is_empty(),
        "unchanged images must not consume network bandwidth"
    );
    assert!(
        browser
            .agent_for_owner(
                COLLABORATIVE_BROWSER_OWNER,
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
        .agent_for_owner(
            COLLABORATIVE_BROWSER_OWNER,
            &scope_a,
            BrowserAction::Navigate {
                url: format!("http://{address}/verify"),
            },
        )
        .await
        .unwrap();
    let mut restored = BrowserFrame::default();
    for _ in 0..20 {
        restored = browser
            .agent_for_owner(COLLABORATIVE_BROWSER_OWNER, &scope_a, BrowserAction::Read)
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

#[test]
fn preview_profile_storage_is_partitioned_by_authenticated_owner() {
    let first = Browser::preview_profile_suffix("device-a", "work");
    let second = Browser::preview_profile_suffix("device-b", "work");
    let same_owner = Browser::preview_profile_suffix("device-a", "work");
    assert_ne!(first, second);
    assert_eq!(first, same_owner);
}
