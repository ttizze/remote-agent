use super::*;
use agent_protocol::models::RepositoryLocator;
use tokio::sync::Notify;

fn identity(root: &str, version: u32) -> RepositoryIdentity {
    RepositoryIdentity {
        canonical_key: format!("example.test/v{version}{root}"),
        locator: RepositoryLocator {
            source: "git-remote".into(),
            remote_name: "origin".into(),
            remote_url: format!("https://example.test/v{version}{root}.git"),
        },
        web_url: None,
        root_path: Some(root.into()),
        display_name: None,
        provider: None,
        owner: None,
        name: None,
    }
}

fn lookup(
    resolve: impl Fn(String) -> BoxFuture<'static, Result<Option<RepositoryIdentity>, String>>
    + Send
    + Sync
    + 'static,
) -> Lookup {
    Arc::new(resolve)
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the identity was not resolved in time");
}

fn key(identities: &ProjectIdentities, root: &str) -> Option<String> {
    identities.peek(root).map(|identity| identity.canonical_key)
}

// ProjectEnrichmentService.test.ts "getAvailable returns immediately while
// repository identity is still unresolved"
#[tokio::test]
async fn reads_immediately_while_the_identity_is_still_unresolved() {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let identities = ProjectIdentities::new(
        lookup({
            let (started, release) = (started.clone(), release.clone());
            move |root| {
                let (started, release) = (started.clone(), release.clone());
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(Some(identity(&root, 1)))
                })
            }
        }),
        Options::new(Clock::default()),
    );

    // Reading does not wait on the hung lookup.
    assert_eq!(identities.available("/pending-identity"), None);
    started.notified().await;
    assert_eq!(identities.peek("/pending-identity"), None);

    // The lookup still completes after the read returned.
    release.notify_one();
    wait_for(|| identities.peek("/pending-identity").is_some()).await;
    assert_eq!(
        key(&identities, "/pending-identity").as_deref(),
        Some("example.test/v1/pending-identity")
    );
}

// "publishes repository completion while favicon enrichment is still pending"
#[tokio::test]
async fn announces_a_completed_identity() {
    let identities = ProjectIdentities::new(
        lookup(|root| Box::pin(async move { Ok(Some(identity(&root, 1))) })),
        Options::new(Clock::default()),
    );
    let mut changes = identities.subscribe();
    identities.request("/completed");
    assert_eq!(changes.recv().await.unwrap(), "/completed");
    assert_eq!(
        key(&identities, "/completed").as_deref(),
        Some("example.test/v1/completed")
    );
}

// "reports successful cached null as resolved while cold and failed lookups stay
// unresolved": a resolved null and a failure leave nothing to show, so nothing is
// announced; a fresh null is not looked up again.
#[tokio::test]
async fn announces_nothing_for_a_missing_identity_or_a_failed_lookup() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let identities = ProjectIdentities::new(
        lookup({
            let calls = calls.clone();
            move |root| {
                calls.lock().unwrap().push(root.clone());
                Box::pin(async move {
                    match root.as_str() {
                        "/no-remote" => Ok(None),
                        "/fails" => Err("repository resolver failed".into()),
                        _ => panic!("repository resolver panicked"),
                    }
                })
            }
        }),
        Options::new(Clock::default()),
    );
    let mut changes = identities.subscribe();
    assert_eq!(identities.peek("/never-requested"), None);

    for root in ["/no-remote", "/fails", "/panics"] {
        identities.request(root);
    }
    wait_for(|| calls.lock().unwrap().len() == 3).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(matches!(
        changes.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    // Warm null and failures are not looked up again yet.
    for root in ["/no-remote", "/fails", "/panics"] {
        assert_eq!(identities.available(root), None);
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(calls.lock().unwrap().len(), 3);
}

// "deduplicates requests, bounds pending work, and reloads invalidated roots"
#[tokio::test]
async fn deduplicates_requests_bounds_pending_work_and_reloads_invalidated_roots() {
    let (first_started, release_first) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let version = Arc::new(std::sync::atomic::AtomicU32::new(1));
    let identities = ProjectIdentities::new(
        lookup({
            let (first_started, release_first, calls, version) = (
                first_started.clone(),
                release_first.clone(),
                calls.clone(),
                version.clone(),
            );
            move |root| {
                calls.lock().unwrap().push(root.clone());
                let (first_started, release_first, version) = (
                    first_started.clone(),
                    release_first.clone(),
                    version.clone(),
                );
                Box::pin(async move {
                    let current = version.load(std::sync::atomic::Ordering::SeqCst);
                    if root == "/first" && current == 1 {
                        first_started.notify_one();
                        release_first.notified().await;
                    }
                    Ok(Some(identity(&root, current)))
                })
            }
        }),
        Options {
            capacity: 8,
            max_pending: 2,
            concurrency: 1,
            ..Options::new(Clock::default())
        },
    );
    for _ in 0..20 {
        identities.request("/first");
    }
    first_started.notified().await;
    identities.request("/second");
    identities.request("/dropped");
    assert_eq!(*calls.lock().unwrap(), ["/first"]);

    release_first.notify_one();
    wait_for(|| identities.peek("/second").is_some()).await;
    assert_eq!(*calls.lock().unwrap(), ["/first", "/second"]);

    identities.request("/dropped");
    wait_for(|| identities.peek("/dropped").is_some()).await;
    assert_eq!(*calls.lock().unwrap(), ["/first", "/second", "/dropped"]);

    version.store(2, std::sync::atomic::Ordering::SeqCst);
    identities.invalidate(["/first"]);
    assert_eq!(identities.available("/first"), None);
    wait_for(|| key(&identities, "/first").as_deref() == Some("example.test/v2/first")).await;
    assert_eq!(
        *calls.lock().unwrap(),
        ["/first", "/second", "/dropped", "/first"]
    );
}

#[tokio::test]
async fn repeats_a_lookup_invalidated_while_it_runs() {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let version = Arc::new(std::sync::atomic::AtomicU32::new(1));
    let identities = ProjectIdentities::new(
        lookup({
            let (started, release, version) = (started.clone(), release.clone(), version.clone());
            move |root| {
                let (started, release, version) =
                    (started.clone(), release.clone(), version.clone());
                Box::pin(async move {
                    let current = version.load(std::sync::atomic::Ordering::SeqCst);
                    if current == 1 {
                        started.notify_one();
                        release.notified().await;
                    }
                    Ok(Some(identity(&root, current)))
                })
            }
        }),
        Options::new(Clock::default()),
    );
    let mut changes = identities.subscribe();
    identities.request("/project");
    started.notified().await;
    version.store(2, std::sync::atomic::Ordering::SeqCst);
    identities.invalidate(["/project"]);
    release.notify_one();
    assert_eq!(changes.recv().await.unwrap(), "/project");
    assert_eq!(
        key(&identities, "/project").as_deref(),
        Some("example.test/v2/project")
    );
}

// An expired identity stays readable while it is looked up again, and only a
// changed one is announced.
#[tokio::test]
async fn looks_up_an_expired_identity_again_and_announces_only_a_change() {
    let clock = Clock::default();
    let version = Arc::new(std::sync::atomic::AtomicU32::new(1));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let identities = ProjectIdentities::new(
        lookup({
            let (version, calls) = (version.clone(), calls.clone());
            move |root| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let current = version.load(std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move { Ok(Some(identity(&root, current))) })
            }
        }),
        Options::new(clock.clone()),
    );
    let mut changes = identities.subscribe();
    identities.request("/project");
    assert_eq!(changes.recv().await.unwrap(), "/project");

    // Within a minute the identity is not looked up again.
    clock.advance(Duration::from_secs(59));
    assert_eq!(
        identities
            .available("/project")
            .map(|i| i.canonical_key)
            .as_deref(),
        Some("example.test/v1/project")
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    // An unchanged identity is looked up again and not announced.
    clock.advance(Duration::from_secs(2));
    identities.available("/project");
    wait_for(|| calls.load(std::sync::atomic::Ordering::SeqCst) == 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(changes.try_recv().is_err());

    // A changed one is announced; the expired one stays readable meanwhile.
    version.store(2, std::sync::atomic::Ordering::SeqCst);
    clock.advance(Duration::from_secs(61));
    assert_eq!(
        identities
            .available("/project")
            .map(|i| i.canonical_key)
            .as_deref(),
        Some("example.test/v1/project")
    );
    assert_eq!(changes.recv().await.unwrap(), "/project");
    assert_eq!(
        key(&identities, "/project").as_deref(),
        Some("example.test/v2/project")
    );
}

// The identity cache in front of the resolver's: an origin changed after a
// lookup shows once both have expired, and is announced.
#[tokio::test]
async fn shows_a_changed_origin_once_both_caches_expire() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let git = |args: &[&str]| crate::git::text(&root, args).unwrap();
    git(&["init", "--quiet"]);
    git(&["remote", "add", "origin", "git@github.com:Acme/widget.git"]);
    let root = root.to_str().unwrap().to_owned();
    let clock = Clock::default();
    let identities = ProjectIdentities::system(clock.clone());
    let mut changes = identities.subscribe();

    assert_eq!(identities.available(&root), None);
    assert_eq!(changes.recv().await.unwrap(), root);
    assert_eq!(
        key(&identities, &root).as_deref(),
        Some("github.com/acme/widget")
    );

    git(&[
        "remote",
        "set-url",
        "origin",
        "git@github.com:Acme/gadget.git",
    ]);
    // The resolver keeps a found identity for fifteen minutes.
    clock.advance(Duration::from_secs(2 * 60));
    identities.available(&root);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(changes.try_recv().is_err());
    assert_eq!(
        key(&identities, &root).as_deref(),
        Some("github.com/acme/widget")
    );

    clock.advance(Duration::from_secs(14 * 60));
    identities.available(&root);
    assert_eq!(changes.recv().await.unwrap(), root);
    assert_eq!(
        key(&identities, &root).as_deref(),
        Some("github.com/acme/gadget")
    );
}
