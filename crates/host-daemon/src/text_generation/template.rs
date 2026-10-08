//! Pull request template lookup against a committed tree.
use crate::vcs::{Options, git as run_git};
use anyhow::{Result, anyhow};
use std::path::Path;

const FILE_CANDIDATES: [&str; 6] = [
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    "pull_request_template.md",
    "PULL_REQUEST_TEMPLATE.md",
    "docs/pull_request_template.md",
    "docs/PULL_REQUEST_TEMPLATE.md",
];
const DIRECTORY_CANDIDATES: [&str; 3] = [
    ".github/PULL_REQUEST_TEMPLATE",
    "PULL_REQUEST_TEMPLATE",
    "docs/PULL_REQUEST_TEMPLATE",
];
const MAX_TEMPLATE_BYTES: usize = 8_000;

fn entries(cwd: &Path, treeish: &str) -> Result<Vec<(String, String)>> {
    let mut args = vec![
        "ls-tree",
        "-r",
        "-z",
        "--full-tree",
        treeish,
        "--",
    ];
    args.extend(FILE_CANDIDATES);
    args.extend(DIRECTORY_CANDIDATES);
    let result = run_git(cwd, &args, Options::default())?;
    if !result.ok() {
        return Err(anyhow!("Git could not read the pull request template tree."));
    }
    Ok(result
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (header, path) = entry.split_once('\t')?;
            (header.split_whitespace().next() == Some("100644"))
                .then(|| ("100644".to_owned(), path.to_owned()))
        })
        .collect())
}

fn read(cwd: &Path, treeish: &str, path: &str) -> Result<String> {
    let spec = format!("{treeish}:{path}");
    let result = run_git(cwd, &["show", &spec], Options::default())?;
    if !result.ok() {
        return Err(anyhow!("Git could not read the pull request template."));
    }
    let bytes = result.stdout;
    let truncated = bytes.len() > MAX_TEMPLATE_BYTES;
    let end = bytes.len().min(MAX_TEMPLATE_BYTES);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        text.push_str("\n\n[truncated]");
    }
    Ok(text)
}

/// Picks the first named file, then a single Markdown file in a template
/// directory. Multiple directory templates are intentionally ambiguous.
pub(crate) fn pull_request_template(cwd: &Path, treeish: &str) -> Result<Option<String>> {
    let files = entries(cwd, treeish)?;
    for candidate in FILE_CANDIDATES {
        if files.iter().any(|(_, path)| path == candidate) {
            return read(cwd, treeish, candidate).map(Some);
        }
    }
    for directory in DIRECTORY_CANDIDATES {
        let matches: Vec<&str> = files
            .iter()
            .map(|(_, path)| path.as_str())
            .filter(|path| {
                path.strip_prefix(&format!("{directory}/"))
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".md"))
            })
            .collect();
        if matches.len() == 1 {
            return read(cwd, treeish, matches[0]).map(Some);
        }
        if matches.len() > 1 {
            return Ok(None);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn picks_named_template_before_directory_templates() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--quiet"]);
        git(cwd, &["config", "user.email", "test@example.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(cwd.join(".github/PULL_REQUEST_TEMPLATE")).unwrap();
        std::fs::write(cwd.join("pull_request_template.md"), "# named\n").unwrap();
        std::fs::write(cwd.join(".github/PULL_REQUEST_TEMPLATE/a.md"), "# a\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "templates", "--quiet"]);
        assert_eq!(
            pull_request_template(cwd, "HEAD").unwrap().as_deref(),
            Some("# named\n")
        );
    }

    #[test]
    fn multiple_directory_templates_are_ambiguous() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path();
        git(cwd, &["init", "--quiet"]);
        git(cwd, &["config", "user.email", "test@example.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(cwd.join("PULL_REQUEST_TEMPLATE")).unwrap();
        std::fs::write(cwd.join("PULL_REQUEST_TEMPLATE/a.md"), "a").unwrap();
        std::fs::write(cwd.join("PULL_REQUEST_TEMPLATE/b.md"), "b").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "templates", "--quiet"]);
        assert!(pull_request_template(cwd, "HEAD").unwrap().is_none());
    }
}
