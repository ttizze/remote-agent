#![cfg(unix)]

#[tokio::test(flavor = "current_thread")]
#[ignore = "temporary isolated Chrome startup diagnostic"]
async fn isolated_chrome_startup() {
    use agent_protocol::{
        browser::{BrowserAction, BrowserRequest},
        session::{ProviderKind, SessionRef},
    };
    let root = tempfile::Builder::new()
        .prefix("bex-ios-")
        .tempdir()
        .unwrap();
    let profile = root.path().join("host/browser");
    let browser = host_daemon::browser::Browser::start(profile.clone())
        .await
        .unwrap();
    let request = BrowserRequest {
        thread_id: SessionRef {
            provider: ProviderKind::Codex,
            id: "diagnostic-thread".into(),
        },
        tab_id: String::new(),
        image_id: String::new(),
        action: BrowserAction::Read,
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        browser.request(&request),
    )
    .await;
    println!(
        "port_file={:?}; outcome_error={:?}",
        std::fs::read_to_string(profile.join("DevToolsActivePort")),
        outcome.as_ref().map(|result| result.as_ref().err())
    );
    browser.shutdown().await;
    let frame = outcome.unwrap().unwrap();
    assert_eq!(frame.tabs.len(), 1);
    assert!(!frame.image.is_empty());
}
