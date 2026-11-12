use anyhow::{Result, anyhow};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(super) struct Comparison {
    pub root: PathBuf,
    pub branch: String,
    pub base: String,
    pub base_ref: Option<String>,
    index: tempfile::TempPath,
}

pub(crate) fn base_key(branch: &str) -> String {
    format!("branch.{branch}.bex-base-ref")
}

/// A symbolic source follows subsequent integration into that branch. Detached
/// sources keep their exact starting commit instead.
pub(crate) fn source_ref(cwd: &Path, source: &str) -> Result<String> {
    let reference = crate::git::text(cwd, &["rev-parse", "--symbolic-full-name", source])?;
    if reference.trim().starts_with("refs/") {
        return Ok(reference.trim().into());
    }
    Ok(crate::git::text(cwd, &["rev-parse", "--verify", source])?
        .trim()
        .into())
}

impl Comparison {
    pub fn open(cwd: &Path) -> Result<Option<Self>> {
        match std::fs::metadata(cwd) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_dir() => {
                return Err(anyhow!(
                    "作業フォルダに指定したパスはフォルダではありません。"
                ));
            }
            Ok(_) => {}
        }
        let membership = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .env("LC_ALL", "C")
            .current_dir(cwd)
            .output()?;
        // Discovery failure is the only Git error that means an ordinary folder.
        let no_repository = membership.status.code() == Some(128)
            && membership
                .stderr
                .starts_with(b"fatal: not a git repository (or any ");
        if !membership.status.success() && !no_repository {
            return Err(anyhow!(crate::git::failure(&membership)));
        }
        if no_repository || membership.stdout.trim_ascii() == b"false" {
            return Ok(None);
        }
        let root = crate::git::text(cwd, &["rev-parse", "--show-toplevel"])?;
        let root = dunce::canonicalize(root.trim_end())?;
        let branch = crate::git::text(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|branch| branch.trim().to_owned());
        let (base, base_ref) = comparison_base(&root, branch.as_deref())?;
        let branch = match branch {
            Some(branch) => branch,
            None => crate::git::text(&root, &["rev-parse", "--short", "HEAD"])?
                .trim()
                .to_owned(),
        };
        let index = working_index(&root)?;
        Ok(Some(Self {
            root,
            branch,
            base,
            base_ref,
            index,
        }))
    }

    pub fn diff(&self, format: &str) -> Result<Vec<u8>> {
        // Compare the working tree itself: a dirty submodule cannot be
        // represented by writing its parent repository's index to a tree.
        index_output(
            &self.root,
            &self.index,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-relative",
                "--find-renames",
                format,
                "-z",
                &self.base,
                "--",
            ],
        )
    }
}

fn comparison_base(cwd: &Path, branch: Option<&str>) -> Result<(String, Option<String>)> {
    let Ok(head) = crate::git::text(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"]) else {
        return Ok((
            crate::git::text(cwd, &["hash-object", "-t", "tree", "--stdin"])?
                .trim()
                .into(),
            None,
        ));
    };
    let head = head.trim().to_owned();
    let Some(branch) = branch else {
        return Ok((head, Some("HEAD".into())));
    };
    let configured = crate::git::text(cwd, &["config", "--get", &base_key(branch)])
        .ok()
        .map(|base| base.trim().to_owned())
        .filter(|base| has_commit(cwd, base));
    // Like T3's resolveBaseBranchForNoUpstream, an absent/deleted source uses
    // the default branch, then main/master. A creation commit would leave
    // already-integrated work visible forever on branches created from HEAD.
    let base = match configured {
        Some(base) => base,
        None => default_base(cwd, branch)?.unwrap_or_else(|| "HEAD".into()),
    };
    let base_ref = base
        .strip_prefix("refs/heads/")
        .or_else(|| base.strip_prefix("refs/remotes/"))
        .unwrap_or(&base)
        .to_owned();
    Ok((
        crate::git::text(cwd, &["merge-base", "--end-of-options", &base, &head])?
            .trim()
            .into(),
        Some(base_ref),
    ))
}

fn default_base(cwd: &Path, branch: &str) -> Result<Option<String>> {
    let remotes = crate::git::text(cwd, &["remote"])?;
    let remote = remotes
        .lines()
        .find(|name| *name == "origin")
        .or_else(|| remotes.lines().next());
    let default = remote.and_then(|remote| {
        crate::git::text(
            cwd,
            &["symbolic-ref", &format!("refs/remotes/{remote}/HEAD")],
        )
        .ok()?
        .trim()
        .strip_prefix(&format!("refs/remotes/{remote}/"))
        .map(str::to_owned)
    });
    for candidate in default.as_deref().into_iter().chain(["main", "master"]) {
        if candidate == branch {
            let upstream = crate::git::text(
                cwd,
                &[
                    "rev-parse",
                    "--symbolic-full-name",
                    &format!("{branch}@{{upstream}}"),
                ],
            )
            .ok();
            if let Some(upstream) = upstream.as_deref().map(str::trim).filter(|upstream| {
                upstream.starts_with("refs/remotes/") && has_commit(cwd, upstream)
            }) {
                return Ok(Some(upstream.into()));
            }
        }
        if let Some(base) = remote
            .map(|remote| format!("refs/remotes/{remote}/{candidate}"))
            .into_iter()
            .chain((candidate != branch).then(|| format!("refs/heads/{candidate}")))
            .find(|base| has_commit(cwd, base))
        {
            return Ok(Some(base));
        }
    }
    Ok(None)
}

fn has_commit(cwd: &Path, reference: &str) -> bool {
    crate::git::text(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .is_ok()
}

fn working_index(cwd: &Path) -> Result<tempfile::TempPath> {
    let git_dir = crate::git::text(cwd, &["rev-parse", "--absolute-git-dir"])?;
    let mut index = tempfile::Builder::new()
        .prefix("bex-review-index-")
        .tempfile_in(git_dir.trim_end())?;
    let original = crate::git::text(cwd, &["rev-parse", "--git-path", "index"])?;
    let original = cwd.join(original.trim_end());
    let index = match std::fs::File::open(original) {
        Ok(mut source) => {
            // Git uses the index timestamp to detect same-stat edits. A newer
            // copy would make a racy entry appear clean and hide changed bytes.
            let modified = source.metadata()?.modified()?;
            std::io::copy(&mut source, index.as_file_mut())?;
            index.as_file().set_modified(modified)?;
            index.into_temp_path()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let index = index.into_temp_path();
            index_output(cwd, &index, &["read-tree", "--empty"])?;
            index
        }
        Err(error) => return Err(error.into()),
    };
    // A private index includes untracked files and their content in the same
    // comparison as committed/staged edits, without changing the user's staging.
    index_output(cwd, &index, &["add", "-A", "--", "."])?;
    Ok(index)
}

fn index_output(cwd: &Path, index: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(["--no-optional-locks", "-c", "core.splitIndex=false"])
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(crate::git::failure(&output)));
    }
    Ok(output.stdout)
}
