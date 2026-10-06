//! The runtime extracts the candidates.
use super::*;
use agent_runtime::title_link_candidates;
use std::sync::{Arc, Mutex};

fn encoded(title: &str, body: &str) -> String {
    serde_json::to_string(&Encoded {
        title: title.into(),
        body: body.into(),
    })
    .unwrap()
}

#[tokio::test]
async fn uses_provider_selected_links_deduplicates_anchors_and_bounds_lookups_and_summaries() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = calls.clone();
    let resolve = move |url: &url::Url, cwd: &str| -> Option<SubjectRead> {
        if url.host_str() != Some("forge.test") {
            return None;
        }
        assert_eq!(cwd, "/tmp/project");
        recorded.lock().unwrap().push(url.to_string());
        Some(Box::pin(async {
            Some(Subject {
                title: "t".repeat(400),
                body: Some("b".repeat(2_000)),
            })
        }))
    };
    let message = "https://docs.test/guide [https://forge.test/change/1] https://forge.test/change/1#discussion https://forge.test/change/1?view=full `https://forge.test/change/2` https://forge.test/change/2. https://forge.test/change/3";
    let result =
        title_link_context("/tmp/project", &title_link_candidates(message), &resolve).await;
    let calls = calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        ["https://forge.test/change/1", "https://forge.test/change/2"]
    );
    assert_eq!(
        result.unwrap(),
        calls
            .iter()
            .map(|url| format!("{url}\n{}", encoded(&"t".repeat(300), &"b".repeat(1_200))))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

#[tokio::test(start_paused = true)]
async fn returns_unavailable_when_a_lookup_times_out_while_retaining_successful_subjects() {
    let resolve = |url: &url::Url, _: &str| -> Option<SubjectRead> {
        Some(if url.path().ends_with('1') {
            Box::pin(std::future::pending())
        } else {
            Box::pin(async {
                Some(Subject {
                    title: "Fix QR pairing expiry".into(),
                    body: Some("Keep remote connections working.".into()),
                })
            })
        })
    };
    let candidates =
        title_link_candidates("https://forge.test/change/1 https://forge.test/change/2");
    assert_eq!(
        title_link_context("/tmp/project", &candidates, &resolve)
            .await
            .unwrap(),
        format!(
            "https://forge.test/change/1: unavailable\n\nhttps://forge.test/change/2\n{}",
            encoded("Fix QR pairing expiry", "Keep remote connections working.")
        )
    );
}

#[tokio::test]
async fn keeps_lookup_failure_out_of_generation_and_skips_unlinked_messages() {
    let resolve = |_: &url::Url, _: &str| -> Option<SubjectRead> { Some(Box::pin(async { None })) };
    assert_eq!(
        title_link_context("/tmp", &title_link_candidates("Fix pairing"), &resolve).await,
        None
    );
    assert!(
        title_link_context(
            "/tmp",
            &title_link_candidates("https://forge.test/change/1"),
            &resolve
        )
        .await
        .unwrap()
        .contains("unavailable")
    );
}

#[test]
fn resolves_only_issue_and_change_links_on_the_public_hosts() {
    let supported = |link: &str| resolve_link(&url::Url::parse(link).unwrap(), "/tmp").is_some();
    assert!(supported("https://github.com/owner/repo/pull/12"));
    assert!(supported("https://github.com/owner/repo/issues/3/files"));
    assert!(supported("https://gitlab.com/group/sub/-/merge_requests/4"));
    assert!(!supported("https://github.com/owner/repo/pull/0"));
    assert!(!supported("https://github.example.com/owner/repo/pull/1"));
    assert!(!supported("https://user@github.com/owner/repo/pull/1"));
    assert!(!supported("http://github.com/owner/repo/pull/1"));
    assert!(!supported("https://github.com/owner/repo/tree/main"));
    assert_eq!(encode_component("group/sub x"), "group%2Fsub%20x");
}
