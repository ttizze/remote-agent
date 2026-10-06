//! T3 `checkpointing/CheckpointStore.test.ts` and `Diffs.test.ts`, plus the restore
//! journal cases of this Host.
use super::*;

const FROM: &str = "refs/t3/test/turn/0";
const TO: &str = "refs/t3/test/turn/1";

async fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"], None).await.unwrap();
    dir
}
/// `initRepoWithCommit`.
async fn repo_with_commit() -> tempfile::TempDir {
    let dir = repo().await;
    let cwd = dir.path();
    git(cwd, &["config", "user.email", "test@test.com"], None)
        .await
        .unwrap();
    git(cwd, &["config", "user.name", "Test"], None)
        .await
        .unwrap();
    std::fs::write(cwd.join("README.md"), "# test\n").unwrap();
    git(cwd, &["add", "."], None).await.unwrap();
    git(cwd, &["commit", "-m", "initial commit"], None)
        .await
        .unwrap();
    dir
}
fn large_text(lines: usize) -> String {
    (0..lines)
        .map(|index| format!("line {index:05}\n"))
        .collect()
}
/// `parseTurnDiffFilesFromNumstat`, compared regardless of order.
fn summary(numstat: &str) -> Vec<(String, u64, u64)> {
    let mut files = turn_diff_files(numstat);
    files.sort();
    files
}
fn files(entries: &[(&str, u64, u64)]) -> Vec<(String, u64, u64)> {
    let mut files: Vec<_> = entries
        .iter()
        .map(|(path, added, deleted)| ((*path).to_owned(), *added, *deleted))
        .collect();
    files.sort();
    files
}

#[tokio::test]
async fn detects_git_repositories_including_nested_workspaces() {
    let plain = tempfile::tempdir().unwrap();
    assert!(!Checkpoints::is_git_repository(plain.path()).await);
    let dir = repo_with_commit().await;
    assert!(Checkpoints::is_git_repository(dir.path()).await);
    let nested = dir.path().join("packages/nested");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(Checkpoints::is_git_repository(&nested).await);
}

#[tokio::test]
async fn returns_full_oversized_checkpoint_diffs_without_truncation() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    store.capture(cwd, FROM).await.unwrap();
    std::fs::write(cwd.join("README.md"), large_text(5_000)).unwrap();
    store.capture(cwd, TO).await.unwrap();
    let diff = store
        .diff(cwd, FROM, TO, true, DiffFormat::Patch, false)
        .await
        .unwrap();
    assert!(diff.contains("diff --git"));
    assert!(!diff.contains("[truncated]"));
    assert!(diff.contains("+line 04999"));
}

#[tokio::test]
async fn keeps_patch_prefixes_when_the_repository_disables_them() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    git(cwd, &["config", "diff.noprefix", "true"], None)
        .await
        .unwrap();
    store.capture(cwd, FROM).await.unwrap();
    std::fs::write(cwd.join("README.md"), "# changed\n").unwrap();
    store.capture(cwd, TO).await.unwrap();
    let diff = store
        .diff(cwd, FROM, TO, false, DiffFormat::Patch, false)
        .await
        .unwrap();
    assert!(diff.contains("diff --git a/README.md b/README.md"));
}

#[tokio::test]
async fn can_hide_indentation_churn_when_changes_wrap_existing_lines() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    let component = cwd.join("Component.tsx");
    std::fs::write(
        &component,
        [
            "export function View() {",
            "  return (",
            "    <section>",
            "      <h1>Title</h1>",
            "      <p>Body</p>",
            "    </section>",
            "  );",
            "}",
            "",
        ]
        .join("\n"),
    )
    .unwrap();
    store.capture(cwd, FROM).await.unwrap();
    std::fs::write(
        &component,
        [
            "export function View() {",
            "  return (",
            "    <section>",
            "      {isReady ? (",
            "        <div>",
            "          <h1>Title</h1>",
            "          <p>Body</p>",
            "        </div>",
            "      ) : null}",
            "    </section>",
            "  );",
            "}",
            "",
        ]
        .join("\n"),
    )
    .unwrap();
    store.capture(cwd, TO).await.unwrap();
    let normal = store
        .diff(cwd, FROM, TO, false, DiffFormat::Patch, false)
        .await
        .unwrap();
    let ignored = store
        .diff(cwd, FROM, TO, true, DiffFormat::Patch, false)
        .await
        .unwrap();
    assert!(normal.contains("diff --git"));
    assert!(normal.contains("-      <h1>Title</h1>"));
    assert!(normal.contains("+          <h1>Title</h1>"));
    assert!(ignored.contains("diff --git"));
    assert!(ignored.contains("+      {isReady ? ("));
    assert!(ignored.contains("+        <div>"));
    assert!(!ignored.contains("-      <h1>Title</h1>"));
    assert!(!ignored.contains("+          <h1>Title</h1>"));
    for ignore_whitespace in [false, true] {
        let numstat = store
            .diff(cwd, FROM, TO, ignore_whitespace, DiffFormat::Numstat, false)
            .await
            .unwrap();
        assert_eq!(
            summary(&numstat),
            files(&[(
                "Component.tsx",
                if ignore_whitespace { 4 } else { 6 },
                if ignore_whitespace { 0 } else { 2 }
            )])
        );
    }
}

#[tokio::test]
async fn counts_changes_whose_full_patch_exceeds_the_output_limit() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    let lines = 20_000;
    std::fs::write(
        cwd.join("README.md"),
        format!("{}\n", "before".repeat(50)).repeat(lines),
    )
    .unwrap();
    store.capture(cwd, FROM).await.unwrap();
    std::fs::write(
        cwd.join("README.md"),
        format!("{}\n", "after".repeat(60)).repeat(lines),
    )
    .unwrap();
    store.capture(cwd, TO).await.unwrap();
    let numstat = store
        .diff(cwd, FROM, TO, false, DiffFormat::Numstat, false)
        .await
        .unwrap();
    assert_eq!(
        summary(&numstat),
        files(&[("README.md", lines as u64, lines as u64)])
    );
    assert!(numstat.len() < 100);
}

#[tokio::test]
async fn preserves_file_paths_and_turn_ranges_without_changing_the_user_index() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    git(cwd, &["config", "diff.renames", "copies"], None)
        .await
        .unwrap();
    let (baseline, first, second) = (FROM, TO, "refs/t3/test/turn/2");
    let copied: String = (0..20)
        .map(|index| format!("copy line {index}\n"))
        .collect();
    let (renamed, added) = if cfg!(windows) {
        ("renamed café.txt", "new café.txt")
    } else {
        ("renamed\tcafé\nname.txt", "new\tfile\n名.txt")
    };
    for (path, contents) in [
        ("copy-source.txt", copied.as_str()),
        ("deleted.txt", "delete me\n"),
        ("rename-old.txt", "before\nkeep one\nkeep two\nkeep three\n"),
        ("binary.bin", "\0before"),
    ] {
        std::fs::write(cwd.join(path), contents).unwrap();
    }
    store.capture(cwd, baseline).await.unwrap();
    std::fs::rename(cwd.join("rename-old.txt"), cwd.join(renamed)).unwrap();
    std::fs::remove_file(cwd.join("deleted.txt")).unwrap();
    for (path, contents) in [
        ("copy-source.txt", format!("{copied}one more\n")),
        ("copied.txt", copied.clone()),
        (renamed, "after\nkeep one\nkeep two\nkeep three\n".into()),
        ("binary.bin", "\0after".into()),
        ("empty.txt", String::new()),
        (added, "first\nsecond\n".into()),
    ] {
        std::fs::write(cwd.join(path), contents).unwrap();
    }
    store.capture(cwd, first).await.unwrap();
    let user_index = std::fs::read(cwd.join(".git/index")).unwrap();
    let numstat = |from: &'static str, to: &'static str| {
        let store = &store;
        async move {
            summary(
                &store
                    .diff(cwd, from, to, false, DiffFormat::Numstat, false)
                    .await
                    .unwrap(),
            )
        }
    };
    let expected = [
        ("binary.bin", 0, 0),
        ("copied.txt", 0, 0),
        ("copy-source.txt", 1, 0),
        ("deleted.txt", 0, 1),
        ("empty.txt", 0, 0),
        (added, 2, 0),
        (renamed, 1, 1),
    ];
    assert_eq!(numstat(baseline, first).await, files(&expected));

    std::fs::remove_file(cwd.join("empty.txt")).unwrap();
    std::fs::write(cwd.join("copy-source.txt"), "replacement\n").unwrap();
    store.capture(cwd, second).await.unwrap();
    assert_eq!(
        numstat(first, second).await,
        files(&[("copy-source.txt", 1, 21), ("empty.txt", 0, 0)])
    );
    let inclusive: Vec<_> = expected
        .iter()
        .filter(|(path, ..)| *path != "empty.txt")
        .map(|(path, added, deleted)| {
            if *path == "copy-source.txt" {
                (*path, 1, 20)
            } else {
                (*path, *added, *deleted)
            }
        })
        .collect();
    assert_eq!(numstat(baseline, second).await, files(&inclusive));
    assert_eq!(
        store
            .diff(cwd, baseline, baseline, false, DiffFormat::Numstat, false)
            .await
            .unwrap(),
        ""
    );
    assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), user_index);
}

#[tokio::test]
async fn uses_head_for_a_missing_baseline_only_when_requested() {
    let dir = repo_with_commit().await;
    let (cwd, store) = (dir.path(), Checkpoints::default());
    std::fs::write(cwd.join("README.md"), "changed\n").unwrap();
    store.capture(cwd, TO).await.unwrap();
    assert!(
        store
            .diff(cwd, FROM, TO, false, DiffFormat::Numstat, false)
            .await
            .is_err()
    );
    let numstat = store
        .diff(cwd, FROM, TO, false, DiffFormat::Numstat, true)
        .await
        .unwrap();
    assert_eq!(summary(&numstat), files(&[("README.md", 1, 1)]));
}

#[test]
fn numstat_summaries_follow_the_reference_parser() {
    let parsed = |numstat: &str| turn_diff_files(numstat);
    let files = |entries: &[(&str, u64, u64)]| {
        entries
            .iter()
            .map(|(path, added, deleted)| ((*path).to_owned(), *added, *deleted))
            .collect::<Vec<_>>()
    };
    assert_eq!(parsed(""), vec![]);
    assert_eq!(
        parsed(&["0\t2\tsrc/b.ts", "2\t1\ta.txt", ""].join("\0")),
        files(&[("a.txt", 2, 1), ("src/b.ts", 0, 2)])
    );
    assert_eq!(
        parsed(
            &[
                "0\t0\t",
                "src/old.ts",
                "src/new.ts",
                "2\t1\t",
                "src/source.ts",
                "src/copied.ts",
                "1\t0\tother.ts",
                "",
            ]
            .join("\0")
        ),
        files(&[
            ("other.ts", 1, 0),
            ("src/copied.ts", 2, 1),
            ("src/new.ts", 0, 0)
        ])
    );
    assert_eq!(
        parsed(&["-\t-\timage.png", "0\t0\tempty.txt", ""].join("\0")),
        files(&[("empty.txt", 0, 0), ("image.png", 0, 0)])
    );
    let path = " café\tline\r\nname.txt ";
    assert_eq!(
        parsed(&format!("3\t2\t\0old\tname\n.txt\0{path}\0")),
        files(&[(path, 3, 2)])
    );
    assert_eq!(parsed(&format!("1\t0\t{path}\0")), files(&[(path, 1, 0)]));
}

// `localeCompare` order, as Node prints it for these paths.
#[test]
fn numstat_summaries_sort_like_locale_compare() {
    let names = [
        "B.txt", "a.txt", "_c.txt", "src/b.ts", "src-a.ts", "src.ts", "A.txt", "10.txt", "9.txt",
    ];
    let numstat: String = names.iter().map(|name| format!("1\t0\t{name}\0")).collect();
    let sorted: Vec<_> = turn_diff_files(&numstat)
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    assert_eq!(
        sorted,
        [
            "_c.txt", "10.txt", "9.txt", "a.txt", "A.txt", "B.txt", "src-a.ts", "src.ts",
            "src/b.ts"
        ]
    );
}

#[tokio::test]
async fn checkpoint_files_summarize_the_changes_between_two_refs() {
    let dir = repo_with_commit().await;
    let cwd = dir.path();
    let store = Checkpoints::default();
    store.capture(cwd, FROM).await.unwrap();
    std::fs::write(cwd.join("README.md"), "# changed\nmore\n").unwrap();
    std::fs::write(cwd.join("B.bin"), [0u8, 1, 2, 0]).unwrap();
    std::fs::write(cwd.join("added.txt"), "one\ntwo\n").unwrap();
    store.capture(cwd, TO).await.unwrap();
    let file = |path: &str, additions, deletions| agent_domain::CheckpointFile {
        path: path.into(),
        kind: "modified".into(),
        additions,
        deletions,
    };
    assert_eq!(
        store.files(cwd, FROM, TO).await.unwrap(),
        [
            file("added.txt", 2, 0),
            file("B.bin", 0, 0),
            file("README.md", 2, 1)
        ]
    );
    assert!(store.files(cwd, FROM, FROM).await.unwrap().is_empty());
    assert!(store.files(cwd, "refs/t3/test/missing", TO).await.is_err());
}

#[tokio::test]
async fn restore_recovers_files_and_removes_later_untracked_files_without_changing_head_or_index() {
    let dir = repo_with_commit().await;
    let cwd = dir.path();
    std::fs::write(cwd.join("tracked"), "before\n").unwrap();
    std::fs::write(cwd.join(".gitignore"), "ignored\n").unwrap();
    git(cwd, &["add", "."], None).await.unwrap();
    git(cwd, &["commit", "-m", "tracked"], None).await.unwrap();
    std::fs::write(cwd.join("tracked"), "staged\n").unwrap();
    git(cwd, &["add", "tracked"], None).await.unwrap();
    std::fs::write(cwd.join("tracked"), "checkpoint\n").unwrap();
    std::fs::write(cwd.join("untracked"), "saved\n").unwrap();
    let store = Checkpoints::default();
    store.capture(cwd, TO).await.unwrap();
    let index = std::fs::read(cwd.join(".git/index")).unwrap();
    let head = text(cwd, &["rev-parse", "HEAD"], None).await.unwrap();
    std::fs::write(cwd.join("tracked"), "later\n").unwrap();
    std::fs::remove_file(cwd.join("untracked")).unwrap();
    std::fs::write(cwd.join("new"), "later\n").unwrap();
    std::fs::write(cwd.join("ignored"), "keep\n").unwrap();
    store
        .prepare_restore(cwd, TO)
        .await
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join("tracked")).unwrap(),
        "checkpoint\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("untracked")).unwrap(),
        "saved\n"
    );
    assert!(!cwd.join("new").exists());
    assert_eq!(
        std::fs::read_to_string(cwd.join("ignored")).unwrap(),
        "keep\n"
    );
    assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
    assert_eq!(text(cwd, &["rev-parse", "HEAD"], None).await.unwrap(), head);
    assert_eq!(
        text(cwd, &["show", ":tracked"], None).await.unwrap(),
        "staged"
    );
}

#[tokio::test]
async fn a_nested_workspace_captures_and_restores_only_its_own_files() {
    let dir = repo_with_commit().await;
    let cwd = dir.path();
    std::fs::create_dir(cwd.join("scope")).unwrap();
    std::fs::write(cwd.join("scope/file"), "before\n").unwrap();
    std::fs::write(cwd.join("sibling"), "original\n").unwrap();
    git(cwd, &["add", "."], None).await.unwrap();
    git(cwd, &["commit", "-m", "scope"], None).await.unwrap();
    std::fs::write(cwd.join("scope/file"), "inside\n").unwrap();
    std::fs::write(cwd.join("sibling"), "outside\n").unwrap();
    let store = Checkpoints::default();
    let scope = cwd.join("scope");
    store.capture(&scope, TO).await.unwrap();
    assert_eq!(
        text(cwd, &["show", &format!("{TO}:scope/file")], None)
            .await
            .unwrap(),
        "inside"
    );
    assert_eq!(
        text(cwd, &["show", &format!("{TO}:sibling")], None)
            .await
            .unwrap(),
        "original"
    );
    std::fs::write(cwd.join("scope/file"), "later\n").unwrap();
    std::fs::write(cwd.join("scope/new"), "remove\n").unwrap();
    store
        .prepare_restore(&scope, TO)
        .await
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join("scope/file")).unwrap(),
        "inside\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("sibling")).unwrap(),
        "outside\n"
    );
    assert!(!cwd.join("scope/new").exists());
}

#[tokio::test]
async fn an_undone_or_interrupted_restore_puts_the_original_files_back() {
    let dir = repo().await;
    let cwd = dir.path();
    std::fs::write(cwd.join("file"), "checkpoint").unwrap();
    let store = Checkpoints::default();
    store.capture(cwd, TO).await.unwrap();
    std::fs::write(cwd.join("file"), "later").unwrap();
    std::fs::write(cwd.join("new"), "keep on failure").unwrap();
    let restored = store.prepare_restore(cwd, TO).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join("file")).unwrap(),
        "checkpoint"
    );
    restored.undo().unwrap();
    assert_eq!(std::fs::read_to_string(cwd.join("file")).unwrap(), "later");
    assert!(cwd.join("new").exists());
    drop(store.prepare_restore(cwd, TO).await.unwrap());
    assert_eq!(std::fs::read_to_string(cwd.join("file")).unwrap(), "later");
    let mut restored = store.prepare_restore(cwd, TO).await.unwrap();
    // Keep the journal as if the process exited without destructors.
    restored.committed = true;
    drop(restored);
    drop(store.prepare_restore(cwd, TO).await.unwrap());
    assert_eq!(std::fs::read_to_string(cwd.join("file")).unwrap(), "later");
    assert!(cwd.join("new").exists());
}

#[tokio::test]
async fn restore_rejects_path_collisions_before_mutating_any_files() {
    let dir = repo().await;
    let cwd = dir.path();
    std::fs::write(cwd.join("a"), "checkpoint").unwrap();
    std::fs::write(cwd.join("z"), "checkpoint").unwrap();
    let store = Checkpoints::default();
    store.capture(cwd, TO).await.unwrap();
    std::fs::write(cwd.join("a"), "later").unwrap();
    std::fs::remove_file(cwd.join("z")).unwrap();
    std::fs::create_dir(cwd.join("z")).unwrap();
    std::fs::write(cwd.join("z/keep"), "keep").unwrap();
    assert!(store.prepare_restore(cwd, TO).await.is_err());
    assert_eq!(std::fs::read_to_string(cwd.join("a")).unwrap(), "later");
    assert!(cwd.join("z/keep").exists());
}

#[tokio::test]
async fn references_can_be_checked_deleted_and_captured_again() {
    let dir = repo().await;
    let cwd = dir.path();
    std::fs::write(cwd.join("file"), "first\n").unwrap();
    let store = Checkpoints::default();
    assert!(!store.has(cwd, TO).await.unwrap());
    store.capture(cwd, TO).await.unwrap();
    assert!(store.has(cwd, TO).await.unwrap());
    store
        .delete(cwd, &[TO.to_owned(), "refs/t3/test/missing".into()])
        .await
        .unwrap();
    assert!(!store.has(cwd, TO).await.unwrap());
    std::fs::write(cwd.join("file"), "second\n").unwrap();
    store.capture(cwd, TO).await.unwrap();
    assert_eq!(
        text(cwd, &["show", &format!("{TO}:file")], None)
            .await
            .unwrap(),
        "second"
    );
}

#[tokio::test]
async fn unborn_repositories_capture_and_other_directories_refuse() {
    let dir = repo().await;
    let store = Checkpoints::default();
    std::fs::write(dir.path().join("new"), "new\n").unwrap();
    store.capture(dir.path(), TO).await.unwrap();
    assert!(!dir.path().join(".git/index").exists());
    assert!(commit_of(dir.path(), "HEAD").await.unwrap().is_none());
    let plain = tempfile::tempdir().unwrap();
    assert!(store.capture(plain.path(), TO).await.is_err());
}

#[tokio::test]
async fn sparse_capture_preserves_excluded_files_and_refuses_non_cone() {
    let dir = repo_with_commit().await;
    let cwd = dir.path();
    std::fs::create_dir(cwd.join("included")).unwrap();
    std::fs::create_dir(cwd.join("excluded")).unwrap();
    std::fs::write(cwd.join("included/file"), "in\n").unwrap();
    std::fs::write(cwd.join("excluded/file"), "out\n").unwrap();
    git(cwd, &["add", "."], None).await.unwrap();
    git(cwd, &["commit", "-m", "sparse"], None).await.unwrap();
    git(
        cwd,
        &[
            "sparse-checkout",
            "set",
            "--cone",
            "--sparse-index",
            "included",
        ],
        None,
    )
    .await
    .unwrap();
    assert!(!cwd.join("excluded/file").exists());
    let index = std::fs::read(cwd.join(".git/index")).unwrap();
    let store = Checkpoints::default();
    std::fs::write(cwd.join("included/file"), "changed\n").unwrap();
    store.capture(cwd, TO).await.unwrap();
    assert_eq!(
        text(cwd, &["show", &format!("{TO}:excluded/file")], None)
            .await
            .unwrap(),
        "out"
    );
    assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
    git(
        cwd,
        &["sparse-checkout", "set", "--no-cone", "/included/"],
        None,
    )
    .await
    .unwrap();
    assert!(store.capture(cwd, "refs/t3/test/turn/2").await.is_err());
}
