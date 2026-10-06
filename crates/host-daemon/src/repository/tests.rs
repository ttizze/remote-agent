//! The resolution cases apply; the cache cases do not, since the Host
//! resolves when it lists projects.
use super::*;

fn git(cwd: &Path, args: &[&str]) {
    crate::git::text(cwd, args).unwrap();
}

fn repository() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    git(&root, &["init", "--quiet"]);
    (directory, root)
}

#[test]
fn normalizes_equivalent_github_remotes_into_a_stable_repository_identity() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let identity = resolve(&root).unwrap();
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

#[test]
fn returns_the_git_top_level_root_path_when_resolving_from_a_nested_workspace() {
    let (_directory, root) = repository();
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let nested = root.join("apps/web");
    std::fs::create_dir_all(&nested).unwrap();
    let identity = resolve(&nested).unwrap();
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(
        dunce::canonicalize(identity.root_path.as_deref().unwrap()).unwrap(),
        root
    );
}

#[test]
fn returns_null_for_non_git_folders_and_repos_without_remotes() {
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(resolve(plain.path()), None);
    let (_directory, root) = repository();
    assert_eq!(resolve(&root), None);
}

#[test]
fn prefers_upstream_over_origin() {
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
    let identity = resolve(&root).unwrap();
    assert_eq!(identity.locator.remote_name, "upstream");
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(identity.display_name.as_deref(), Some("acme/widget"));
}

#[test]
fn uses_the_last_remote_path_segment_as_the_repository_name_for_nested_groups() {
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
    let identity = resolve(&root).unwrap();
    assert_eq!(identity.canonical_key, "gitlab.com/acme/platform/widget");
    assert_eq!(
        identity.display_name.as_deref(),
        Some("acme/platform/widget")
    );
    assert_eq!(identity.owner.as_deref(), Some("acme"));
    assert_eq!(identity.name.as_deref(), Some("widget"));
    assert_eq!(identity.provider.as_deref(), Some("gitlab"));
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
