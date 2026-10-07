use super::*;

fn run(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(cwd: &Path, path: &str, text: &str) {
    let path = cwd.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A repository with one commit of `README.md`; returns its branch.
fn repository_with_commit(cwd: &Path) -> String {
    run(cwd, &["init", "--quiet"]);
    run(cwd, &["config", "user.email", "test@test.com"]);
    run(cwd, &["config", "user.name", "Test"]);
    run(cwd, &["config", "commit.gpgsign", "false"]);
    write(cwd, "README.md", "# test\n");
    run(cwd, &["add", "."]);
    run(cwd, &["commit", "--quiet", "-m", "initial commit"]);
    run(cwd, &["branch", "--show-current"])
}

fn request(cwd: &Path, base: Option<&str>) -> DiffPreview {
    DiffPreview {
        cwd: cwd.to_string_lossy().into_owned(),
        base_ref: base.map(str::to_owned),
        ignore_whitespace: false,
        file: None,
    }
}

fn files(result: &DiffPreviewResult, kind: DiffSourceKind) -> Vec<(String, u64, u64)> {
    result
        .sources
        .iter()
        .find(|source| source.kind == kind)
        .unwrap()
        .files
        .as_ref()
        .unwrap()
        .iter()
        .map(|file| (file.path.clone(), file.additions, file.deletions))
        .collect()
}

fn changes(result: &DiffPreviewResult) -> &DiffSource {
    result
        .sources
        .iter()
        .find(|source| source.kind == DiffSourceKind::BranchRange)
        .unwrap()
}

fn file(path: &str, additions: u64) -> (String, u64, u64) {
    (path.into(), additions, 0)
}

// GitVcsDriverCore.test.ts "Changes combines commits, uncommitted edits, and
// untracked files".
#[test]
fn changes_combine_commits_uncommitted_edits_and_untracked_files() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    run(cwd, &["checkout", "--quiet", "-b", "feature/combined"]);
    write(cwd, "README.md", "# test\ncommitted\n");
    run(cwd, &["commit", "--quiet", "-am", "commit edit"]);
    let clean = preview(&request(cwd, Some(&initial))).unwrap();
    assert_eq!(files(&clean, DiffSourceKind::WorkingTree), []);
    assert_eq!(
        files(&clean, DiffSourceKind::BranchRange),
        [file("README.md", 1)]
    );
    assert_eq!(changes(&clean).title, format!("Changes vs {initial}"));
    assert_eq!(
        changes(&clean).head_ref.as_deref(),
        Some("feature/combined")
    );

    write(cwd, "README.md", "# test\ncommitted\ndirty\n");
    write(cwd, "new.txt", "new\n");
    let dirty = preview(&request(cwd, Some(&initial))).unwrap();
    assert_eq!(
        files(&dirty, DiffSourceKind::BranchRange),
        [file("README.md", 2), file("new.txt", 1)]
    );
    assert_eq!(
        changes(&dirty)
            .diff
            .matches("diff --git a/README.md ")
            .count(),
        1
    );
    assert!(changes(&dirty).diff.contains("+dirty"));
    assert_eq!(
        files(&dirty, DiffSourceKind::WorkingTree),
        [file("README.md", 1), file("new.txt", 1)]
    );
    // Untracked files are added to a copy of the index only.
    assert!(run(cwd, &["status", "--porcelain"]).contains("?? new.txt"));
    assert_ne!(changes(&clean).diff_hash, changes(&dirty).diff_hash);
}

// "Changes on the default branch compares with its remote copy".
#[test]
fn changes_on_the_default_branch_compare_with_its_remote_copy() {
    let directory = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    repository_with_commit(cwd);
    run(cwd, &["branch", "-M", "main"]);
    write(cwd, "unpushed.txt", "unpushed\n");
    run(cwd, &["add", "unpushed.txt"]);
    run(cwd, &["commit", "--quiet", "-m", "unpushed"]);
    let local = preview(&request(cwd, None)).unwrap();
    assert_eq!(changes(&local).base_ref, None);
    assert_eq!(files(&local, DiffSourceKind::BranchRange), []);

    run(remote.path(), &["init", "--quiet", "--bare"]);
    run(
        cwd,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    run(
        cwd,
        &["push", "--quiet", "origin", "HEAD~1:refs/heads/main"],
    );
    run(cwd, &["fetch", "--quiet", "origin"]);
    let pushed = preview(&request(cwd, None)).unwrap();
    assert_eq!(changes(&pushed).base_ref.as_deref(), Some("origin/main"));
    assert_eq!(
        files(&pushed, DiffSourceKind::BranchRange),
        [file("unpushed.txt", 1)]
    );
}

// "Changes accepts an explicit base on a detached HEAD and rejects a bad one".
#[test]
fn changes_take_an_explicit_base_on_a_detached_head_and_reject_a_bad_one() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    run(cwd, &["checkout", "--quiet", "--detach"]);
    write(cwd, "detached.txt", "detached\n");
    run(cwd, &["add", "detached.txt"]);
    run(cwd, &["commit", "--quiet", "-m", "detached work"]);
    let explicit = preview(&request(cwd, Some(&initial))).unwrap();
    assert_eq!(
        files(&explicit, DiffSourceKind::BranchRange),
        [file("detached.txt", 1)]
    );
    let implicit = preview(&request(cwd, None)).unwrap();
    assert_eq!(files(&implicit, DiffSourceKind::BranchRange), []);
    assert_eq!(changes(&implicit).head_ref.as_deref(), Some("HEAD"));
    let error = preview(&request(cwd, Some("missing-base"))).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Could not find a common commit between 'missing-base' and HEAD."
    );
}

// "Changes compares with the empty tree before the first commit".
#[test]
fn changes_before_the_first_commit_compare_with_the_empty_tree() {
    let upstream = tempfile::tempdir().unwrap();
    repository_with_commit(upstream.path());
    run(upstream.path(), &["branch", "-M", "main"]);
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    run(cwd, &["init", "--quiet", "-b", "main"]);
    run(
        cwd,
        &[
            "remote",
            "add",
            "origin",
            &upstream.path().to_string_lossy(),
        ],
    );
    run(cwd, &["fetch", "--quiet", "origin"]);
    write(cwd, "new.txt", "one\ntwo\n");
    let result = preview(&request(cwd, None)).unwrap();
    assert_eq!(changes(&result).base_ref, None);
    assert_eq!(
        files(&result, DiffSourceKind::BranchRange),
        [file("new.txt", 2)]
    );
    assert_eq!(
        status(cwd).unwrap().branch_changes,
        Some(BranchChanges {
            base_ref: None,
            insertions: 2,
            deletions: 0
        })
    );
}

// "Changes does not show newer base commits as deletions".
#[test]
fn changes_do_not_show_newer_base_commits_as_deletions() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    run(cwd, &["checkout", "--quiet", "-b", "feature/rebased"]);
    write(cwd, "feature.txt", "feature\n");
    run(cwd, &["add", "feature.txt"]);
    run(cwd, &["commit", "--quiet", "-m", "feature work"]);
    run(cwd, &["checkout", "--quiet", &initial]);
    write(cwd, "upstream.txt", "upstream\n");
    run(cwd, &["add", "upstream.txt"]);
    run(cwd, &["commit", "--quiet", "-m", "base moves"]);
    run(cwd, &["checkout", "--quiet", "feature/rebased"]);
    for rebase in [false, true] {
        if rebase {
            run(cwd, &["rebase", "--quiet", &initial]);
        }
        let result = preview(&request(cwd, Some(&initial))).unwrap();
        assert_eq!(
            files(&result, DiffSourceKind::BranchRange),
            [file("feature.txt", 1)]
        );
    }
}

// "honors whitespace filtering for worktree and branch previews" and the
// per-file request of "loads repository-relative files from a nested project
// directory".
#[test]
fn whitespace_and_single_file_requests() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    write(cwd, "README.md", "#   test\n");
    let ignoring = preview(&DiffPreview {
        ignore_whitespace: true,
        ..request(cwd, Some(&initial))
    })
    .unwrap();
    for source in &ignoring.sources {
        assert_eq!(source.diff, "", "{:?}", source.kind);
        assert_eq!(source.files.as_deref(), Some(&[][..]), "{:?}", source.kind);
    }
    write(cwd, "nested/other.txt", "other\n");
    let single = preview(&DiffPreview {
        file: Some(agent_protocol::workspace::DiffPreviewFile {
            path: "README.md".into(),
            previous_path: None,
            source: DiffSourceKind::WorkingTree,
        }),
        ..request(&cwd.join("nested"), None)
    })
    .unwrap();
    assert_eq!(
        files(&single, DiffSourceKind::WorkingTree),
        [("README.md".to_owned(), 1, 1)]
    );
    assert!(single.sources[0].diff.contains("b/README.md"));
    assert_eq!(files(&single, DiffSourceKind::BranchRange), []);
}

// "reads Changes totals with untracked files when requested" and "reports
// refName and dirty state for a repository".
#[test]
fn status_reports_the_branch_and_its_changes() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    repository_with_commit(cwd);
    run(cwd, &["branch", "-M", "main"]);
    run(cwd, &["checkout", "--quiet", "-b", "feature/totals"]);
    write(cwd, "README.md", "# test\ncommitted\n");
    run(cwd, &["commit", "--quiet", "-am", "commit edit"]);
    write(cwd, "nested/untracked.txt", "one\ntwo\n");
    let read = status(&cwd.join("nested")).unwrap();
    assert!(read.is_repo && read.has_working_tree_changes);
    assert_eq!(read.ref_name.as_deref(), Some("feature/totals"));
    assert!(!read.is_default_ref);
    assert_eq!(
        read.branch_changes,
        Some(BranchChanges {
            base_ref: Some("main".into()),
            insertions: 3,
            deletions: 0
        })
    );
    let plain = tempfile::tempdir().unwrap();
    assert!(!status(plain.path()).unwrap().is_repo);
}

#[test]
fn status_counts_a_rename_under_its_new_path() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    repository_with_commit(cwd);
    run(cwd, &["mv", "README.md", "RENAMED.md"]);
    let read = status(cwd).unwrap();
    let paths: Vec<&str> = read
        .working_tree
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["RENAMED.md"]);
}

// "reports non-repository directories without failing", "optionally includes
// remote refs that match local branches" and "marks the origin default ref as
// default when no local copy exists".
#[test]
fn refs_list_local_and_remote_branches_for_the_base_picker() {
    let plain = tempfile::tempdir().unwrap();
    let all = |cwd: &Path, include: bool, kind: RefKind, limit: Option<u32>| {
        list_refs(&ListRefs {
            cwd: cwd.to_string_lossy().into_owned(),
            query: None,
            cursor: None,
            include_matching_remote_refs: include,
            ref_kind: kind,
            limit,
        })
        .unwrap()
    };
    let none = all(plain.path(), false, RefKind::All, None);
    assert!(!none.is_repo && none.refs.is_empty());

    let directory = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    run(remote.path(), &["init", "--quiet", "--bare"]);
    run(
        cwd,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    run(cwd, &["push", "--quiet", "-u", "origin", &initial]);
    let names = |list: &RefList| list.refs.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    let deduplicated = all(cwd, false, RefKind::All, None);
    assert!(!names(&deduplicated).contains(&format!("origin/{initial}")));
    assert!(deduplicated.refs[0].current);
    let complete = all(cwd, true, RefKind::All, None);
    assert!(names(&complete).contains(&initial));
    assert!(names(&complete).contains(&format!("origin/{initial}")));
    let remote_only = all(cwd, true, RefKind::Remote, Some(1));
    assert_eq!(names(&remote_only), [format!("origin/{initial}")]);
    assert!(remote_only.refs[0].is_remote);

    run(cwd, &["remote", "set-head", "origin", &initial]);
    run(cwd, &["checkout", "--quiet", "-b", "feature/only-local"]);
    run(cwd, &["branch", "--quiet", "-D", &initial]);
    let refs = all(cwd, false, RefKind::All, None);
    let default = refs
        .refs
        .iter()
        .find(|entry| entry.name == format!("origin/{initial}"))
        .unwrap();
    assert!(default.is_remote && default.is_default);
    assert!(refs.has_primary_remote);
}

// GitVcsDriverCore switchRef: a local branch, a remote branch tracked by a new
// local branch, and the local branch already tracking it.
#[test]
fn switching_refs_checks_out_local_and_remote_branches() {
    let directory = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let cwd = directory.path();
    let initial = repository_with_commit(cwd);
    run(remote.path(), &["init", "--quiet", "--bare"]);
    run(
        cwd,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    run(cwd, &["checkout", "--quiet", "-b", "feature/remote"]);
    run(cwd, &["push", "--quiet", "origin", "feature/remote"]);
    run(cwd, &["checkout", "--quiet", &initial]);
    run(cwd, &["branch", "--quiet", "-D", "feature/remote"]);
    run(cwd, &["branch", "--quiet", "local-only"]);
    let switch = |name: &str| {
        checkout(&SwitchRef {
            cwd: cwd.to_string_lossy().into_owned(),
            ref_name: name.into(),
        })
    };
    assert_eq!(
        switch("local-only").unwrap().ref_name.as_deref(),
        Some("local-only")
    );
    assert_eq!(
        switch("origin/feature/remote").unwrap().ref_name.as_deref(),
        Some("feature/remote")
    );
    run(cwd, &["checkout", "--quiet", &initial]);
    assert_eq!(
        switch("origin/feature/remote").unwrap().ref_name.as_deref(),
        Some("feature/remote")
    );
    let missing = switch("no-such-branch").unwrap_err().to_string();
    assert!(missing.ends_with("git checkout failed"), "{missing}");
}
