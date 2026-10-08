//! Bound disposable Cargo profiles while holding their original lock inodes.
use crate::{Result, supervision};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

#[cfg(test)]
#[path = "build_cleanup_tests.rs"]
mod integration_tests;

const TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
const LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];
const MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_AGE: f64 = 3.0 * 86400.0;

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Action {
    Keep,
    InUse,
    Locked,
    WouldClean,
    Cleaned,
}

#[derive(Serialize, Debug)]
struct Entry {
    path: PathBuf,
    bytes: Option<u64>,
    modified: f64,
    owners: BTreeSet<PathBuf>,
    action: Action,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Report {
    dry_run: bool,
    max_idle_bytes: u64,
    max_age_days: f64,
    entries: Vec<Entry>,
    before_idle_bytes: u64,
    after_idle_bytes: u64,
    reclaimed_bytes: u64,
    would_reclaim_bytes: u64,
}

async fn command(args: &[OsString], cwd: &Path, cancel: &watch::Receiver<bool>) -> Result<Vec<u8>> {
    Ok(supervision::run(
        args,
        cwd,
        supervision::Io::Capture,
        cancel,
        Duration::from_secs(60),
    )
    .await?
    .stdout)
}

fn cache_root(path: &Path) -> Result<bool> {
    let tag = path.join("CACHEDIR.TAG");
    if matches!(
        path.extension().and_then(OsStr::to_str),
        Some("app" | "xcresult" | "xcarchive")
    ) || !directory(path)
        || !tag.is_file()
        || tag.is_symlink()
    {
        return Ok(false);
    }
    Ok(fs::read(tag)?.starts_with(TAG))
}

fn directory(path: &Path) -> bool {
    path.is_dir() && !path.is_symlink()
}

async fn profiles(
    worktrees: &[PathBuf],
    cancel: &watch::Receiver<bool>,
) -> Result<BTreeMap<PathBuf, BTreeSet<PathBuf>>> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let targets = command(
        &[rustc, "--print".into(), "target-list".into()],
        Path::new("/"),
        cancel,
    )
    .await?;
    let targets: BTreeSet<_> = String::from_utf8(targets)?
        .lines()
        .map(OsString::from)
        .collect();
    let mut found = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
    for owner in worktrees {
        let target = owner.join("target");
        if !target.is_dir() {
            continue;
        }
        // Resolve only the worktree's target link. Nested links remain opaque.
        let target = target.canonicalize()?;
        let mut roots = Vec::new();
        if cache_root(&target)? {
            roots.push(target.clone());
        }
        for child in fs::read_dir(&target)? {
            let path = child?.path();
            if cache_root(&path)? {
                roots.push(path);
            }
        }
        for root in roots {
            let mut parents = vec![root.clone()];
            for child in fs::read_dir(&root)? {
                let child = child?;
                if targets.contains(&child.file_name()) && directory(&child.path()) {
                    parents.push(child.path());
                }
            }
            for parent in parents {
                for name in ["debug", "release"] {
                    let path = parent.join(name);
                    if directory(&path)
                        && directory(&path.join(".fingerprint"))
                        && LOCKS.iter().any(|lock| path.join(lock).is_file())
                    {
                        found
                            .entry(path.canonicalize()?)
                            .or_default()
                            .insert(owner.clone());
                    }
                }
            }
        }
    }
    Ok(found)
}

fn usage(path: &Path) -> Result<(u64, f64)> {
    let mut size = 0;
    let mut modified: f64 = 0.0;
    let mut seen = BTreeSet::new();
    let mut pending = vec![path.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if LOCKS.iter().any(|name| entry.file_name() == *name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if !seen.insert((metadata.dev(), metadata.ino())) {
                continue;
            }
            size += metadata.blocks() * 512;
            modified = modified
                .max(metadata.mtime() as f64 + metadata.mtime_nsec() as f64 / 1_000_000_000.0);
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok((size, modified))
}

fn parse_open_paths(
    ps: &[u8],
    lsof: &[u8],
    worktrees: &[PathBuf],
    own_pid: u32,
) -> Result<Vec<(PathBuf, Option<PathBuf>)>> {
    let parents: BTreeMap<u32, u32> = String::from_utf8_lossy(ps)
        .lines()
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<Vec<u32>, _>>()?;
            if values.len() != 2 {
                return Err("invalid process inspection".into());
            }
            Ok((values[0], values[1]))
        })
        .collect::<Result<_>>()?;
    let mut ancestors = BTreeSet::new();
    let mut pid = own_pid;
    while pid > 1 && ancestors.insert(pid) {
        pid = *parents.get(&pid).unwrap_or(&0);
    }
    let mut paths = BTreeMap::new();
    let mut pid = 0;
    let mut cwd = false;
    for field in lsof.split(|byte| *byte == 0) {
        let field = field.trim_ascii_start();
        match field.first() {
            Some(b'p') => pid = std::str::from_utf8(&field[1..])?.parse()?,
            Some(b'f') => cwd = &field[1..] == b"cwd",
            Some(b'n')
                if field.starts_with(b"n/")
                    && pid != own_pid
                    && !(cwd && ancestors.contains(&pid)) =>
            {
                let key = (field[1..].to_vec(), cwd);
                if paths.contains_key(&key) {
                    continue;
                }
                let path = PathBuf::from(OsString::from_vec(field[1..].to_vec()));
                let path = path.canonicalize().unwrap_or(path);
                let owner = if cwd {
                    worktrees
                        .iter()
                        .filter(|root| path.starts_with(root))
                        .max_by_key(|root| root.components().count())
                        .cloned()
                } else {
                    None
                };
                paths.insert(key, (path, owner));
            }
            _ => {}
        }
    }
    Ok(paths.into_values().collect())
}

async fn open_paths(
    worktrees: &[PathBuf],
    cancel: &watch::Receiver<bool>,
) -> Result<Vec<(PathBuf, Option<PathBuf>)>> {
    let ps = command(
        &["ps".into(), "-axo".into(), "pid=,ppid=".into()],
        Path::new("/"),
        cancel,
    )
    .await?;
    let lsof = command(
        &["lsof".into(), "-nP".into(), "-F".into(), "pfn0".into()],
        Path::new("/"),
        cancel,
    )
    .await?;
    parse_open_paths(&ps, &lsof, worktrees, std::process::id())
}

fn in_use(path: &Path, owners: &BTreeSet<PathBuf>, opened: &[(PathBuf, Option<PathBuf>)]) -> bool {
    opened.iter().any(|(opened, cwd_owner)| {
        opened.starts_with(path)
            || cwd_owner
                .as_ref()
                .is_some_and(|owner| owners.contains(owner))
    })
}

fn profile_locks(path: &Path) -> Result<Option<Vec<File>>> {
    let mut locks = Vec::new();
    for name in LOCKS {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path.join(name))?;
        match file.try_lock() {
            Ok(()) => locks.push(file),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Ok(Some(locks))
}

fn disposable(bytes: u64, max_bytes: u64, modified: f64, cutoff: f64) -> bool {
    bytes > max_bytes || modified < cutoff
}

async fn prune(
    worktrees: &[PathBuf],
    dry_run: bool,
    max_bytes: u64,
    max_age: f64,
    now: f64,
    cancel: &watch::Receiver<bool>,
) -> Result<Report> {
    let worktrees = worktrees
        .iter()
        .map(|root| root.canonicalize())
        .collect::<std::io::Result<Vec<_>>>()?;
    let opened = open_paths(&worktrees, cancel).await?;
    let mut entries = Vec::new();
    for (path, owners) in profiles(&worktrees, cancel).await? {
        let mut entry = Entry {
            path,
            owners,
            bytes: None,
            modified: 0.0,
            action: Action::InUse,
        };
        if !in_use(&entry.path, &entry.owners, &opened) {
            if let Some(_locks) = profile_locks(&entry.path)? {
                let (bytes, modified) = usage(&entry.path)?;
                entry.bytes = Some(bytes);
                entry.modified = modified;
                entry.action = Action::Keep;
            } else {
                entry.action = Action::Locked;
            }
        }
        entries.push(entry);
    }
    let before_idle_bytes = entries.iter().filter_map(|entry| entry.bytes).sum();
    let mut remaining = before_idle_bytes;
    entries.sort_by(|a, b| a.modified.total_cmp(&b.modified).then(a.path.cmp(&b.path)));
    for entry in &mut entries {
        if entry.action != Action::Keep
            || !disposable(remaining, max_bytes, entry.modified, now - max_age)
        {
            continue;
        }
        let Some(_locks) = profile_locks(&entry.path)? else {
            remaining -= entry.bytes.take().unwrap();
            entry.action = Action::Locked;
            continue;
        };
        if in_use(
            &entry.path,
            &entry.owners,
            &open_paths(&worktrees, cancel).await?,
        ) {
            remaining -= entry.bytes.take().unwrap();
            entry.action = Action::InUse;
            continue;
        }
        let (size, modified) = usage(&entry.path)?;
        remaining = remaining - entry.bytes.replace(size).unwrap() + size;
        entry.modified = modified;
        if !disposable(remaining, max_bytes, modified, now - max_age) {
            continue;
        }
        if !dry_run {
            for child in fs::read_dir(&entry.path)? {
                let child = child?;
                if LOCKS.iter().any(|name| child.file_name() == *name) {
                    continue;
                }
                if child.file_type()?.is_dir() {
                    fs::remove_dir_all(child.path())?;
                } else {
                    fs::remove_file(child.path())?;
                }
            }
        }
        entry.action = if dry_run {
            Action::WouldClean
        } else {
            Action::Cleaned
        };
        remaining -= size;
    }
    Ok(Report {
        dry_run,
        max_idle_bytes: max_bytes,
        max_age_days: max_age / 86400.0,
        before_idle_bytes,
        after_idle_bytes: remaining,
        reclaimed_bytes: entries
            .iter()
            .filter(|entry| entry.action == Action::Cleaned)
            .filter_map(|entry| entry.bytes)
            .sum(),
        would_reclaim_bytes: entries
            .iter()
            .filter(|entry| entry.action == Action::WouldClean)
            .filter_map(|entry| entry.bytes)
            .sum(),
        entries,
    })
}

pub async fn run(dry_run: bool) -> Result<()> {
    let cancel = supervision::cancellation();
    let cwd = std::env::current_dir()?;
    let common = command(
        &[
            "git".into(),
            "rev-parse".into(),
            "--path-format=absolute".into(),
            "--git-common-dir".into(),
        ],
        &cwd,
        &cancel,
    )
    .await?;
    let common = PathBuf::from(OsString::from_vec(common.trim_ascii_end().to_vec()));
    let worktrees = command(
        &[
            "git".into(),
            "worktree".into(),
            "list".into(),
            "--porcelain".into(),
            "-z".into(),
        ],
        &cwd,
        &cancel,
    )
    .await?;
    let worktrees = worktrees
        .split(|byte| *byte == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .collect::<Vec<_>>();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(common.join("bex-build-cleanup.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            println!("Build cleanup already running; left its outputs alone.");
            return Ok(());
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &prune(&worktrees, dry_run, MAX_BYTES, MAX_AGE, now, &cancel).await?
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn keeping_a_profile_is_monotonic_when_budget_and_retention_grow(bytes in 0u64..1000, budget in 0u64..1000, extra in 0u64..1000, modified in 0u32..1000, cutoff in 0u32..1000) {
            if !disposable(bytes, budget, f64::from(modified), f64::from(cutoff)) {
                prop_assert!(!disposable(bytes, budget + extra, f64::from(modified), f64::from(cutoff.saturating_sub(extra as u32))));
            }
        }
    }

    #[test]
    fn exact_capacity_and_retention_boundaries_keep_the_build() {
        assert!(!disposable(10, 10, 1000.0, 1000.0));
        assert!(disposable(10, 9, 1001.0, 1000.0));
        assert!(disposable(10, 11, 999.0, 1000.0));
        assert!(!disposable(10, 11, 1001.0, 1000.0));
    }

    #[test]
    fn duplicate_descriptors_preserve_cwd_and_nested_owner_protection() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().canonicalize().unwrap();
        let nested = parent.join("nested");
        fs::create_dir(&nested).unwrap();
        let ps = b"1 0\n2 1\n3 2\n4 1\n5 1\n";
        let lsof = format!(
            "p2\0fcwd\0n{}\0p4\0fcwd\0n{}\0p5\0fcwd\0n{}\0f1\0n{}\0",
            parent.display(),
            nested.display(),
            nested.display(),
            nested.display()
        );
        let opened =
            parse_open_paths(ps, lsof.as_bytes(), &[parent.clone(), nested.clone()], 3).unwrap();
        assert_eq!(opened.len(), 2);
        assert!(!in_use(
            &parent.join("target/debug"),
            &BTreeSet::from([parent.clone()]),
            &opened
        ));
        assert!(in_use(
            &nested.join("target/debug"),
            &BTreeSet::from([nested.clone()]),
            &opened
        ));
        assert!(in_use(&nested, &BTreeSet::from([parent]), &opened));
    }
}
