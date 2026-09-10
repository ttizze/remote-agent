use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReview {
    branch: String,
    additions: u64,
    deletions: u64,
    files: Vec<WorkspaceFileChange>,
    diff: String,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileChange {
    path: String,
    status: &'static str,
    additions: Option<u64>,
    deletions: Option<u64>,
}

pub async fn inspect_workspace(cwd: String) -> Result<WorkspaceReview, String> {
    tokio::task::spawn_blocking(move || collect_workspace_review(PathBuf::from(cwd)))
        .await
        .map_err(|error| format!("failed to inspect workspace: {error}"))?
}

fn collect_workspace_review(cwd: PathBuf) -> Result<WorkspaceReview, String> {
    let cwd = cwd
        .canonicalize()
        .map_err(|error| format!("working directory is unavailable: {error}"))?;
    if !cwd.is_dir() {
        return Err("working directory is not a directory".to_owned());
    }
    let membership = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .env("LC_ALL", "C")
        .current_dir(&cwd)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    // Git has no distinct exit code for discovery failure. Recognize only its
    // ordinary parent-directory/mount-boundary result in a fixed locale;
    // broken metadata, permissions and ownership errors must still surface.
    let no_repository = membership.status.code() == Some(128)
        && membership
            .stderr
            .starts_with(b"fatal: not a git repository (or any ");
    if !membership.status.success() && !no_repository {
        return Err(git_failure(&membership));
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
    let branch = run_git(&cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|_| run_git(&cwd, &["rev-parse", "--short", "HEAD"]))?
        .trim()
        .to_owned();
    let status = run_git_output(&cwd, &["status", "--porcelain=v1", "-z"])?.stdout;
    let mut files = parse_git_status(&status);
    let numstat = run_git_output(&cwd, &["diff", "--numstat", "-z", "HEAD", "--"])
        .or_else(|_| run_git_output(&cwd, &["diff", "--numstat", "-z", "--"]))?;
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
    let mut diff = run_git(&cwd, &["diff", "--no-ext-diff", "--no-color", "HEAD", "--"])
        .or_else(|_| run_git(&cwd, &["diff", "--no-ext-diff", "--no-color", "--"]))?;
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
            .output()
            .map_err(|error| error.to_string())?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err("cannot read untracked file diff".into());
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
            return Err(
                "working-tree diff exceeds 8 MiB; select a smaller working directory".into(),
            );
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

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_output(cwd, args)?;
    String::from_utf8(output.stdout).map_err(|_| "git returned non-UTF-8 output".to_owned())
}

fn run_git_output(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    Err(git_failure(&output))
}

fn git_failure(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        "git command failed".to_owned()
    } else {
        message
    }
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
            status,
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
                run_git(
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
        run_git(cwd, &["init", "--initial-branch=main"]).unwrap();
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
    fn parses_status_entries_and_skips_rename_sources() {
        let status = b" M src/main.rs\0?? notes.txt\0R  new.rs\0old.rs\0D  gone.rs\0";

        assert_eq!(
            parse_git_status(status),
            vec![
                WorkspaceFileChange {
                    path: "src/main.rs".to_owned(),
                    status: "modified",
                    additions: Some(0),
                    deletions: Some(0),
                },
                WorkspaceFileChange {
                    path: "notes.txt".to_owned(),
                    status: "untracked",
                    additions: Some(0),
                    deletions: Some(0),
                },
                WorkspaceFileChange {
                    path: "new.rs".to_owned(),
                    status: "renamed",
                    additions: Some(0),
                    deletions: Some(0),
                },
                WorkspaceFileChange {
                    path: "gone.rs".to_owned(),
                    status: "deleted",
                    additions: Some(0),
                    deletions: Some(0),
                },
            ]
        );
    }

    #[test]
    fn sums_text_changes_and_ignores_binary_counts() {
        assert_eq!(
            parse_numstat(
                b"10\t2\ta.rs\0-\t-\timage.png\x003\t0\tb.rs\0",
                |_, _, _| {}
            ),
            (13, 2)
        );
    }

    #[test]
    fn file_counts_follow_renames_deletions_and_binary_changes() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        run_git(cwd, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(cwd.join("old.txt"), "first\nsecond\nthird\n").unwrap();
        std::fs::write(cwd.join("gone\tfile\n.txt"), "one\ntwo\n").unwrap();
        std::fs::write(cwd.join("binary.dat"), b"before\0").unwrap();
        run_git(cwd, &["add", "."]).unwrap();
        run_git(
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
        run_git(cwd, &["mv", "old.txt", "new\tname\n.txt"]).unwrap();
        std::fs::write(
            cwd.join("new\tname\n.txt"),
            "first\nsecond\nthird\nfourth\n",
        )
        .unwrap();
        std::fs::remove_file(cwd.join("gone\tfile\n.txt")).unwrap();
        std::fs::write(cwd.join("binary.dat"), b"after\0").unwrap();
        let review = collect_workspace_review(cwd.to_path_buf()).unwrap();
        assert_eq!((review.additions, review.deletions), (1, 2));
        let counts = |path: &str| {
            let file = review.files.iter().find(|file| file.path == path).unwrap();
            (file.additions, file.deletions)
        };
        assert_eq!(counts("new\tname\n.txt"), (Some(1), Some(0)));
        assert_eq!(counts("gone\tfile\n.txt"), (Some(0), Some(2)));
        assert_eq!(counts("binary.dat"), (None, None));
        let json = serde_json::to_value(&review).unwrap();
        let renamed = json["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "new\tname\n.txt")
            .unwrap();
        assert_eq!(renamed["additions"], 1);
        assert_eq!(renamed["deletions"], 0);
    }
}
