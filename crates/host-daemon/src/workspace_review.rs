pub(crate) mod comparison;

use agent_protocol::models::{ChangedFile, WorkspaceReview};
use anyhow::{Context as _, Result, anyhow};
use comparison::Comparison;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub async fn inspect_workspace(cwd: String) -> Result<WorkspaceReview> {
    tokio::task::spawn_blocking(move || collect_workspace_review(PathBuf::from(cwd)))
        .await
        .context("failed to inspect workspace")?
}

fn collect_workspace_review(cwd: PathBuf) -> Result<WorkspaceReview> {
    // A submission can replace a deleted worktree between Git commands.
    let review = review_existing_workspace(&cwd).or_else(|_| review_existing_workspace(&cwd));
    if review.is_err()
        && matches!(std::fs::metadata(&cwd), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(empty_review());
    }
    review
}

fn review_existing_workspace(cwd: &Path) -> Result<WorkspaceReview> {
    let Some(comparison) = Comparison::open(cwd)? else {
        return Ok(empty_review());
    };
    let mut files = parse_files(&comparison.diff("--name-status")?);
    let mut counts: HashMap<_, _> = files
        .iter_mut()
        .map(|file| {
            (
                file.path.as_bytes(),
                (&mut file.additions, &mut file.deletions),
            )
        })
        .collect();
    let (additions, deletions) =
        parse_numstat(&comparison.diff("--numstat")?, |path, added, deleted| {
            if let Some((additions, deletions)) = counts.get_mut(path) {
                **additions = added;
                **deletions = deleted;
            }
        });
    let patch = comparison.diff("--patch")?;
    if patch.len() > 8 * 1024 * 1024 {
        return Err(anyhow!("working-tree diff exceeds 8 MiB"));
    }
    Ok(WorkspaceReview {
        branch: comparison.branch,
        base_ref: comparison.base_ref,
        additions,
        deletions,
        files,
        diff: String::from_utf8_lossy(&patch).into_owned(),
    })
}

pub(crate) async fn directory_changes(cwds: HashSet<String>) -> Result<HashMap<String, bool>> {
    use futures_util::{StreamExt as _, TryStreamExt as _};
    let results: Vec<_> = futures_util::stream::iter(cwds)
        .map(|cwd| async move {
            tokio::task::spawn_blocking(move || {
                let comparison = Comparison::open(Path::new(&cwd)).ok().flatten()?;
                let changed = !comparison.diff("--name-status").ok()?.is_empty();
                Some((cwd, changed))
            })
            .await
        })
        .buffer_unordered(4)
        .try_collect()
        .await?;
    Ok(results.into_iter().flatten().collect())
}

fn empty_review() -> WorkspaceReview {
    WorkspaceReview {
        branch: String::new(),
        base_ref: None,
        additions: 0,
        deletions: 0,
        files: Vec::new(),
        diff: String::new(),
    }
}

fn parse_files(output: &[u8]) -> Vec<ChangedFile> {
    let mut entries = output.split(|byte| *byte == 0);
    let mut files = Vec::new();
    while let Some(code) = entries.next().filter(|code| !code.is_empty()) {
        let Some(mut path) = entries.next() else {
            break;
        };
        let status = match code[0] {
            b'A' => "added",
            b'D' => "deleted",
            b'R' | b'C' => "renamed",
            _ => "modified",
        };
        if status == "renamed" {
            let Some(destination) = entries.next() else {
                break;
            };
            path = destination;
        }
        files.push(ChangedFile {
            path: String::from_utf8_lossy(path).into_owned(),
            status: status.into(),
            additions: Some(0),
            deletions: Some(0),
        });
        if files.len() == 200 {
            break;
        }
    }
    files
}

fn parse_numstat(
    output: &[u8],
    mut file: impl FnMut(&[u8], Option<u64>, Option<u64>),
) -> (u64, u64) {
    let mut entries = output.split(|byte| *byte == 0);
    let (mut additions, mut deletions) = (0, 0);
    while let Some(entry) = entries.next() {
        let mut fields = entry.splitn(3, |byte| *byte == b'\t');
        let count = |field: Option<&[u8]>| {
            field.and_then(|value| std::str::from_utf8(value).ok()?.parse::<u64>().ok())
        };
        let added = count(fields.next());
        let deleted = count(fields.next());
        let Some(mut path) = fields.next() else {
            continue;
        };
        if path.is_empty() {
            // With -z, renames contain separate source and destination records.
            entries.next();
            let Some(destination) = entries.next() else {
                break;
            };
            path = destination;
        }
        additions += added.unwrap_or(0);
        deletions += deleted.unwrap_or(0);
        file(path, added, deleted);
    }
    (additions, deletions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let mut configured = vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        configured.extend_from_slice(args);
        crate::git::text(cwd, &configured).unwrap()
    }

    async fn assert_changes(cwd: &Path, paths: &[&str], lines: (u64, u64)) -> WorkspaceReview {
        let cwd_text = cwd.to_str().unwrap().to_owned();
        let review = inspect_workspace(cwd_text.clone()).await.unwrap();
        assert_eq!((review.additions, review.deletions), lines);
        let actual: std::collections::BTreeSet<_> =
            review.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(actual, paths.iter().copied().collect());
        let marks = directory_changes([cwd_text.clone()].into()).await.unwrap();
        assert_eq!(marks.get(&cwd_text), Some(&!paths.is_empty()));
        review
    }

    #[tokio::test]
    async fn changes_and_marks_follow_the_worktrees_source_through_edits_commits_and_integration() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--initial-branch=main"]);
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "base"]);
        git(root, &["switch", "-c", "source"]);
        std::fs::write(root.join("inherited.txt"), "existing source work\n").unwrap();
        git(root, &["add", "inherited.txt"]);
        git(root, &["commit", "-m", "source work"]);
        let store = crate::worktrees::Worktrees::new(&root.join("projects.json"));
        store
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let task = store
            .prepare(root.to_str(), || async { Ok(()) })
            .await
            .unwrap()
            .unwrap();
        let branch = git(&task, &["branch", "--show-current"]).trim().to_owned();
        let key = comparison::base_key(&branch);
        assert_eq!(
            git(&task, &["config", "--get", &key]).trim(),
            "refs/heads/source"
        );
        assert_eq!(
            assert_changes(&task, &[], (0, 0)).await.base_ref.as_deref(),
            Some("source")
        );
        assert!(!git(&task, &["diff", "--name-only", "main...HEAD"]).is_empty());

        std::fs::write(root.join("source-only.txt"), "newer source work\n").unwrap();
        git(root, &["add", "source-only.txt"]);
        git(root, &["commit", "-m", "advance source"]);
        assert_changes(&task, &[], (0, 0)).await;
        std::fs::write(task.join("tracked.txt"), "base\nmine\n").unwrap();
        std::fs::write(task.join("staged.txt"), "staged\n").unwrap();
        git(&task, &["add", "staged.txt"]);
        std::fs::create_dir(task.join("notes")).unwrap();
        std::fs::write(task.join("notes/new.txt"), "one\ntwo\n").unwrap();
        let index = PathBuf::from(git(&task, &["rev-parse", "--git-path", "index"]).trim());
        let before = std::fs::read(task.join(&index)).unwrap();
        assert_changes(
            &task,
            &["tracked.txt", "staged.txt", "notes/new.txt"],
            (4, 0),
        )
        .await;
        assert_eq!(
            std::fs::read(task.join(&index)).unwrap(),
            before,
            "review must preserve staging"
        );
        assert_changes(
            &task.join("notes"),
            &["tracked.txt", "staged.txt", "notes/new.txt"],
            (4, 0),
        )
        .await;
        git(&task, &["add", "."]);
        git(&task, &["commit", "-m", "task work"]);
        assert_changes(
            &task,
            &["tracked.txt", "staged.txt", "notes/new.txt"],
            (4, 0),
        )
        .await;
        git(&task, &["branch", "-m", "renamed-task"]);
        assert_changes(
            &task,
            &["tracked.txt", "staged.txt", "notes/new.txt"],
            (4, 0),
        )
        .await;
        git(root, &["merge", "--no-edit", "renamed-task"]);
        assert_changes(&task, &[], (0, 0)).await;

        // Staging can differ even when the final file content matches the base.
        std::fs::write(task.join("tracked.txt"), "staged change\n").unwrap();
        git(&task, &["add", "tracked.txt"]);
        std::fs::write(task.join("tracked.txt"), "base\nmine\n").unwrap();
        assert_changes(&task, &[], (0, 0)).await;
    }

    #[tokio::test]
    async fn missing_or_deleted_source_falls_back_to_main_and_clears_integrated_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        git(cwd, &["commit", "--allow-empty", "-m", "base"]);
        git(cwd, &["switch", "-c", "source"]);
        std::fs::write(cwd.join("inherited.txt"), "existing work\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "source work"]);
        git(cwd, &["switch", "-c", "task", "HEAD"]);
        assert_changes(cwd, &["inherited.txt"], (1, 0)).await;
        git(cwd, &["commit", "--allow-empty", "-m", "no file changes"]);
        assert_changes(cwd, &["inherited.txt"], (1, 0)).await;
        std::fs::write(cwd.join("own.txt"), "own work\n").unwrap();
        git(cwd, &["add", "own.txt"]);
        git(cwd, &["commit", "-m", "own work"]);
        assert_changes(cwd, &["inherited.txt", "own.txt"], (2, 0)).await;
        git(cwd, &["revert", "--no-edit", "HEAD"]);
        assert_changes(cwd, &["inherited.txt"], (1, 0)).await;
        git(
            cwd,
            &[
                "config",
                &comparison::base_key("task"),
                "refs/heads/deleted",
            ],
        );
        assert_eq!(
            assert_changes(cwd, &["inherited.txt"], (1, 0))
                .await
                .base_ref
                .as_deref(),
            Some("main")
        );
        git(cwd, &["switch", "main"]);
        git(cwd, &["merge", "--no-edit", "task"]);
        git(cwd, &["switch", "task"]);
        assert_changes(cwd, &[], (0, 0)).await;
        std::fs::write(cwd.join("pending.txt"), "not integrated\n").unwrap();
        assert_changes(cwd, &["pending.txt"], (1, 0)).await;
    }

    #[tokio::test]
    async fn fallback_follows_the_primary_remotes_default_branch_without_network_access() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        git(cwd, &["commit", "--allow-empty", "-m", "base"]);
        git(cwd, &["switch", "-c", "trunk"]);
        std::fs::write(cwd.join("inherited.txt"), "default branch work\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "trunk work"]);
        git(cwd, &["remote", "add", "mirror", "./unreachable"]);
        git(cwd, &["update-ref", "refs/remotes/mirror/trunk", "HEAD"]);
        git(
            cwd,
            &[
                "symbolic-ref",
                "refs/remotes/mirror/HEAD",
                "refs/remotes/mirror/trunk",
            ],
        );
        git(cwd, &["switch", "-c", "task", "HEAD"]);
        std::fs::write(cwd.join("own.txt"), "task work\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "task work"]);
        assert_eq!(
            assert_changes(cwd, &["own.txt"], (1, 0))
                .await
                .base_ref
                .as_deref(),
            Some("mirror/trunk")
        );

        git(cwd, &["remote", "add", "origin", "./also-unreachable"]);
        git(cwd, &["update-ref", "refs/remotes/origin/release", "main"]);
        git(
            cwd,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/release",
            ],
        );
        assert_eq!(
            assert_changes(cwd, &["inherited.txt", "own.txt"], (2, 0))
                .await
                .base_ref
                .as_deref(),
            Some("origin/release")
        );
        git(cwd, &["update-ref", "refs/remotes/origin/release", "HEAD"]);
        assert_changes(cwd, &[], (0, 0)).await;
    }

    #[tokio::test]
    async fn default_branch_uses_its_tracked_remote_and_detached_heads_only_show_pending_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        git(cwd, &["commit", "--allow-empty", "-m", "base"]);
        git(cwd, &["remote", "add", "origin", "./unreachable"]);
        git(cwd, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(cwd.join("own.txt"), "local commit\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "local work"]);
        assert_changes(cwd, &["own.txt"], (1, 0)).await;

        git(cwd, &["remote", "add", "other", "./also-unreachable"]);
        git(cwd, &["update-ref", "refs/remotes/other/stable", "HEAD"]);
        git(cwd, &["branch", "--set-upstream-to=other/stable"]);
        assert_eq!(
            assert_changes(cwd, &[], (0, 0)).await.base_ref.as_deref(),
            Some("other/stable")
        );
        git(cwd, &["checkout", "--detach"]);
        assert_eq!(
            assert_changes(cwd, &[], (0, 0)).await.base_ref.as_deref(),
            Some("HEAD")
        );
        std::fs::write(cwd.join("own.txt"), "pending edit\n").unwrap();
        assert_changes(cwd, &["own.txt"], (1, 1)).await;
    }

    #[tokio::test]
    async fn same_size_edits_with_racy_index_timestamps_remain_visible() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        git(cwd, &["config", "core.trustctime", "false"]);
        let path = cwd.join("own.txt");
        let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let set_modified = |path: &Path| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(timestamp)
                .unwrap();
        };
        std::fs::write(&path, "local commit\n").unwrap();
        set_modified(&path);
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "base"]);
        let index_path = cwd.join(".git/index");
        set_modified(&index_path);
        let index = std::fs::read(&index_path).unwrap();
        std::fs::write(&path, "pending edit\n").unwrap();
        set_modified(&path);
        assert_changes(cwd, &["own.txt"], (1, 1)).await;
        assert_eq!(std::fs::read(&index_path).unwrap(), index);
        assert_eq!(
            std::fs::metadata(index_path).unwrap().modified().unwrap(),
            timestamp
        );
    }

    #[tokio::test]
    async fn repositories_without_a_comparison_branch_only_show_pending_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=develop"]);
        git(cwd, &["commit", "--allow-empty", "-m", "base"]);
        git(cwd, &["switch", "-c", "task"]);
        std::fs::write(cwd.join("own.txt"), "committed\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "task work"]);
        assert_eq!(
            assert_changes(cwd, &[], (0, 0)).await.base_ref.as_deref(),
            Some("HEAD")
        );
        std::fs::write(cwd.join("own.txt"), "pending\n").unwrap();
        assert_changes(cwd, &["own.txt"], (1, 1)).await;
        std::fs::write(cwd.join("own.txt"), "committed\n").unwrap();
        git(cwd, &["branch", "master", "HEAD^"]);
        assert_changes(cwd, &["own.txt"], (1, 0)).await;
        git(cwd, &["branch", "main"]);
        assert_changes(cwd, &[], (0, 0)).await;
    }

    #[tokio::test]
    async fn unrelated_history_remains_an_error_instead_of_hiding_committed_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        git(cwd, &["commit", "--allow-empty", "-m", "base"]);
        git(cwd, &["switch", "--orphan", "unrelated"]);
        std::fs::write(cwd.join("own.txt"), "unrelated work\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "separate history"]);
        std::fs::write(cwd.join("pending.txt"), "pending\n").unwrap();
        // T3's resolveReviewMergeBase errors when an existing comparison ref
        // shares no ancestor; falling back to HEAD would conceal committed work.
        let cwd_text = cwd.to_str().unwrap().to_owned();
        assert!(inspect_workspace(cwd_text.clone()).await.is_err());
        let marks = directory_changes([cwd_text.clone()].into()).await.unwrap();
        assert_eq!(marks.get(&cwd_text), None, "unknown is not a clean result");
    }

    #[tokio::test]
    async fn zero_line_file_changes_still_show_the_same_changes_mark() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--initial-branch=main"]);
        std::fs::write(cwd.join("old.txt"), "same content\n").unwrap();
        std::fs::write(cwd.join("binary.dat"), b"before\0").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "base"]);
        git(cwd, &["switch", "-c", "task"]);
        std::fs::rename(cwd.join("old.txt"), cwd.join("new.txt")).unwrap();
        std::fs::write(cwd.join("binary.dat"), b"after\0").unwrap();
        std::fs::write(cwd.join("empty.txt"), b"").unwrap();
        assert_changes(cwd, &["binary.dat", "empty.txt", "new.txt"], (0, 0)).await;
    }

    #[tokio::test]
    async fn dirty_submodules_remain_visible_in_both_changes_and_marks() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let repo = directory.path().join("repo");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&repo).unwrap();
        git(&source, &["init", "--initial-branch=main"]);
        std::fs::write(source.join("tracked.txt"), "original\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "submodule base"]);
        git(&repo, &["init", "--initial-branch=main"]);
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                source.to_str().unwrap(),
                "sub",
            ],
        );
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["switch", "-c", "task"]);
        std::fs::write(repo.join("sub/tracked.txt"), "pending\n").unwrap();
        let cwd = repo.to_str().unwrap().to_owned();
        let review = inspect_workspace(cwd.clone()).await.unwrap();
        assert_eq!(
            review
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["sub"]
        );
        assert!(review.diff.contains("-dirty"));
        let marks = directory_changes([cwd.clone()].into()).await.unwrap();
        assert_eq!(marks.get(&cwd), Some(&true));
    }

    #[test]
    fn folders_without_a_git_worktree_have_no_changes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("photo.png"), b"untracked outside Git").unwrap();
        for bare in [false, true] {
            if bare {
                crate::git::text(
                    directory.path(),
                    &["init", "--bare", "--initial-branch=main"],
                )
                .unwrap();
            }
            let review = collect_workspace_review(directory.path().into()).unwrap();
            assert!(review.branch.is_empty());
            assert!(review.base_ref.is_none());
            assert_eq!((review.additions, review.deletions), (0, 0));
            assert!(review.files.is_empty());
            assert!(review.diff.is_empty());
        }
    }

    #[test]
    fn deleted_workspaces_have_no_changes_but_invalid_workspaces_remain_errors() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            collect_workspace_review(directory.path().join("missing"))
                .unwrap()
                .files
                .is_empty()
        );
        let file = directory.path().join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert!(collect_workspace_review(file).is_err());
        std::fs::write(directory.path().join(".git"), b"gitdir: missing\n").unwrap();
        assert!(collect_workspace_review(directory.path().into()).is_err());
    }

    #[test]
    fn untracked_files_contribute_to_patch_and_line_counts() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        crate::git::text(cwd, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(cwd.join("notes.txt"), "first\nsecond\n").unwrap();
        let review = collect_workspace_review(cwd.to_path_buf()).unwrap();
        assert!(
            review.base_ref.is_none(),
            "an unborn branch has no comparison ref"
        );
        assert_eq!((review.additions, review.deletions), (2, 0));
        assert!(review.diff.starts_with("diff --git "));
        assert!(review.diff.contains("+first\n+second\n"));
        assert_eq!(review.files[0].status, "added");
        assert_eq!(
            (review.files[0].additions, review.files[0].deletions),
            (Some(2), Some(0))
        );
    }

    #[test]
    fn file_counts_follow_renames_deletions_and_binary_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        let (deleted, renamed) = if cfg!(windows) {
            ("gone file.txt", "new name.txt")
        } else {
            ("gone\tfile\n.txt", "new\tname\n.txt")
        };
        crate::git::text(cwd, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(cwd.join("old.txt"), "first\nsecond\nthird\n").unwrap();
        std::fs::write(cwd.join(deleted), "one\ntwo\n").unwrap();
        std::fs::write(cwd.join("binary.dat"), b"before\0").unwrap();
        crate::git::text(cwd, &["add", "."]).unwrap();
        git(cwd, &["commit", "-m", "initial"]);
        crate::git::text(cwd, &["mv", "old.txt", renamed]).unwrap();
        std::fs::write(cwd.join(renamed), "first\nsecond\nthird\nfourth\n").unwrap();
        std::fs::remove_file(cwd.join(deleted)).unwrap();
        std::fs::write(cwd.join("binary.dat"), b"after\0").unwrap();
        let review = collect_workspace_review(cwd.to_path_buf()).unwrap();
        assert_eq!((review.additions, review.deletions), (1, 2));
        assert_eq!(review.files.len(), 3);
        for (path, status, additions, deletions) in [
            (renamed, "renamed", Some(1), Some(0)),
            (deleted, "deleted", Some(0), Some(2)),
            ("binary.dat", "modified", None, None),
        ] {
            let file = review.files.iter().find(|file| file.path == path).unwrap();
            assert_eq!(
                (file.status.as_str(), file.additions, file.deletions),
                (status, additions, deletions)
            );
        }
        assert!(!review.files.iter().any(|file| file.path == "old.txt"));
        std::fs::remove_file(cwd.join(renamed)).unwrap();
        let review = collect_workspace_review(cwd.to_path_buf()).unwrap();
        assert_eq!(
            review.files.len(),
            3,
            "changes are compared to the source tree independently of staging"
        );
        assert_eq!(
            review
                .files
                .iter()
                .find(|file| file.path == "old.txt")
                .unwrap()
                .status,
            "deleted"
        );
        assert_eq!((review.additions, review.deletions), (0, 5));
    }
}
