use anyhow::{Context as _, Result, anyhow};
use std::{collections::HashMap, path::PathBuf, process::Command};

use agent_protocol::models::ChangedFile as WorkspaceFileChange;
pub use agent_protocol::models::WorkspaceReview;

pub async fn inspect_workspace(cwd: String) -> Result<WorkspaceReview> {
    tokio::task::spawn_blocking(move || collect_workspace_review(PathBuf::from(cwd)))
        .await
        .context("failed to inspect workspace")?
}

fn collect_workspace_review(cwd: PathBuf) -> Result<WorkspaceReview> {
    let cwd = cwd
        .canonicalize()
        .context("working directory is unavailable")?;
    if !cwd.is_dir() {
        return Err(anyhow!("working directory is not a directory"));
    }
    let membership = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .env("LC_ALL", "C")
        .current_dir(&cwd)
        .output()
        .context("failed to run git")?;
    // Git has no distinct exit code for discovery failure. Recognize only its
    // ordinary parent-directory/mount-boundary result in a fixed locale;
    // broken metadata, permissions and ownership errors must still surface.
    let no_repository = membership.status.code() == Some(128)
        && membership
            .stderr
            .starts_with(b"fatal: not a git repository (or any ");
    if !membership.status.success() && !no_repository {
        return Err(anyhow!(crate::git::failure(&membership)));
    }
    if no_repository || membership.stdout.trim_ascii() == b"false" {
        return Ok(WorkspaceReview {
            branch: String::new(),
            additions: 0,
            deletions: 0,
            files: Vec::new(),
            diff: String::new(),
        });
    }
    let branch = crate::git::text(&cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|_| crate::git::text(&cwd, &["rev-parse", "--short", "HEAD"]))?
        .trim()
        .to_owned();
    let status = crate::git::output(&cwd, &["status", "--porcelain=v1", "-z"])?.stdout;
    let mut files = parse_git_status(&status);
    let numstat = crate::git::output(&cwd, &["diff", "--numstat", "-z", "HEAD", "--"])
        .or_else(|_| crate::git::output(&cwd, &["diff", "--numstat", "-z", "--"]))?;
    let (mut additions, mut deletions) = {
        let mut counts: HashMap<_, _> = files
            .iter_mut()
            .map(|file| {
                (
                    file.path.as_bytes(),
                    (&mut file.additions, &mut file.deletions),
                )
            })
            .collect();
        parse_numstat(&numstat.stdout, |path, added, deleted| {
            if let Some((additions, deletions)) = counts.get_mut(path) {
                **additions = added;
                **deletions = deleted;
            }
        })
    };
    let mut diff =
        crate::git::text(&cwd, &["diff", "--no-ext-diff", "--no-color", "HEAD", "--"])
            .or_else(|_| crate::git::text(&cwd, &["diff", "--no-ext-diff", "--no-color", "--"]))?;
    for file in files.iter_mut().filter(|file| file.status == "untracked") {
        let output = Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--numstat",
                "-z",
                "--patch",
                "--no-ext-diff",
                "--no-color",
                "--",
                "/dev/null",
                &file.path,
            ])
            .current_dir(&cwd)
            .output()?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(anyhow!("cannot read untracked file diff"));
        }
        let output = String::from_utf8_lossy(&output.stdout);
        let patch_start = output.find("diff --git ").unwrap_or(output.len());
        let (added, deleted) =
            parse_numstat(output[..patch_start].as_bytes(), |_, added, deleted| {
                file.additions = added;
                file.deletions = deleted;
            });
        additions += added;
        deletions += deleted;
        diff.push_str(&output[patch_start..]);
        if diff.len() > 8 * 1024 * 1024 {
            return Err(anyhow!(
                "working-tree diff exceeds 8 MiB; select a smaller working directory"
            ));
        }
    }
    Ok(WorkspaceReview {
        branch,
        additions,
        deletions,
        files,
        diff,
    })
}

fn parse_git_status(output: &[u8]) -> Vec<WorkspaceFileChange> {
    let entries = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut index = 0;
    while index < entries.len() && files.len() < 200 {
        let entry = entries[index];
        if entry.len() < 4 {
            index += 1;
            continue;
        }
        let code = &entry[..2];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let status = if code == b"??" {
            "untracked"
        } else if code.contains(&b'D') {
            "deleted"
        } else if code.contains(&b'A') {
            "added"
        } else if code.contains(&b'R') || code.contains(&b'C') {
            index += 1;
            "renamed"
        } else {
            "modified"
        };
        files.push(WorkspaceFileChange {
            path,
            status: status.into(),
            additions: Some(0),
            deletions: Some(0),
        });
        index += 1;
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
            assert_eq!((review.additions, review.deletions), (0, 0));
            assert!(review.files.is_empty());
            assert!(review.diff.is_empty());
        }
    }

    #[test]
    fn unavailable_paths_and_broken_git_metadata_remain_errors() {
        let directory = tempfile::tempdir().unwrap();
        assert!(collect_workspace_review(directory.path().join("missing")).is_err());
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
        assert_eq!((review.additions, review.deletions), (2, 0));
        assert!(review.diff.starts_with("diff --git "));
        assert!(review.diff.contains("+first\n+second\n"));
        assert_eq!(review.files[0].status, "untracked");
        assert_eq!(
            (review.files[0].additions, review.files[0].deletions),
            (Some(2), Some(0))
        );
    }

    #[test]
    fn file_counts_follow_renames_deletions_and_binary_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        crate::git::text(cwd, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(cwd.join("old.txt"), "first\nsecond\nthird\n").unwrap();
        std::fs::write(cwd.join("gone\tfile\n.txt"), "one\ntwo\n").unwrap();
        std::fs::write(cwd.join("binary.dat"), b"before\0").unwrap();
        crate::git::text(cwd, &["add", "."]).unwrap();
        crate::git::text(
            cwd,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "initial",
            ],
        )
        .unwrap();
        crate::git::text(cwd, &["mv", "old.txt", "new\tname\n.txt"]).unwrap();
        std::fs::write(
            cwd.join("new\tname\n.txt"),
            "first\nsecond\nthird\nfourth\n",
        )
        .unwrap();
        std::fs::remove_file(cwd.join("gone\tfile\n.txt")).unwrap();
        std::fs::write(cwd.join("binary.dat"), b"after\0").unwrap();
        let review = collect_workspace_review(cwd.to_path_buf()).unwrap();
        assert_eq!((review.additions, review.deletions), (1, 2));
        assert_eq!(review.files.len(), 3);
        for (path, status, additions, deletions) in [
            ("new\tname\n.txt", "renamed", Some(1), Some(0)),
            ("gone\tfile\n.txt", "deleted", Some(0), Some(2)),
            ("binary.dat", "modified", None, None),
        ] {
            let file = review.files.iter().find(|file| file.path == path).unwrap();
            assert_eq!(
                (file.status.as_str(), file.additions, file.deletions),
                (status, additions, deletions)
            );
        }
        assert!(!review.files.iter().any(|file| file.path == "old.txt"));
    }
}
