#!/usr/bin/env python3
"""Bound disposable Cargo outputs; preserve live builds, apps and test records."""

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

TAG = b"Signature: 8a477f597d28d172789f06886806bc55"
LOCKS = (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock")
MAX_BYTES = 32 * 1024**3
MAX_AGE = 3 * 86400


def command(*args, cwd=None):
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, timeout=60).stdout


def cache_root(path):
    tag = path / "CACHEDIR.TAG"
    return (path.suffix not in (".app", ".xcresult", ".xcarchive") and not path.is_symlink() and
            tag.is_file() and not tag.is_symlink() and tag.read_bytes().startswith(TAG))


def profiles(worktrees, common):
    found = {}
    targets = set(command(os.environ.get("RUSTC", "rustc"), "--print", "target-list").decode().splitlines())
    for owner, target in [(p, p / "target") for p in worktrees] + [(None, common / "bex-quality/cargo-target")]:
        if not target.is_dir() or target.is_symlink():
            continue
        roots = ([target] if cache_root(target) else []) + [p for p in target.iterdir() if p.is_dir() and not p.is_symlink() and cache_root(p)]
        for root in roots:
            parents = [root] + [p for p in root.iterdir() if p.name in targets and p.is_dir() and not p.is_symlink()]
            for parent in parents:
                for name in ("debug", "release"):
                    path = parent / name
                    if (path.is_dir() and not path.is_symlink() and
                            (path / ".fingerprint").is_dir() and not (path / ".fingerprint").is_symlink() and
                            any((path / lock).is_file() for lock in LOCKS)):
                        found[path.resolve()] = owner
    return found


def usage(path):
    size = 0
    modified = 0
    seen = set()
    pending = [path]
    while pending:
        directory = pending.pop()
        for entry in os.scandir(directory):
            if entry.name in LOCKS:
                continue
            info = entry.stat(follow_symlinks=False)
            identity = (info.st_dev, info.st_ino)
            if identity in seen:
                continue
            seen.add(identity)
            size += info.st_blocks * 512
            modified = max(modified, info.st_mtime)
            if entry.is_dir(follow_symlinks=False):
                pending.append(Path(entry.path))
    return size, modified


def open_paths(worktrees):
    parents = {}
    for line in command("ps", "-axo", "pid=,ppid=").splitlines():
        pid, parent = map(int, line.split())
        parents[pid] = parent
    ancestors = set()
    pid = os.getpid()
    while pid > 1 and pid not in ancestors:
        ancestors.add(pid)
        pid = parents.get(pid, 0)
    paths = []
    pid = 0
    descriptor = b""
    for field in command("lsof", "-nP", "-F", "pfn0", cwd="/").split(b"\0"):
        field = field.lstrip(b"\n")
        if field.startswith(b"p"):
            pid = int(field[1:])
        elif field.startswith(b"f"):
            descriptor = field[1:]
        elif field.startswith(b"n/") and pid != os.getpid():
            if descriptor == b"cwd" and pid in ancestors:
                continue
            path = Path(os.fsdecode(field[1:])).resolve()
            owners = [root for root in worktrees if path.is_relative_to(root)] if descriptor == b"cwd" else []
            paths.append((path, max(owners, key=lambda root: len(root.parts), default=None)))
    return paths


def in_use(path, owner, opened):
    return any(p == path or p.is_relative_to(path) or
               (owner is not None and cwd_owner == owner)
               for p, cwd_owner in opened)


@contextlib.contextmanager
def profile_locks(path):
    # Keep the lock inodes in place: Cargo waits on these across clean/build races.
    with contextlib.ExitStack() as stack:
        for name in LOCKS:
            descriptor = os.open(path / name, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            file = stack.enter_context(os.fdopen(descriptor, "a+b"))
            fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def prune(worktrees, common, dry_run=False, max_bytes=MAX_BYTES, max_age=MAX_AGE):
    common = common.resolve()
    worktrees = [p.resolve() for p in worktrees]
    report = {"dryRun": dry_run, "maxIdleBytes": max_bytes, "maxAgeDays": max_age / 86400, "entries": []}
    opened = open_paths(worktrees)
    for path, owner in profiles(worktrees, common).items():
        if in_use(path, owner, opened):
            report["entries"].append({"path": str(path), "bytes": None, "modified": 0, "action": "in-use"})
            continue
        try:
            with profile_locks(path):
                size, modified = usage(path)
        except BlockingIOError:
            report["entries"].append({"path": str(path), "bytes": None, "modified": 0, "action": "locked"})
            continue
        report["entries"].append({"path": str(path), "bytes": size, "modified": modified,
                                  "owner": str(owner) if owner else None,
                                  "action": "keep"})
    report["beforeIdleBytes"] = remaining = sum(e["bytes"] or 0 for e in report["entries"])
    cutoff = time.time() - max_age
    for entry in sorted(report["entries"], key=lambda e: (e["modified"], e["path"])):
        if entry["action"] in ("in-use", "locked") or (remaining <= max_bytes and entry["modified"] >= cutoff):
            continue
        path = Path(entry["path"])
        owner = Path(entry["owner"]) if entry["owner"] else None
        try:
            with profile_locks(path):
                if in_use(path, owner, open_paths(worktrees)):
                    remaining -= entry["bytes"]
                    entry["bytes"] = None
                    entry["action"] = "in-use"
                    continue
                size, modified = usage(path)
                remaining += size - entry["bytes"]
                entry.update(bytes=size, modified=modified)
                if remaining <= max_bytes and modified >= cutoff:
                    continue
                if not dry_run:
                    for child in path.iterdir():
                        if child.name in LOCKS:
                            continue
                        if child.is_dir() and not child.is_symlink():
                            shutil.rmtree(child)
                        else:
                            child.unlink()
                entry["action"] = "would-clean" if dry_run else "cleaned"
                remaining -= size
        except BlockingIOError:
            remaining -= entry["bytes"]
            entry["bytes"] = None
            entry["action"] = "locked"
    report["afterIdleBytes"] = remaining
    report["reclaimedBytes"] = sum(e["bytes"] for e in report["entries"] if e["action"] == "cleaned")
    report["wouldReclaimBytes"] = sum(e["bytes"] for e in report["entries"] if e["action"] == "would-clean")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="report without deleting build outputs")
    args = parser.parse_args()
    common = Path(os.fsdecode(command("git", "rev-parse", "--path-format=absolute", "--git-common-dir")).strip())
    worktrees = [Path(os.fsdecode(field[9:])) for field in
                 command("git", "worktree", "list", "--porcelain", "-z").split(b"\0")
                 if field.startswith(b"worktree ")]
    with (common / "bex-build-cleanup.lock").open("a+b") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("Build cleanup already running; left its outputs alone.")
            return
        report = prune(worktrees, common, args.dry_run)
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
