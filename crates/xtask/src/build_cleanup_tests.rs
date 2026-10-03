use super::*;
use crate::{supervision::Child, test_support::Fixture};
use std::{os::unix::fs::symlink, time::Instant};
use tokio::{io::AsyncWriteExt, process::Command};

async fn inspect(worktrees: &[PathBuf], common: &Path, dry: bool, budget: u64) -> Report {
    let (_sender, cancel) = watch::channel(false);
    prune(
        worktrees,
        common,
        dry,
        budget,
        MAX_AGE,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64(),
        &cancel,
    )
    .await
    .unwrap()
}

async fn await_cleaned(worktrees: &[PathBuf], common: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let report = inspect(worktrees, common, false, 0).await;
        if report.entries[0].action == Action::Cleaned {
            return;
        }
        // Concurrent fixture spawns can briefly inherit a held flock until
        // exec closes it. Active-worktree protection must already be gone.
        assert_eq!(report.entries[0].action, Action::Locked);
        assert!(Instant::now() < deadline, "{report:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn old_outputs_rebuild_while_records_backups_external_links_and_lock_inodes_survive() {
    let fixture = Fixture::new().await;
    let root = fixture.project("old").await;
    let profile = root.join("target/debug");
    let evidence = root.join("target/qa/result.summary.json");
    fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    fs::write(&evidence, "{\"passedTests\":1}").unwrap();
    let backup = root.join("target/previous.app");
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join("host"), b"previous host").unwrap();
    let outside = fixture.root.join("user-data");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("draft"), "preserve me").unwrap();
    symlink(&outside, profile.join("outside")).unwrap();
    let _locks = profile_locks(&profile).unwrap().unwrap();
    drop(_locks);
    let locks = LOCKS.map(|name| fs::metadata(profile.join(name)).unwrap().ino());
    Fixture::age(&profile, 2);
    assert_eq!(
        inspect(
            std::slice::from_ref(&root),
            &fixture.common,
            false,
            MAX_BYTES
        )
        .await
        .entries[0]
            .action,
        Action::Keep
    );
    Fixture::age(&profile, 4);
    assert_eq!(
        inspect(
            std::slice::from_ref(&root),
            &fixture.common,
            true,
            MAX_BYTES
        )
        .await
        .entries[0]
            .action,
        Action::WouldClean
    );
    Fixture::runs(&root);
    let report = inspect(
        std::slice::from_ref(&root),
        &fixture.common,
        false,
        MAX_BYTES,
    )
    .await;
    assert_eq!(report.entries[0].action, Action::Cleaned);
    assert!(report.reclaimed_bytes > 0);
    assert!(!Fixture::executable(&root).exists());
    assert_eq!(
        LOCKS.map(|name| fs::metadata(profile.join(name)).unwrap().ino()),
        locks
    );
    assert_eq!(fs::read_to_string(evidence).unwrap(), "{\"passedTests\":1}");
    assert_eq!(fs::read(backup.join("host")).unwrap(), b"previous host");
    assert_eq!(
        fs::read_to_string(outside.join("draft")).unwrap(),
        "preserve me"
    );
    fixture.build(&root, &root.join("target")).await;
    Fixture::runs(&root);
    let output = std::process::Command::new(Fixture::executable(&root))
        .arg("panic")
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("src/main.rs:"));
}

#[tokio::test]
async fn capacity_evicts_oldest_recent_profile_and_keeps_newer_build() {
    let fixture = Fixture::new().await;
    let older = fixture.project("older").await;
    let newer = fixture.project("newer").await;
    Fixture::age(&older.join("target/debug"), 1);
    let budget = usage(&newer.join("target/debug")).unwrap().0;
    let report = inspect(
        &[older.clone(), newer.clone()],
        &fixture.common,
        false,
        budget,
    )
    .await;
    assert!(report.after_idle_bytes <= budget);
    assert!(!Fixture::executable(&older).exists());
    Fixture::runs(&newer);
    assert_eq!(
        inspect(&[older, newer], &fixture.common, false, budget)
            .await
            .reclaimed_bytes,
        0
    );
}

#[tokio::test]
async fn running_binary_and_active_worktree_stay_protected_until_exit() {
    let fixture = Fixture::new().await;
    let root = fixture.project("live").await;
    Fixture::age(&root.join("target/debug"), 30);
    for cwd in [&fixture.root, &root] {
        let mut command = Command::new(Fixture::executable(&root));
        command
            .arg("hold")
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped());
        let mut process = Child::spawn(command).unwrap();
        let mut stdin = process.take_stdin().unwrap();
        assert_eq!(
            inspect(std::slice::from_ref(&root), &fixture.common, false, 0)
                .await
                .entries[0]
                .action,
            Action::InUse
        );
        Fixture::runs(&root);
        stdin.write_all(b"\n").await.unwrap();
        drop(stdin);
        let (_sender, cancel) = watch::channel(false);
        assert!(
            process
                .output(&cancel, Duration::from_secs(5))
                .await
                .unwrap()
                .status
                .success()
        );
    }
    await_cleaned(&[root], &fixture.common).await;
}

#[tokio::test]
async fn real_cargo_waits_for_the_same_cleanup_lock_inode() {
    let fixture = Fixture::new().await;
    let root = fixture.project("locked").await;
    let locks = profile_locks(&root.join("target/debug")).unwrap().unwrap();
    let mut process = Child::spawn(fixture.cargo(&root, &root.join("target"))).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(process.try_wait().unwrap().is_none());
    drop(locks);
    let (_sender, cancel) = watch::channel(false);
    let output = process
        .output(&cancel, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Blocking waiting for file lock"));
    Fixture::runs(&root);
}

#[tokio::test]
async fn active_cargo_in_shared_target_is_not_cleaned() {
    let fixture = Fixture::new().await;
    let root = fixture.project("shared").await;
    let target = fixture.common.join("bex-quality/cargo-target");
    fixture.build(&root, &target).await;
    fs::write(root.join("build.rs"), "fn main() { std::fs::write(\"ready\", \"\").unwrap(); while !std::path::Path::new(\"release-build\").exists() { std::thread::sleep(std::time::Duration::from_millis(20)); } }").unwrap();
    let mut process = Child::spawn(fixture.cargo(&root, &target)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !root.join("ready").exists() {
        assert!(process.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let report = inspect(&[], &fixture.common, false, 0).await;
    assert!(matches!(
        report.entries[0].action,
        Action::InUse | Action::Locked
    ));
    assert!(target.join("debug/.fingerprint").is_dir());
    fs::write(root.join("release-build"), "").unwrap();
    let (_sender, cancel) = watch::channel(false);
    assert!(
        process
            .output(&cancel, Duration::from_secs(30))
            .await
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        inspect(&[], &fixture.common, false, 0).await.entries[0].action,
        Action::Cleaned
    );
}

#[tokio::test]
async fn external_symlink_and_untagged_nested_outputs_are_not_candidates() {
    let fixture = Fixture::new().await;
    let outside = fixture.project("outside").await;
    let root = fixture.root.join("inside");
    fs::create_dir(&root).unwrap();
    symlink(outside.join("target"), root.join("target")).unwrap();
    assert!(
        inspect(std::slice::from_ref(&root), &fixture.common, false, 0)
            .await
            .entries
            .is_empty()
    );
    fs::remove_file(root.join("target")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::remove_file(outside.join("target/CACHEDIR.TAG")).unwrap();
    fs::rename(outside.join("target"), root.join("target/unknown")).unwrap();
    assert!(
        inspect(std::slice::from_ref(&root), &fixture.common, false, 0)
            .await
            .entries
            .is_empty()
    );
    fs::rename(root.join("target/unknown"), outside.join("target")).unwrap();
    Fixture::runs(&outside);
}

#[tokio::test]
async fn untagged_cargo_profile_is_not_cleaned() {
    let fixture = Fixture::new().await;
    let root = fixture.project("untagged").await;
    fs::remove_file(root.join("target/CACHEDIR.TAG")).unwrap();
    assert!(
        inspect(std::slice::from_ref(&root), &fixture.common, false, 0)
            .await
            .entries
            .is_empty()
    );
    fixture.build(&root, &root.join("target")).await;
    Fixture::runs(&root);
}

#[tokio::test]
async fn nested_worktree_activity_does_not_pin_parent_builds() {
    let fixture = Fixture::new().await;
    let root = fixture.project("main").await;
    let nested = root.join(".git/worktrees/active");
    fs::create_dir_all(&nested).unwrap();
    let mut command = Command::new("sleep");
    command.arg("30").current_dir(&nested);
    let mut process = Child::spawn(command).unwrap();
    await_cleaned(&[root.clone(), nested], &fixture.common).await;
    process.stop(false).await.unwrap();
}

#[tokio::test]
async fn symlinked_lock_leaves_builds_and_external_data_untouched() {
    let fixture = Fixture::new().await;
    let root = fixture.project("inspection-failure").await;
    let outside = fixture.root.join("outside-lock");
    fs::write(&outside, "private").unwrap();
    fs::remove_file(root.join("target/debug/.cargo-build-lock")).unwrap();
    symlink(&outside, root.join("target/debug/.cargo-build-lock")).unwrap();
    let (_sender, cancel) = watch::channel(false);
    assert!(
        prune(
            std::slice::from_ref(&root),
            &fixture.common,
            false,
            0,
            MAX_AGE,
            0.0,
            &cancel
        )
        .await
        .is_err()
    );
    Fixture::runs(&root);
    assert_eq!(fs::read_to_string(outside).unwrap(), "private");
}
