use super::{tests::git, *};
use agent_protocol::operations::WorkspaceAction;

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "--initial-branch=main"]);
    git(directory.path(), &["commit", "--allow-empty", "-m", "base"]);
    directory
}

#[test]
fn selected_comparison_changes_diff_without_mutating_default_or_index() {
    let directory = repository();
    let cwd = directory.path();
    git(cwd, &["switch", "-c", "task"]);
    git(
        cwd,
        &["config", &comparison::base_key("task"), "refs/heads/main"],
    );
    std::fs::write(cwd.join("committed"), "one\ntwo\n").unwrap();
    git(cwd, &["add", "."]);
    git(cwd, &["commit", "-m", "task work"]);
    std::fs::write(cwd.join("pending"), "three\n").unwrap();
    git(cwd, &["add", "."]);
    let index = std::fs::read(cwd.join(".git/index")).unwrap();
    let all = collect_workspace_review(cwd.into(), None).unwrap();
    assert_eq!((all.additions, all.deletions), (3, 0));
    let pending = collect_workspace_review(cwd.into(), Some("HEAD")).unwrap();
    assert_eq!(pending.base_ref.as_deref(), Some("HEAD"));
    assert_eq!((pending.additions, pending.deletions), (1, 0));
    assert_eq!(
        pending
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["pending"]
    );
    assert!(pending.diff.contains("+three"));
    for reference in ["missing", "--all", "HEAD:committed"] {
        assert!(collect_workspace_review(cwd.into(), Some(reference)).is_err());
    }
    assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
    assert_eq!(collect_workspace_review(cwd.into(), None).unwrap(), all);
}

#[test]
fn branch_actions_reject_stale_state_conflicts_and_occupied_worktrees() {
    let directory = repository();
    let cwd = directory.path();
    git(cwd, &["switch", "-c", "other"]);
    std::fs::write(cwd.join("conflict"), "other\n").unwrap();
    git(cwd, &["add", "."]);
    git(cwd, &["commit", "-m", "other work"]);
    git(cwd, &["switch", "main"]);
    std::fs::write(cwd.join("conflict"), "local\n").unwrap();
    let switch = WorkspaceAction::SwitchBranch {
        name: "other".into(),
    };
    assert!(controls::change(cwd, "old", &switch).is_err());
    assert!(controls::change(cwd, "main", &switch).is_err());
    assert_eq!(
        std::fs::read_to_string(cwd.join("conflict")).unwrap(),
        "local\n"
    );
    assert_eq!(git(cwd, &["branch", "--show-current"]).trim(), "main");
    assert!(
        controls::change(
            cwd,
            "main",
            &WorkspaceAction::CreateBranch {
                name: "--detach".into()
            }
        )
        .is_err()
    );
    let linked = directory.path().join("linked");
    git(cwd, &["worktree", "add", linked.to_str().unwrap(), "other"]);
    assert!(controls::change(cwd, "main", &switch).is_err());
    git(cwd, &["switch", "--detach", "HEAD"]);
    let detached = git(cwd, &["rev-parse", "--short", "HEAD"]);
    assert!(!controls::inspect(cwd).unwrap().can_push);
    controls::change(
        cwd,
        detached.trim(),
        &WorkspaceAction::CreateBranch {
            name: "fresh".into(),
        },
    )
    .unwrap();
    assert_eq!(git(cwd, &["branch", "--show-current"]).trim(), "fresh");
    controls::change(
        cwd,
        "fresh",
        &WorkspaceAction::SwitchBranch {
            name: "main".into(),
        },
    )
    .unwrap();
}

#[test]
fn push_uses_selected_remote_and_configured_upstream_branch() {
    let directory = repository();
    let cwd = directory.path();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare"]);
    git(
        cwd,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    assert!(
        controls::change(
            cwd,
            "main",
            &WorkspaceAction::Push {
                remote: "missing".into()
            }
        )
        .is_err()
    );
    controls::change(
        cwd,
        "main",
        &WorkspaceAction::Push {
            remote: "origin".into(),
        },
    )
    .unwrap();
    assert_eq!(
        git(remote.path(), &["rev-parse", "refs/heads/main"]),
        git(cwd, &["rev-parse", "HEAD"])
    );
    git(
        cwd,
        &["config", "branch.main.merge", "refs/heads/published"],
    );
    git(cwd, &["commit", "--allow-empty", "-m", "next"]);
    controls::change(
        cwd,
        "main",
        &WorkspaceAction::Push {
            remote: "origin".into(),
        },
    )
    .unwrap();
    assert_eq!(
        git(remote.path(), &["rev-parse", "refs/heads/published"]),
        git(cwd, &["rev-parse", "HEAD"])
    );
    git(cwd, &["switch", "--detach"]);
    let detached = git(cwd, &["rev-parse", "--short", "HEAD"]);
    assert!(
        controls::change(
            cwd,
            detached.trim(),
            &WorkspaceAction::Push {
                remote: "origin".into()
            }
        )
        .is_err()
    );
}

#[test]
fn branch_and_tag_with_the_same_name_remain_distinct_controls_and_comparisons() {
    let directory = repository();
    let cwd = directory.path();
    git(cwd, &["tag", "main"]);
    git(cwd, &["config", "core.warnAmbiguousRefs", "false"]);
    std::fs::write(cwd.join("branch-work"), "branch work\n").unwrap();
    git(cwd, &["add", "."]);
    git(cwd, &["commit", "-m", "branch work"]);
    let review = collect_workspace_review(cwd.into(), None).unwrap();
    assert_eq!(review.branch, "main");
    assert_eq!(review.controls.branches, ["main"]);
    assert!(review.controls.references.contains(&"heads/main".into()));
    assert!(review.controls.references.contains(&"tags/main".into()));
    assert_eq!(
        collect_workspace_review(cwd.into(), Some("heads/main"))
            .unwrap()
            .additions,
        0
    );
    assert_eq!(
        collect_workspace_review(cwd.into(), Some("tags/main"))
            .unwrap()
            .additions,
        1
    );
    controls::change(
        cwd,
        "main",
        &WorkspaceAction::CreateBranch {
            name: "other".into(),
        },
    )
    .unwrap();
    controls::change(
        cwd,
        "other",
        &WorkspaceAction::SwitchBranch {
            name: "main".into(),
        },
    )
    .unwrap();
    assert_eq!(
        collect_workspace_review(cwd.into(), None).unwrap().branch,
        "main"
    );
}
