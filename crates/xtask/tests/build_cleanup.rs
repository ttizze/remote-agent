#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
};
use xtask::supervision;
mod support;
use support::Fixture;

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn cli_discovers_worktrees_preserves_sources_and_rejects_failed_inspection() {
    let fixture = Fixture::new().await;
    let root = fixture.project("main with spaces").await;
    git(&root, &["init"]);
    let unmanaged = fixture.root.join("unmanaged-build");
    fixture.build(&root, &unmanaged).await;
    git(&root, &["add", "Cargo.toml", "src"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "fixture",
        ],
    );
    let worktree = fixture.root.join("another worktree");
    git(
        &root,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()],
    );
    fixture.build(&worktree, &worktree.join("target")).await;
    let external = fixture.root.join("build volume");
    fs::rename(worktree.join("target"), &external).unwrap();
    symlink(&external, worktree.join("target")).unwrap();
    for project in [&root, &worktree] {
        Fixture::age(&project.join("target/debug"), 4);
    }
    let source = worktree.join("src/main.rs");
    let original = fs::read(&source).unwrap();
    let mut changed = original.clone();
    changed.extend(b"\n// uncommitted user change\n");
    fs::write(&source, &changed).unwrap();
    let run = |dry_run: bool, path: Option<&std::ffi::OsStr>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.arg("clean-builds").current_dir(&worktree);
        if dry_run {
            command.arg("--dry-run");
        }
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.output().unwrap()
    };
    let bin = fixture.root.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("lsof"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(bin.join("lsof"), fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    assert!(!run(false, Some(&path)).status.success());
    Fixture::runs(&root);
    Fixture::runs(&worktree);
    let output = run(true, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"].as_array().unwrap().len(), 2, "{report}");
    assert!(
        report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["action"] == "would-clean")
    );
    Fixture::runs(&root);
    Fixture::runs(&worktree);
    let output = run(false, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["action"] == "cleaned")
    );
    assert!(!Fixture::executable(&root).exists());
    assert!(!Fixture::executable(&worktree).exists());
    assert_eq!(fs::read(&source).unwrap(), changed);
    assert_eq!(fs::read(root.join("src/main.rs")).unwrap(), original);
    assert!(unmanaged.join("debug/cache-check").is_file());
    fixture.build(&worktree, &worktree.join("target")).await;
    Fixture::runs(&worktree);
}
