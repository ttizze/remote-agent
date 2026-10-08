//! The diff panel's `Uncommitted` and `Changes` previews, read against a copy
//! of the index that lists untracked files as intent-to-add.
use super::{
    DIFF_ARGS, METADATA_MAX_BYTES, Options, Run, TRUNCATED_MARKER, git, merge_base, repository,
    unborn_head,
};
use agent_protocol::workspace::{
    BranchChanges, DiffFile, DiffPreview, DiffPreviewResult, DiffSource, DiffSourceKind,
};
use anyhow::{Context as _, Result, anyhow};
use std::path::Path;

const PATCH_MAX_BYTES: usize = 120_000;
const FILE_PATCH_MAX_BYTES: usize = 1024 * 1024;

/// `--numstat -z` entries; renames carry both paths.
pub(super) fn parse_numstat(stdout: &str) -> Vec<DiffFile> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut files = vec![];
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        index += 1;
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let count = |value: &str| -> Option<u64> {
            if value == "-" {
                Some(0)
            } else {
                value.parse().ok()
            }
        };
        let (Some(additions), Some(deletions)) = (count(added), count(deleted)) else {
            continue;
        };
        let (path, previous_path) = if path.is_empty() {
            let previous = fields.get(index).copied().unwrap_or_default().to_owned();
            let current = fields
                .get(index + 1)
                .copied()
                .unwrap_or_default()
                .to_owned();
            index += 2;
            (current, Some(previous))
        } else {
            (path.to_owned(), None)
        };
        files.push(DiffFile {
            path,
            previous_path,
            additions,
            deletions,
        });
    }
    files
}

fn empty_tree(cwd: &Path) -> Result<String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    stdout(cwd, &["hash-object", "-t", "tree", null]).context("could not hash the empty tree")
}

/// A copy of the index with the untracked files added as intent-to-add, so a
/// diff against any commit shows them as new. `None` when the listing is too
/// large; `Some(None)` when nothing is untracked.
pub(super) struct ReviewIndex {
    _file: Option<tempfile::NamedTempFile>,
    pub(super) env: Option<Vec<(String, String)>>,
}

pub(super) fn review_index(
    cwd: &Path,
    paths: &[String],
    only: Option<&str>,
) -> Result<Option<ReviewIndex>> {
    let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
    args.extend(paths.iter().map(String::as_str));
    let listed = git(cwd, &args, Options::default())?;
    if listed.stdout.len() > METADATA_MAX_BYTES {
        return Ok(None);
    }
    if !listed.ok() {
        return Err(anyhow!("could not list untracked files"));
    }
    let staged_deletions = git(
        cwd,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=D",
            "-z",
            "HEAD",
            "--",
        ],
        Options::default(),
    )?
    .text();
    let deleted: Vec<&str> = staged_deletions
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect();
    let text = listed.text();
    let untracked: Vec<&str> = text
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| only.is_none_or(|only| *path == only))
        .filter(|path| !deleted.contains(path))
        .collect();
    if untracked.is_empty() {
        return Ok(Some(ReviewIndex {
            _file: None,
            env: None,
        }));
    }
    let index_path = stdout(cwd, &["rev-parse", "--git-path", "index"]).context("no index path")?;
    let index_path = cwd.join(index_path);
    let file = tempfile::NamedTempFile::new()?;
    let exists = index_path.is_file();
    if exists {
        std::fs::copy(&index_path, file.path())?;
        if let Ok(modified) = std::fs::metadata(&index_path).and_then(|m| m.modified()) {
            let seconds = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
            let _ = file.as_file().set_modified(time);
        }
    }
    let env = vec![(
        "GIT_INDEX_FILE".to_owned(),
        file.path().to_string_lossy().into_owned(),
    )];
    let config = [
        "-c",
        "core.splitIndex=false",
        "-c",
        "splitIndex.sharedIndexExpire=never",
    ];
    let run = |args: &[&str], stdin: Option<&[u8]>| -> Result<()> {
        let result = git(
            cwd,
            args,
            Options {
                env: Some(&env),
                stdin,
            },
        )?;
        if !result.ok() {
            return Err(anyhow!("Could not prepare the review index."));
        }
        Ok(())
    };
    if !exists {
        run(&["read-tree", "--empty"], None)?;
    }
    run(
        &[&config[..], &["update-index", "--no-split-index"]].concat(),
        None,
    )?;
    let mut list = untracked.join("\0").into_bytes();
    list.push(0);
    run(
        &[
            &config[..],
            &[
                "--literal-pathspecs",
                "add",
                "--intent-to-add",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
        ]
        .concat(),
        Some(&list),
    )?;
    Ok(Some(ReviewIndex {
        _file: Some(file),
        env: Some(env),
    }))
}

struct Tracked {
    diff: String,
    truncated: bool,
    files: Option<Vec<DiffFile>>,
}

fn truncate(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}{TRUNCATED_MARKER}", &text[..end]), true)
}

fn diff_command(
    diff_args: &[&str],
    extra: &[&str],
    reference: &str,
    paths: &[String],
) -> Vec<String> {
    diff_args
        .iter()
        .chain(extra)
        .map(|arg| (*arg).to_owned())
        .chain([reference.to_owned(), "--".to_owned()])
        .chain(paths.iter().cloned())
        .collect()
}

/// The files that changed from one commit to the working tree, and the commit
/// actually compared: the empty tree before the first commit.
pub(super) fn statistics(
    cwd: &Path,
    reference: &str,
    diff_args: &[&str],
    paths: &[String],
    env: Option<&[(String, String)]>,
) -> Result<(String, Vec<DiffFile>)> {
    let run = |reference: &str| {
        let args = diff_command(diff_args, &["--numstat", "-z"], reference, paths);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(cwd, &args, Options { env, stdin: None })
    };
    let mut reference = reference.to_owned();
    let mut stat = run(&reference)?;
    if !stat.ok() && reference == "HEAD" && unborn_head(&stat.stderr) {
        reference = empty_tree(cwd)?;
        stat = run(&reference)?;
    }
    if !stat.ok() || stat.stdout.len() > METADATA_MAX_BYTES {
        return Err(anyhow!("Could not read complete diff statistics."));
    }
    Ok((reference, parse_numstat(&stat.text())))
}

/// One commit against the working tree: statistics, then the patch when
/// anything changed.
fn tracked(
    cwd: &Path,
    reference: &str,
    diff_args: &[&str],
    paths: &[String],
    env: Option<&[(String, String)]>,
    limit: usize,
) -> Result<Tracked> {
    let (reference, files) = statistics(cwd, reference, diff_args, paths, env)?;
    if files.is_empty() {
        return Ok(Tracked {
            diff: String::new(),
            truncated: false,
            files: Some(files),
        });
    }
    let args = diff_command(diff_args, &["--patch"], &reference, paths);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let patch = git(cwd, &args, Options { env, stdin: None })?;
    if !patch.ok() {
        return Err(anyhow!("Could not read the diff."));
    }
    let (diff, truncated) = truncate(patch.text(), limit);
    Ok(Tracked {
        diff,
        truncated,
        files: Some(files),
    })
}

fn diff_hash(diff: &str, files: &[DiffFile]) -> String {
    use ring::digest::{SHA256, digest};
    let json = serde_json::to_string(&(diff, files)).unwrap_or_default();
    digest(&SHA256, json.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now() -> agent_domain::Timestamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    agent_domain::Timestamp::from_millis(millis).expect("the clock is within range")
}

pub(super) fn preview(request: &DiffPreview) -> Result<DiffPreviewResult> {
    let cwd = Path::new(&request.cwd);
    let empty = || DiffPreviewResult {
        cwd: request.cwd.clone(),
        generated_at: now(),
        sources: vec![],
    };
    if !cwd.is_dir() {
        return Ok(empty());
    }
    let Some(repository) = repository(cwd) else {
        return Ok(empty());
    };
    let root = repository.root.as_path();
    let paths: Vec<String> = request
        .file
        .iter()
        .flat_map(|file| std::iter::once(&file.path).chain(&file.previous_path))
        .map(|path| format!(":(top,literal){path}"))
        .collect();
    let limit = if request.file.is_some() {
        FILE_PATCH_MAX_BYTES
    } else {
        PATCH_MAX_BYTES
    };
    let source = request.file.as_ref().map(|file| file.source);
    let dirty_ref = (source != Some(DiffSourceKind::BranchRange)).then_some("HEAD");
    let review = if source == Some(DiffSourceKind::WorkingTree) {
        (request.base_ref.clone(), None)
    } else {
        let base = merge_base(
            root,
            repository.branch.as_deref(),
            request.base_ref.as_deref(),
        )?;
        (base.base_ref, Some(base.commit))
    };
    let mut diff_args: Vec<&str> = DIFF_ARGS.to_vec();
    if request.ignore_whitespace {
        diff_args.push("--ignore-all-space");
    }
    let index = review_index(
        root,
        &paths,
        request.file.as_ref().map(|file| file.path.as_str()),
    )?;
    let env = index.as_ref().and_then(|index| index.env.as_deref());
    let read = |reference: Option<&str>| -> Result<Tracked> {
        match reference {
            None => Ok(Tracked {
                diff: String::new(),
                truncated: false,
                files: Some(vec![]),
            }),
            Some(reference) => tracked(root, reference, &diff_args, &paths, env, limit),
        }
    };
    let dirty = read(dirty_ref)?;
    let base = if review.1.as_deref() == dirty_ref {
        Tracked {
            diff: dirty.diff.clone(),
            truncated: dirty.truncated,
            files: dirty.files.clone(),
        }
    } else {
        read(review.1.as_deref())?
    };
    // Too many untracked files to list: tracked changes only, totals incomplete.
    let incomplete = |reference: Option<&str>, result: Tracked| match (&index, reference) {
        (None, Some(_)) => Tracked {
            files: None,
            truncated: true,
            ..result
        },
        _ => result,
    };
    let dirty = incomplete(dirty_ref, dirty);
    let base = incomplete(review.1.as_deref(), base);
    let source = |id: &str, kind, title: String, base_ref, head_ref, tracked: Tracked| DiffSource {
        id: id.into(),
        kind,
        title,
        base_ref,
        head_ref,
        diff_hash: diff_hash(&tracked.diff, tracked.files.as_deref().unwrap_or_default()),
        diff: tracked.diff,
        truncated: tracked.truncated,
        files: tracked.files,
    };
    Ok(DiffPreviewResult {
        cwd: request.cwd.clone(),
        generated_at: now(),
        sources: vec![
            source(
                "working-tree",
                DiffSourceKind::WorkingTree,
                "Uncommitted".into(),
                Some("HEAD".into()),
                None,
                dirty,
            ),
            source(
                "branch-range",
                DiffSourceKind::BranchRange,
                review
                    .0
                    .as_ref()
                    .map_or_else(|| "Changes".into(), |base| format!("Changes vs {base}")),
                review.0.clone(),
                Some(repository.branch.clone().unwrap_or_else(|| "HEAD".into())),
                base,
            ),
        ],
    })
}

/// `Changes` totals with untracked files, like the `Changes` view.
pub(super) fn branch_changes(cwd: &Path, branch: Option<&str>) -> Result<BranchChanges> {
    let base = merge_base(cwd, branch, None)?;
    let index = review_index(cwd, &[], None)?
        .ok_or_else(|| anyhow!("Too many untracked files to count."))?;
    let (_, files) = statistics(cwd, &base.commit, &DIFF_ARGS, &[], index.env.as_deref())?;
    Ok(BranchChanges {
        base_ref: base.base_ref,
        insertions: files.iter().map(|file| file.additions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
    })
}

