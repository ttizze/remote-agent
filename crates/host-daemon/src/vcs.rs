//! What the diff panel reads from Git: the checkout's status, its branches for
//! the base picker, and the `Uncommitted` and `Changes` diffs. `Changes`
//! compares the working tree, untracked files included, with the merge base of
//! HEAD and the base branch.
use agent_protocol::workspace::{
    BranchChanges, DiffFile, DiffPreview, DiffPreviewResult, DiffSource, DiffSourceKind,
    FileChangeTotals, ListRefs, RefKind, RefList, SwitchRef, SwitchedRef, VcsRef, VcsStatus,
    WorkingTreeChanges,
};
use anyhow::{Context as _, Result, anyhow};
use std::{
    collections::{BTreeMap, HashMap},
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const PATCH_MAX_BYTES: usize = 120_000;
const FILE_PATCH_MAX_BYTES: usize = 1024 * 1024;
const METADATA_MAX_BYTES: usize = 16 * 1024 * 1024;
const TRUNCATED_MARKER: &str = "\n\n[truncated]";
const DEFAULT_LIST_LIMIT: u32 = 100;
const BASE_CANDIDATES: [&str; 2] = ["main", "master"];
/// Shared by previews and status totals, so the Changes row matches the view.
const DIFF_ARGS: [&str; 8] = [
    "diff",
    "--find-renames",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--minimal",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

struct Run {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}
impl Run {
    fn ok(&self) -> bool {
        self.code == 0
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

#[derive(Default)]
struct Options<'a> {
    env: Option<&'a [(String, String)]>,
    stdin: Option<&'a [u8]>,
}

fn git(cwd: &Path, args: &[&str], options: Options<'_>) -> Result<Run> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
        .stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in options.env.into_iter().flatten() {
        command.env(key, value);
    }
    let mut child = command.spawn().context("failed to run git")?;
    if let Some(input) = options.stdin {
        child
            .stdin
            .take()
            .context("git input unavailable")?
            .write_all(input)?;
    }
    let output = child.wait_with_output()?;
    Ok(Run {
        code: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    git(cwd, args, Options::default())
        .ok()
        .filter(Run::ok)
        .map(|run| run.text().trim().to_owned())
}

fn unborn_head(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    stderr.contains("bad revision 'head'")
        || (stderr.contains("unknown revision") && stderr.contains("path not in the working tree"))
}

/// The checkout root and its branch; `None` outside a Git checkout.
struct Repository {
    root: PathBuf,
    branch: Option<String>,
    common_dir: PathBuf,
}

fn repository(cwd: &Path) -> Option<Repository> {
    let common = stdout(cwd, &["rev-parse", "--git-common-dir"])?;
    let root = stdout(cwd, &["rev-parse", "--show-toplevel"])?;
    let branch = stdout(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let common = Path::new(&common);
    Some(Repository {
        root: PathBuf::from(root),
        branch,
        common_dir: if common.is_absolute() {
            common.to_owned()
        } else {
            cwd.join(common)
        },
    })
}

/// Remote names, longest first so nested names match before their prefixes.
fn remote_names(cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = stdout(cwd, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names
}

fn primary_remote(cwd: &Path) -> Option<String> {
    let names = stdout(cwd, &["remote"])?;
    let names: Vec<&str> = names
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    if names.contains(&"origin") {
        return Some("origin".into());
    }
    names.first().map(|name| (*name).to_owned())
}

fn default_branch(cwd: &Path, remote: &str) -> Option<String> {
    let reference = stdout(
        cwd,
        &["symbolic-ref", &format!("refs/remotes/{remote}/HEAD")],
    )?;
    reference
        .strip_prefix(&format!("refs/remotes/{remote}/"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn ref_exists(cwd: &Path, reference: &str) -> bool {
    git(
        cwd,
        &["show-ref", "--verify", "--quiet", reference],
        Options::default(),
    )
    .is_ok_and(|run| run.ok())
}

/// The branch `Changes` compares with: the configured merge base, the remote's
/// default branch, then `main` or `master`, preferring the remote copy. The
/// current branch compares with its own remote copy.
fn base_branch(cwd: &Path, branch: &str) -> Option<String> {
    let configured = stdout(
        cwd,
        &["config", "--get", &format!("branch.{branch}.gh-merge-base")],
    )
    .filter(|value| !value.is_empty());
    let remote = primary_remote(cwd);
    let default = remote
        .as_deref()
        .and_then(|remote| default_branch(cwd, remote));
    let candidates = configured
        .into_iter()
        .chain(default)
        .chain(BASE_CANDIDATES.map(str::to_owned));
    for candidate in candidates {
        let prefix = remote
            .as_deref()
            .filter(|remote| *remote != "origin")
            .map(|remote| format!("{remote}/"));
        let name = candidate
            .strip_prefix("origin/")
            .or_else(|| {
                prefix
                    .as_deref()
                    .and_then(|prefix| candidate.strip_prefix(prefix))
            })
            .unwrap_or(&candidate)
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let remote_copy = remote
            .as_deref()
            .filter(|remote| ref_exists(cwd, &format!("refs/remotes/{remote}/{name}")))
            .map(|remote| format!("{remote}/{name}"));
        if name == branch {
            if remote_copy.is_some() {
                return remote_copy;
            }
            continue;
        }
        if remote_copy.is_some() {
            return remote_copy;
        }
        if ref_exists(cwd, &format!("refs/heads/{name}")) {
            return Some(name);
        }
    }
    None
}

struct MergeBase {
    base_ref: Option<String>,
    commit: String,
}

/// Without a usable base, `Changes` is the diff against HEAD.
fn merge_base(cwd: &Path, branch: Option<&str>, explicit: Option<&str>) -> Result<MergeBase> {
    let base = explicit
        .map(str::to_owned)
        .or_else(|| branch.and_then(|branch| base_branch(cwd, branch)));
    let Some(base) = base else {
        return Ok(MergeBase {
            base_ref: None,
            commit: "HEAD".into(),
        });
    };
    let run = git(cwd, &["merge-base", &base, "HEAD"], Options::default())?;
    let commit = run.text().trim().to_owned();
    if run.ok() && !commit.is_empty() {
        return Ok(MergeBase {
            base_ref: Some(base),
            commit,
        });
    }
    let head = git(
        cwd,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        Options::default(),
    )?;
    if explicit.is_none() && !head.ok() {
        return Ok(MergeBase {
            base_ref: None,
            commit: "HEAD".into(),
        });
    }
    Err(anyhow!(
        "Could not find a common commit between '{base}' and HEAD."
    ))
}

/// `--numstat -z` entries; renames carry both paths.
fn parse_numstat(stdout: &str) -> Vec<DiffFile> {
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
struct ReviewIndex {
    _file: Option<tempfile::NamedTempFile>,
    env: Option<Vec<(String, String)>>,
}

fn review_index(cwd: &Path, paths: &[String], only: Option<&str>) -> Result<Option<ReviewIndex>> {
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
fn statistics(
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

fn preview(request: &DiffPreview) -> Result<DiffPreviewResult> {
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
fn branch_changes(cwd: &Path, branch: Option<&str>) -> Result<BranchChanges> {
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

fn status(cwd: &Path) -> Result<VcsStatus> {
    let not_repository = VcsStatus {
        is_repo: false,
        has_primary_remote: false,
        is_default_ref: false,
        ref_name: None,
        has_working_tree_changes: false,
        working_tree: WorkingTreeChanges {
            files: vec![],
            insertions: 0,
            deletions: 0,
        },
        branch_changes: None,
    };
    if !cwd.is_dir() {
        return Ok(not_repository);
    }
    if let Some(index) = stdout(cwd, &["rev-parse", "--git-path", "index"])
        && cwd.join(format!("{index}.lock")).exists()
    {
        return Err(anyhow!(
            "Git index is locked. Status will resume when the index lock is removed."
        ));
    }
    let porcelain = git(
        cwd,
        &["status", "--porcelain=2", "--branch", "--no-ahead-behind"],
        Options::default(),
    )?;
    if !porcelain.ok() {
        if porcelain
            .stderr
            .to_lowercase()
            .contains("not a git repository")
        {
            return Ok(not_repository);
        }
        return Err(anyhow!("Git status failed."));
    }
    let mut ref_name = None;
    let mut changed = vec![];
    let text = porcelain.text();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            let head = head.trim();
            ref_name = (!head.starts_with('(')).then(|| head.to_owned());
        } else if !line.trim().is_empty() && !line.starts_with('#') {
            changed.push(porcelain_path(line));
        }
    }
    let numstat = git(
        cwd,
        &["diff", "HEAD", "--numstat", "--"],
        Options::default(),
    )?;
    let numstat = if numstat.ok() {
        numstat.text()
    } else if unborn_head(&numstat.stderr) {
        let staged = stdout(cwd, &["diff", "--cached", "--numstat"]).unwrap_or_default();
        let unstaged = stdout(cwd, &["diff", "--numstat"]).unwrap_or_default();
        format!("{staged}\n{unstaged}")
    } else {
        return Err(anyhow!("git diff HEAD --numstat failed."));
    };
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A rename counts toward its new path.
        let path = path.trim();
        let path = match path.split_once(" => ") {
            Some((_, renamed)) if !renamed.trim().is_empty() => renamed.trim(),
            _ => path,
        };
        let entry = totals.entry(path.to_owned()).or_default();
        entry.0 += added.parse::<u64>().unwrap_or(0);
        entry.1 += deleted.parse::<u64>().unwrap_or(0);
    }
    for path in changed.into_iter().flatten() {
        totals.entry(path).or_default();
    }
    let files: Vec<FileChangeTotals> = totals
        .into_iter()
        .map(|(path, (insertions, deletions))| FileChangeTotals {
            path,
            insertions,
            deletions,
        })
        .collect();
    let default = default_branch(cwd, "origin");
    let is_default_ref = ref_name.as_deref().is_some_and(|name| {
        Some(name) == default.as_deref() || (default.is_none() && BASE_CANDIDATES.contains(&name))
    });
    let root = repository(cwd).map(|repository| repository.root);
    Ok(VcsStatus {
        is_repo: true,
        has_primary_remote: stdout(cwd, &["remote", "get-url", "origin"]).is_some(),
        is_default_ref,
        has_working_tree_changes: text
            .lines()
            .any(|line| !line.starts_with('#') && !line.trim().is_empty()),
        working_tree: WorkingTreeChanges {
            insertions: files.iter().map(|file| file.insertions).sum(),
            deletions: files.iter().map(|file| file.deletions).sum(),
            files,
        },
        branch_changes: branch_changes(root.as_deref().unwrap_or(cwd), ref_name.as_deref()).ok(),
        ref_name,
    })
}

/// The path of one `--porcelain=2` entry.
fn porcelain_path(line: &str) -> Option<String> {
    if let Some(path) = line.strip_prefix("? ").or_else(|| line.strip_prefix("! ")) {
        return Some(path.trim().to_owned());
    }
    let fields: usize = match line.chars().next()? {
        '1' => 8,
        '2' => 9,
        'u' => 10,
        _ => return None,
    };
    let path = line.splitn(fields + 1, ' ').nth(fields)?;
    Some(path.split('\t').next().unwrap_or(path).trim().to_owned())
}

/// The base picker's branches; current and default lead, then newest commit.
fn list_refs(request: &ListRefs) -> Result<RefList> {
    let cwd = Path::new(&request.cwd);
    let not_repository = RefList {
        refs: vec![],
        is_repo: false,
        has_primary_remote: false,
        next_cursor: None,
        total_count: 0,
    };
    if !cwd.is_dir() {
        return Ok(not_repository);
    }
    let Some(repository) = repository(cwd) else {
        return Ok(not_repository);
    };
    let git_dir = repository.common_dir.to_string_lossy().into_owned();
    let with_dir = |args: &[&str]| -> Vec<String> {
        ["--git-dir", git_dir.as_str()]
            .iter()
            .chain(args)
            .map(|arg| (*arg).to_owned())
            .collect()
    };
    let run = |args: Vec<String>| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(cwd, &args, Options::default())
    };
    let refs = run(with_dir(&[
        "for-each-ref",
        "--format=%(refname)%09%(committerdate:unix)%09%(symref)",
        "refs/heads",
        "refs/remotes",
    ]))?;
    if !refs.ok() {
        return Err(anyhow!("Git ref snapshot enumeration failed."));
    }
    let default = run(with_dir(&["symbolic-ref", "refs/remotes/origin/HEAD"]))
        .ok()
        .filter(Run::ok)
        .map(|run| {
            let text = run.text();
            text.trim()
                .strip_prefix("refs/remotes/origin/")
                .unwrap_or(text.trim())
                .to_owned()
        });
    let mut worktrees: HashMap<String, String> = HashMap::new();
    if let Ok(list) = run(with_dir(&["worktree", "list", "--porcelain", "-z"]))
        && list.ok()
    {
        let (mut path, mut branch, mut prunable) = (None, None, false);
        for field in list.text().split('\0') {
            if field.is_empty() {
                if let (Some(path), Some(branch), false) = (path.take(), branch.take(), prunable)
                    && Path::new(&path).exists()
                {
                    worktrees.insert(branch, path);
                }
                prunable = false;
            } else if let Some(value) = field.strip_prefix("worktree ") {
                path = Some(value.to_owned());
            } else if let Some(value) = field.strip_prefix("branch refs/heads/") {
                branch = Some(value.to_owned());
            } else if field == "prunable" || field.starts_with("prunable ") {
                prunable = true;
            }
        }
    }
    let remotes = remote_names(cwd);
    let mut local: Vec<(i64, VcsRef)> = vec![];
    let mut remote: Vec<(i64, VcsRef)> = vec![];
    for line in refs.text().lines() {
        let mut fields = line.split('\t');
        let (Some(full), time, symbolic) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if full.is_empty() || symbolic.is_some_and(|target| !target.is_empty()) {
            continue;
        }
        let time = time.and_then(|time| time.parse().ok()).unwrap_or(0);
        if let Some(name) = full.strip_prefix("refs/heads/") {
            local.push((
                time,
                VcsRef {
                    name: name.into(),
                    is_remote: false,
                    remote_name: None,
                    current: false,
                    is_default: Some(name) == default.as_deref(),
                    worktree_path: worktrees.get(name).cloned(),
                },
            ));
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            let parsed = remotes.iter().find_map(|remote| {
                let branch = name.strip_prefix(&format!("{remote}/"))?.trim();
                (!branch.is_empty()).then(|| (remote.clone(), branch.to_owned()))
            });
            remote.push((
                time,
                VcsRef {
                    name: name.into(),
                    is_remote: true,
                    is_default: parsed.as_ref().is_some_and(|(remote, branch)| {
                        remote == "origin" && Some(branch.as_str()) == default.as_deref()
                    }),
                    remote_name: parsed.map(|(remote, _)| remote),
                    current: false,
                    worktree_path: None,
                },
            ));
        }
    }
    let recent =
        |a: &(i64, VcsRef), b: &(i64, VcsRef)| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name));
    local.sort_by(recent);
    remote.sort_by(recent);
    let root = repository.root.to_string_lossy().into_owned();
    let in_worktree = local
        .iter()
        .any(|(_, entry)| entry.worktree_path.as_deref() == Some(root.as_str()));
    let mut local: Vec<VcsRef> = local.into_iter().map(|(_, entry)| entry).collect();
    for entry in &mut local {
        entry.current = if in_worktree {
            entry.worktree_path.as_deref() == Some(root.as_str())
        } else {
            Some(entry.name.as_str()) == repository.branch.as_deref()
        };
    }
    let local_names: Vec<String> = local.iter().map(|entry| entry.name.clone()).collect();
    let mut all: Vec<VcsRef> = local;
    all.extend(remote.into_iter().map(|(_, entry)| entry).filter(|entry| {
        if request.include_matching_remote_refs || entry.remote_name.as_deref() != Some("origin") {
            return true;
        }
        let first_slash = entry
            .name
            .find('/')
            .filter(|index| *index > 0 && *index < entry.name.len() - 1)
            .map_or(entry.name.as_str(), |index| &entry.name[index + 1..]);
        let without_remote = entry
            .name
            .strip_prefix("origin/")
            .filter(|rest| !rest.is_empty());
        ![Some(first_slash), without_remote]
            .into_iter()
            .flatten()
            .any(|candidate| local_names.iter().any(|name| name == candidate))
    }));
    all.sort_by_key(|entry| {
        if entry.current {
            0
        } else if entry.is_default {
            1
        } else {
            2
        }
    });
    let query = request
        .query
        .as_deref()
        .map(str::to_lowercase)
        .filter(|q| !q.is_empty());
    let filtered: Vec<VcsRef> = all
        .into_iter()
        .filter(|entry| match request.ref_kind {
            RefKind::All => true,
            RefKind::Local => !entry.is_remote,
            RefKind::Remote => entry.is_remote,
        })
        .filter(|entry| {
            query
                .as_deref()
                .is_none_or(|query| entry.name.to_lowercase().contains(query))
        })
        .collect();
    let cursor = request.cursor.unwrap_or(0) as usize;
    let limit = request.limit.unwrap_or(DEFAULT_LIST_LIMIT) as usize;
    let total = filtered.len();
    let page: Vec<VcsRef> = filtered.into_iter().skip(cursor).take(limit).collect();
    let next = (cursor + page.len() < total).then(|| (cursor + page.len()) as u32);
    Ok(RefList {
        refs: page,
        is_repo: true,
        has_primary_remote: remotes.iter().any(|name| name == "origin"),
        next_cursor: next,
        total_count: total as u32,
    })
}

pub(crate) async fn diff_preview(request: DiffPreview) -> Result<DiffPreviewResult> {
    tokio::task::spawn_blocking(move || preview(&request)).await?
}

pub(crate) async fn read_status(cwd: String) -> Result<VcsStatus> {
    tokio::task::spawn_blocking(move || status(Path::new(&cwd))).await?
}

pub(crate) async fn refs(request: ListRefs) -> Result<RefList> {
    if request
        .limit
        .is_some_and(|limit| !(1..=200).contains(&limit))
        || request
            .query
            .as_deref()
            .is_some_and(|query| query.chars().count() > 256)
    {
        return Err(anyhow!("invalid ref list request"));
    }
    tokio::task::spawn_blocking(move || list_refs(&request)).await?
}

/// The local branch that tracks `upstream`.
fn tracking_branch(cwd: &Path, upstream: &str) -> Option<String> {
    stdout(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)",
            "refs/heads",
        ],
    )?
    .lines()
    .filter_map(|line| line.trim().split_once('\t'))
    .map(|(branch, upstream)| (branch.trim(), upstream.trim()))
    .find(|(branch, candidate)| !branch.is_empty() && *candidate == upstream)
    .map(|(branch, _)| branch.to_owned())
}

/// Checks out a branch: an existing local one, the local branch tracking a
/// remote one, or a new local branch tracking it.
fn checkout(request: &SwitchRef) -> Result<SwitchedRef> {
    let cwd = Path::new(&request.cwd);
    let name = request.ref_name.as_str();
    let local = ref_exists(cwd, &format!("refs/heads/{name}"));
    let remote = ref_exists(cwd, &format!("refs/remotes/{name}"));
    let tracking = remote.then(|| tracking_branch(cwd, name)).flatten();
    let tracked_name = name
        .split_once('/')
        .map(|(_, branch)| branch.trim())
        .filter(|branch| !branch.is_empty());
    let tracked_exists = remote
        && tracked_name.is_some_and(|branch| ref_exists(cwd, &format!("refs/heads/{branch}")));
    let mut args = match (local, remote, &tracking) {
        (true, _, _) => vec!["checkout", name],
        (false, true, None) if tracked_exists => vec!["checkout", name],
        (false, true, None) => vec!["checkout", "--track", name],
        (false, true, Some(branch)) => vec!["checkout", branch.as_str()],
        (false, false, _) => vec!["checkout", name],
    };
    // A stale ref must not turn into a path checkout that discards local edits.
    args.push("--");
    let run = git(cwd, &args, Options::default())?;
    if !run.ok() {
        return Err(anyhow!(
            "Git command failed in GitVcsDriver.switchRef.checkout ({}): git checkout failed",
            request.cwd
        ));
    }
    Ok(SwitchedRef {
        ref_name: stdout(cwd, &["branch", "--show-current"]).filter(|name| !name.is_empty()),
    })
}

pub(crate) async fn switch_ref(request: SwitchRef) -> Result<SwitchedRef> {
    if request.ref_name.trim().is_empty() || request.ref_name.starts_with('-') {
        return Err(anyhow!("invalid ref name"));
    }
    tokio::task::spawn_blocking(move || checkout(&request)).await?
}

#[cfg(test)]
mod tests;
