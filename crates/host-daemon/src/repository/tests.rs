use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn git(cwd: &Path, args: &[&str]) {
    crate::git::text(cwd, args).unwrap();
}

fn repository() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    git(&root, &["init", "--quiet"]);
    (directory, root)
}

fn options(clock: &Clock) -> ResolverOptions {
    ResolverOptions::new(clock.clone())
}

async fn resolve(cwd: &Path) -> Option<RepositoryIdentity> {
    Resolver::new(options(&Clock::default()))
        .resolve(cwd.to_str().unwrap(), false)
        .await
        .unwrap()
}

fn path(cwd: &Path) -> &str {
    cwd.to_str().unwrap()
}

/// A Git whose top level and remote URL the test changes, recording each call.
struct FakeGit {
    root: Mutex<String>,
    remote: Mutex<String>,
    root_attempts: AtomicUsize,
    failed_root_attempts: usize,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}
impl FakeGit {
    fn new(root: &str, remote: &str) -> Arc<Self> {
        Self::failing_first(root, remote, 0)
    }
    fn failing_first(root: &str, remote: &str, failed_root_attempts: usize) -> Arc<Self> {
        Arc::new(Self {
            root: Mutex::new(root.into()),
            remote: Mutex::new(remote.into()),
            root_attempts: AtomicUsize::new(0),
            failed_root_attempts,
            calls: Mutex::default(),
        })
    }
    fn runner(self: &Arc<Self>) -> Git {
        let git = self.clone();
        Arc::new(move |cwd, args| {
            git.calls.lock().unwrap().push((
                cwd.to_string_lossy().into_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if args.contains(&"rev-parse") {
                if git.root_attempts.fetch_add(1, Ordering::SeqCst) < git.failed_root_attempts {
                    return None;
                }
                return Some(format!("{}\n", git.root.lock().unwrap()));
            }
            Some(format!("origin\t{} (fetch)\n", git.remote.lock().unwrap()))
        })
    }
    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

fn call(cwd: &str, args: &[&str]) -> (String, Vec<String>) {
    (
        cwd.into(),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
    )
}

#[tokio::test]
async fn refreshes_the_git_root_only_when_requested() {
    let git = FakeGit::new("/repo", "git@github.com:Acme/widget.git");
    let clock = Clock::default();
    let refinements = Arc::new(AtomicUsize::new(0));
    let refinement_fails = Arc::new(AtomicBool::new(false));
    let mut options = options(&clock);
    options.git = git.runner();
    options.refine = Some({
        let (refinements, refinement_fails) = (refinements.clone(), refinement_fails.clone());
        Arc::new(move |identity: RepositoryIdentity| {
            refinements.fetch_add(1, Ordering::SeqCst);
            let fails = refinement_fails.load(Ordering::SeqCst);
            Box::pin(async move {
                if fails {
                    return Err("account unavailable".into());
                }
                Ok(if identity.canonical_key.starts_with("ssh.forge.test/") {
                    RepositoryIdentity {
                        provider: Some("forgejo".into()),
                        web_url: Some("http://forge.test:3000/git/team/repo".into()),
                        ..identity
                    }
                } else {
                    identity
                })
            })
        })
    });
    let resolver = Resolver::new(options);

    let first = resolver.resolve("/repo/packages/web", false).await.unwrap();
    *git.root.lock().unwrap() = "/repo/packages/web".into();
    // Longer than a minute, the cadence of re-reading projects.
    clock.advance(Duration::from_secs(10 * 60));
    let second = resolver.resolve("/repo/packages/web", false).await.unwrap();

    assert_eq!(
        first.as_ref().unwrap().canonical_key,
        "github.com/acme/widget"
    );
    assert_eq!(second, first);
    assert_eq!(refinements.load(Ordering::SeqCst), 1);
    assert_eq!(
        git.calls(),
        [
            call("/repo/packages/web", &["rev-parse", "--show-toplevel"]),
            call("/repo", &["remote", "-v"]),
        ]
    );

    let refreshed = resolver.resolve("/repo/packages/web", true).await.unwrap();
    assert_eq!(
        refreshed.as_ref().unwrap().root_path.as_deref(),
        Some("/repo/packages/web")
    );
    assert_eq!(
        resolver.resolve("/repo/packages/web", false).await.unwrap(),
        refreshed
    );
    assert_eq!(
        git.calls()[2..],
        [
            call("/repo/packages/web", &["rev-parse", "--show-toplevel"]),
            call("/repo/packages/web", &["remote", "-v"]),
        ]
    );
    *git.remote.lock().unwrap() = "git@ssh.forge.test:team/repo.git".into();
    let forgejo = resolver
        .resolve("/repo/packages/web", true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        forgejo.web_url.as_deref(),
        Some("http://forge.test:3000/git/team/repo")
    );
    assert_eq!(forgejo.provider.as_deref(), Some("forgejo"));
    assert_eq!(forgejo.canonical_key, "ssh.forge.test/team/repo");
    assert_eq!(
        forgejo.locator.remote_url,
        "git@ssh.forge.test:team/repo.git"
    );
    assert_eq!(
        resolver.resolve("/repo/packages/web", false).await.unwrap(),
        Some(forgejo)
    );
    assert_eq!(refinements.load(Ordering::SeqCst), 3);
    refinement_fails.store(true, Ordering::SeqCst);
    let unavailable = resolver
        .resolve("/repo/packages/web", true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unavailable.web_url, None);
    assert_eq!(unavailable.canonical_key, "ssh.forge.test/team/repo");
}

#[tokio::test]
async fn retries_git_root_discovery_after_the_negative_ttl() {
    let git = FakeGit::failing_first("/repo", "git@github.com:Acme/widget.git", 1);
    let clock = Clock::default();
    let mut options = options(&clock);
    options.git = git.runner();
    let resolver = Resolver::new(options);

    assert_eq!(
        resolver.resolve("/repo/packages/web", false).await.unwrap(),
        None
    );
    assert_eq!(
        resolver.resolve("/repo/packages/web", false).await.unwrap(),
        None
    );

    clock.advance(Duration::from_secs(60));
    let recovered = resolver.resolve("/repo/packages/web", false).await.unwrap();
    assert_eq!(recovered.unwrap().root_path.as_deref(), Some("/repo"));
    assert_eq!(
        git.calls(),
        [
            call("/repo/packages/web", &["rev-parse", "--show-toplevel"]),
            call("/repo/packages/web", &["rev-parse", "--show-toplevel"]),
            call("/repo", &["remote", "-v"]),
        ]
    );
}

#[tokio::test]
async fn normalizes_equivalent_github_remotes_into_a_stable_repository_identity() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let identity = resolve(&root).await.unwrap();
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(
        dunce::canonicalize(identity.root_path.as_deref().unwrap()).unwrap(),
        root
    );
    assert_eq!(identity.display_name.as_deref(), Some("acme/widget"));
    assert_eq!(identity.provider.as_deref(), Some("github"));
    assert_eq!(identity.owner.as_deref(), Some("acme"));
    assert_eq!(identity.name.as_deref(), Some("widget"));
    assert_eq!(identity.locator.source, "git-remote");
    assert_eq!(identity.locator.remote_name, "origin");
    assert_eq!(
        identity.locator.remote_url,
        "git@github.com:Acme/widget.git"
    );
}

#[tokio::test]
async fn returns_the_git_top_level_root_path_when_resolving_from_a_nested_workspace() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let nested = root.join("apps/web");
    std::fs::create_dir_all(&nested).unwrap();
    let identity = resolve(&nested).await.unwrap();
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(
        dunce::canonicalize(identity.root_path.as_deref().unwrap()).unwrap(),
        root
    );
}

#[tokio::test]
async fn returns_null_for_non_git_folders_and_repos_without_remotes() {
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(resolve(plain.path()).await, None);
    let (_directory, root) = repository();
    assert_eq!(resolve(&root).await, None);
}

#[tokio::test]
async fn prefers_upstream_over_origin() {
    let (_directory, root) = repository();
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:julius/widget.git",
        ],
    );
    git(
        &root,
        &[
            "remote",
            "add",
            "upstream",
            "git@github.com:Acme/widget.git",
        ],
    );
    let identity = resolve(&root).await.unwrap();
    assert_eq!(identity.locator.remote_name, "upstream");
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(identity.display_name.as_deref(), Some("acme/widget"));
}

#[tokio::test]
async fn falls_back_to_the_first_remote_in_locale_order() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "Zeta", "git@github.com:acme/zeta.git"],
    );
    git(
        &root,
        &["remote", "add", "beta", "git@github.com:acme/beta.git"],
    );
    let identity = resolve(&root).await.unwrap();
    assert_eq!(identity.locator.remote_name, "beta");
    assert_eq!(identity.canonical_key, "github.com/acme/beta");
}

#[tokio::test]
async fn refreshes_the_primary_upstream_after_add_or_replace_before_cache_expiry() {
    for change in ["add", "replace"] {
        let (_directory, root) = repository();
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:julius/widget.git",
            ],
        );
        if change == "replace" {
            git(
                &root,
                &[
                    "remote",
                    "add",
                    "upstream",
                    "git@github.com:Acme/previous.git",
                ],
            );
        }
        let resolver = Resolver::new(options(&Clock::default()));
        let initial = resolver.resolve(path(&root), false).await.unwrap();
        assert_eq!(
            initial.as_ref().unwrap().canonical_key,
            if change == "add" {
                "github.com/julius/widget"
            } else {
                "github.com/acme/previous"
            },
            "{change}"
        );

        git(
            &root,
            &[
                "remote",
                if change == "add" { "add" } else { "set-url" },
                "upstream",
                "git@github.com:Acme/widget.git",
            ],
        );
        assert_eq!(resolver.resolve(path(&root), false).await.unwrap(), initial);
        let identity = resolver.resolve(path(&root), true).await.unwrap();
        let refreshed = identity.as_ref().unwrap();
        assert_eq!(refreshed.locator.remote_name, "upstream", "{change}");
        assert_eq!(refreshed.canonical_key, "github.com/acme/widget");
        assert_eq!(refreshed.display_name.as_deref(), Some("acme/widget"));
        assert_eq!(
            resolver.resolve(path(&root), false).await.unwrap(),
            identity
        );
    }
}

#[tokio::test]
async fn uses_the_last_remote_path_segment_as_the_repository_name_for_nested_groups() {
    let (_directory, root) = repository();
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "git@gitlab.com:Acme/platform/widget.git",
        ],
    );
    let identity = resolve(&root).await.unwrap();
    assert_eq!(identity.canonical_key, "gitlab.com/acme/platform/widget");
    assert_eq!(
        identity.display_name.as_deref(),
        Some("acme/platform/widget")
    );
    assert_eq!(identity.owner.as_deref(), Some("acme"));
    assert_eq!(identity.name.as_deref(), Some("widget"));
    assert_eq!(identity.provider.as_deref(), Some("gitlab"));
}

#[tokio::test]
async fn keeps_null_identities_cached_across_repeated_resolves_until_the_negative_ttl_expires() {
    let (_directory, root) = repository();
    let clock = Clock::default();
    let mut options = options(&clock);
    options.negative_ttl = Duration::from_millis(50);
    options.positive_ttl = Duration::from_secs(1);
    let resolver = Resolver::new(options);
    assert_eq!(resolver.resolve(path(&root), false).await.unwrap(), None);

    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    for _attempt in 0..3 {
        assert_eq!(resolver.resolve(path(&root), false).await.unwrap(), None);
    }

    clock.advance(Duration::from_millis(120));
    let refreshed = resolver.resolve(path(&root), false).await.unwrap().unwrap();
    assert_eq!(refreshed.canonical_key, "github.com/acme/widget");
    assert_eq!(refreshed.name.as_deref(), Some("widget"));
}

#[tokio::test]
async fn refreshes_cached_identities_after_the_positive_ttl_when_a_remote_changes() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let clock = Clock::default();
    let mut options = options(&clock);
    options.negative_ttl = Duration::from_millis(50);
    options.positive_ttl = Duration::from_millis(100);
    let resolver = Resolver::new(options);
    let initial = resolver.resolve(path(&root), false).await.unwrap().unwrap();
    assert_eq!(initial.canonical_key, "github.com/acme/widget");

    git(
        &root,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:Acme/widget-next.git",
        ],
    );
    let cached = resolver.resolve(path(&root), false).await.unwrap().unwrap();
    assert_eq!(cached.canonical_key, "github.com/acme/widget");

    clock.advance(Duration::from_millis(180));
    let refreshed = resolver.resolve(path(&root), false).await.unwrap().unwrap();
    assert_eq!(refreshed.canonical_key, "github.com/acme/widget-next");
    assert_eq!(refreshed.display_name.as_deref(), Some("acme/widget-next"));
    assert_eq!(refreshed.name.as_deref(), Some("widget-next"));
}

#[test]
fn detects_the_provider_from_the_remote_host() {
    for (remote, kind) in [
        ("https://codeberg.org/team/repo.git", "forgejo"),
        ("ssh://git@ssh.forge.gitea.example/team/repo", "forgejo"),
        ("https://github.example.com/team/repo", "github"),
        ("git@gitlab.internal:team/repo.git", "gitlab"),
        ("git@ssh.dev.azure.com:v3/org/project/repo", "azure-devops"),
        ("https://bitbucket.org/team/repo", "bitbucket"),
        ("https://forge.test:3000/team/repo", "unknown"),
    ] {
        assert_eq!(provider_kind(remote), Some(kind), "{remote}");
    }
    assert_eq!(provider_kind(""), None);
}

// sourceControl.test.ts "detectSourceControlProviderFromRemoteUrl": the kinds.
#[test]
fn detects_common_source_control_hosts() {
    for (remote, kind) in [
        ("git@github.com:owner/repo.git", "github"),
        ("https://gitlab.com/group/repo.git", "gitlab"),
        (
            "https://dev.azure.com/org/project/_git/repo",
            "azure-devops",
        ),
        ("git@bitbucket.org:workspace/repo.git", "bitbucket"),
    ] {
        assert_eq!(provider_kind(remote), Some(kind), "{remote}");
    }
}

#[test]
fn detects_forgejo_and_gitea_hosts_while_preserving_http_origins() {
    for host in ["codeberg.org", "forgejo.example.test", "gitea.example.test"] {
        let remote = format!("http://{host}:3000/team/repo.git");
        assert_eq!(provider_kind(&remote), Some("forgejo"), "{remote}");
        assert_eq!(remote_host(&remote), Some(format!("{host}:3000")));
    }
}

#[test]
fn detects_azure_devops_ssh_remotes() {
    for remote in [
        "git@ssh.dev.azure.com:v3/org/project/repo",
        "ssh://git@ssh.dev.azure.com:22/v3/org/project/repo",
        "git@vs-ssh.visualstudio.com:v3/org/project/repo",
    ] {
        assert_eq!(provider_kind(remote), Some("azure-devops"), "{remote}");
    }
}

#[test]
fn preserves_ports_while_classifying_by_hostname() {
    assert_eq!(
        provider_kind("https://gitlab.com:8443/group/repo.git"),
        Some("gitlab")
    );
    assert_eq!(
        remote_host("https://gitlab.com:8443/group/repo.git").as_deref(),
        Some("gitlab.com:8443")
    );
    assert_eq!(
        provider_kind("https://self-hosted.example.test:8443/group/repo.git"),
        Some("unknown")
    );
    assert_eq!(
        remote_host("https://self-hosted.example.test:8443/group/repo.git").as_deref(),
        Some("self-hosted.example.test:8443")
    );
}

#[test]
fn does_not_reuse_ssh_ports_for_https_provider_urls() {
    assert_eq!(
        provider_kind("ssh://git@gitlab.example.test:24/group/repo.git"),
        Some("gitlab")
    );
    assert_eq!(
        remote_host("ssh://git@gitlab.example.test:24/group/repo.git").as_deref(),
        Some("gitlab.example.test")
    );
    assert_eq!(
        provider_kind("ssh://git@code.example.test:24/team/project.git"),
        Some("unknown")
    );
    assert_eq!(
        remote_host("ssh://git@code.example.test:24/team/project.git").as_deref(),
        Some("code.example.test")
    );
}

#[test]
fn matches_self_hosted_providers_by_complete_dns_labels() {
    for (remote, kind) in [
        ("https://github.example.com/owner/repo.git", "github"),
        ("https://gitlab.example.com/group/repo.git", "gitlab"),
        (
            "https://bitbucket.example.com/workspace/repo.git",
            "bitbucket",
        ),
        ("https://notgithub.example.com/owner/repo.git", "unknown"),
        ("https://notgitlab.example.com/group/repo.git", "unknown"),
        (
            "https://notbitbucket.example.com/workspace/repo.git",
            "unknown",
        ),
    ] {
        assert_eq!(provider_kind(remote), Some(kind), "{remote}");
    }
}

#[test]
fn detects_ssh_remotes_with_non_git_ssh_users() {
    assert_eq!(
        provider_kind("gitlab@gitlab.example.com:group/project.git"),
        Some("gitlab")
    );
    assert_eq!(
        remote_host("gitlab@gitlab.example.com:group/project.git").as_deref(),
        Some("gitlab.example.com")
    );
    assert_eq!(
        provider_kind("deploy@github.com:owner/repo.git"),
        Some("github")
    );
    assert_eq!(
        provider_kind("git@bitbucket.org:workspace/repo.git"),
        Some("bitbucket")
    );
}

fn neutral_label() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::strategy::Strategy as _;
    "[a-z][a-z0-9]{0,6}".prop_filter("a label no provider is named by", |label| {
        ![
            "github",
            "gitlab",
            "bitbucket",
            "forgejo",
            "gitea",
            "dev",
            "azure",
        ]
        .contains(&label.as_str())
    })
}

proptest::proptest! {
    // A host is classified by whole DNS labels, the same through every remote form.
    #[test]
    fn classifies_every_remote_form_of_a_host_alike(
        labels in proptest::collection::vec(neutral_label(), 1..4),
        provider in proptest::sample::select(vec!["github", "gitlab", "bitbucket", "forgejo", "gitea"]),
        port in 1u16..65535,
        path in "[a-z]{1,6}/[a-z]{1,6}",
    ) {
        let mut parts = labels.clone();
        parts.insert(labels.len() / 2, provider.to_owned());
        let host = parts.join(".");
        let expected = match provider {
            "gitea" | "forgejo" => "forgejo",
            other => other,
        };
        for remote in [
            format!("git@{host}:{path}.git"),
            format!("ssh://git@{host}:{port}/{path}.git"),
            format!("https://{host}:{port}/{path}"),
            format!("http://{host}/{path}.git"),
        ] {
            proptest::prop_assert_eq!(provider_kind(&remote), Some(expected), "{}", remote);
        }
        let unrelated = format!("https://x{provider}.{}/{path}", labels.join("."));
        proptest::prop_assert_eq!(provider_kind(&unrelated), Some("unknown"));
    }
}
