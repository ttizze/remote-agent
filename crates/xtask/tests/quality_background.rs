// The Nix worker runs on Unix; Windows CI runs native Rust commands directly.
#![cfg(unix)]

use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn checks_committed_snapshot_and_exposes_failures_without_touching_source() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Quality test"]);
    git(root, &["config", "user.email", "quality@example.invalid"]);
    fs::write(root.join("subject"), "committed").unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture"],
    );
    let commit = git(root, &["rev-parse", "HEAD"]);
    let state = root.join(".git/bex-quality");
    fs::create_dir_all(state.join("queue")).unwrap();
    let bin = root.join(".git/bin");
    fs::create_dir(&bin).unwrap();
    // Nix/tool downloads are the expensive boundary. Git and worker are real.
    let nix = bin.join("nix");
    fs::write(
        &nix,
        "#!/bin/sh\n[ \"$(cat \"$2/subject\")\" = committed ] || exit 7\necho checked-snapshot\n",
    )
    .unwrap();
    fs::set_permissions(&nix, fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let enqueue = || {
        fs::write(
            state.join(format!("queue/{commit}.request")),
            format!("{commit}\n{}", root.display()),
        )
        .unwrap()
    };
    let run = |command: &str| {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg(command)
            .current_dir(root)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    fs::write(root.join("subject"), "uncommitted").unwrap();
    enqueue();
    let output = run("quality-worker");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run("quality-status");
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["commit"], commit);
    assert_eq!(result["workingTreeDirty"], true);
    assert_eq!(
        fs::read_to_string(root.join("subject")).unwrap(),
        "uncommitted"
    );
    fs::write(root.join("subject"), "committed").unwrap();
    assert!(run("quality-status").status.success());
    fs::write(nix, "#!/bin/sh\necho deliberate-failure >&2\nexit 9\n").unwrap();
    enqueue();
    assert!(run("quality-worker").status.success());
    let output = run("quality-status");
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "failed");
    assert!(
        fs::read_to_string(result["log"].as_str().unwrap())
            .unwrap()
            .contains("deliberate-failure")
    );
    assert_eq!(
        git(root, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

#[test]
fn coalesces_only_queued_commits_from_the_same_worktree() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Quality test"]);
    git(root, &["config", "user.email", "quality@example.invalid"]);
    let state = root.join(".git/bex-quality");
    fs::create_dir_all(state.join("queue")).unwrap();
    let bin = root.join(".git/bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("nix"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("nix"), fs::Permissions::from_mode(0o700)).unwrap();
    let mut commits = Vec::new();
    for index in 0..3 {
        fs::write(root.join("subject"), index.to_string()).unwrap();
        git(root, &["add", "."]);
        git(
            root,
            &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture"],
        );
        let commit = git(root, &["rev-parse", "HEAD"]);
        let request = state.join(format!("queue/{commit}.request"));
        // The middle request belongs to another source worktree.
        fs::write(
            &request,
            format!(
                "{commit}\n{}",
                if index == 1 {
                    "/another-worktree".into()
                } else {
                    root.display().to_string()
                }
            ),
        )
        .unwrap();
        fs::File::open(request)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(index + 1))
            .unwrap();
        commits.push(commit);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("quality-worker")
        .current_dir(root)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (index, expected) in ["superseded", "passed", "passed"].iter().enumerate() {
        let result: Value = serde_json::from_slice(
            &fs::read(state.join(format!("results/{}.json", commits[index]))).unwrap(),
        )
        .unwrap();
        assert_eq!(result["status"], *expected);
    }
    assert_eq!(fs::read_dir(state.join("queue")).unwrap().count(), 0);
}
