use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceReview {
    branch: String,
    additions: u64,
    deletions: u64,
    files: Vec<WorkspaceFileChange>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileChange {
    path: String,
    status: &'static str,
}

pub(crate) async fn inspect(cwd: String) -> Result<WorkspaceReview, String> {
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
    run_git(&cwd, &["rev-parse", "--is-inside-work-tree"])?;
    let branch = run_git(&cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|_| run_git(&cwd, &["rev-parse", "--short", "HEAD"]))?
        .trim()
        .to_owned();
    let status = run_git_output(&cwd, &["status", "--porcelain=v1", "-z"])?.stdout;
    let files = parse_git_status(&status);
    let numstat = run_git(&cwd, &["diff", "--numstat", "HEAD", "--"])
        .or_else(|_| run_git(&cwd, &["diff", "--numstat", "--"]))?;
    let (additions, deletions) = parse_numstat(&numstat);
    Ok(WorkspaceReview {
        branch,
        additions,
        deletions,
        files,
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
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if message.is_empty() {
        "git command failed".to_owned()
    } else {
        message
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
        files.push(WorkspaceFileChange { path, status });
        index += 1;
    }
    files
}

fn parse_numstat(output: &str) -> (u64, u64) {
    output.lines().fold((0, 0), |(added, deleted), line| {
        let mut fields = line.split('\t');
        let line_added = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let line_deleted = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (added + line_added, deleted + line_deleted)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_entries_and_skips_rename_sources() {
        let status = b" M src/main.rs\0?? notes.txt\0R  new.rs\0old.rs\0D  gone.rs\0";

        assert_eq!(
            parse_git_status(status),
            vec![
                WorkspaceFileChange {
                    path: "src/main.rs".to_owned(),
                    status: "modified",
                },
                WorkspaceFileChange {
                    path: "notes.txt".to_owned(),
                    status: "untracked",
                },
                WorkspaceFileChange {
                    path: "new.rs".to_owned(),
                    status: "renamed",
                },
                WorkspaceFileChange {
                    path: "gone.rs".to_owned(),
                    status: "deleted",
                },
            ]
        );
    }

    #[test]
    fn sums_text_changes_and_ignores_binary_counts() {
        assert_eq!(
            parse_numstat("10\t2\ta.rs\n-\t-\timage.png\n3\t0\tb.rs\n"),
            (13, 2)
        );
    }
}
