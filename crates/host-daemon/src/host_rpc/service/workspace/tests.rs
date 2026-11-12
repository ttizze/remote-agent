use super::*;

fn service(directory: &Path) -> HostRpcService {
    HostRpcService::new(
        [],
        crate::ProjectStore::new(directory.join("projects.json")),
    )
}

async fn change(
    service: &HostRpcService,
    cwd: &Path,
    action: WorkspaceAction,
) -> Result<Empty, Failure> {
    service
        .change_workspace(&ChangeWorkspace {
            cwd: cwd.to_string_lossy().into_owned(),
            branch: String::new(),
            action,
        })
        .await
}

fn save(previous: Option<&str>, name: &str, command: &str) -> WorkspaceAction {
    WorkspaceAction::SaveScript {
        previous_name: previous.map(str::to_owned),
        name: name.into(),
        command: command.into(),
    }
}

async fn scripts(service: &HostRpcService, cwd: &Path) -> Vec<ProjectScript> {
    service
        .review_workspace(&ReviewWorkspace {
            cwd: cwd.to_string_lossy().into_owned(),
            base_ref: None,
        })
        .await
        .unwrap()
        .controls
        .scripts
}

fn git(cwd: &Path, args: &[&str]) {
    crate::git::output(cwd, args).unwrap();
}

#[tokio::test]
async fn project_scripts_share_worktrees_and_persist_renames_without_touching_files() {
    let directory = tempfile::tempdir().unwrap();
    let repo = directory.path().join("repo");
    let linked = directory.path().join("linked");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let first = service(directory.path());
    change(&first, &repo, save(None, " Build ", " echo build "))
        .await
        .unwrap();
    assert_eq!(
        scripts(&first, &linked).await,
        [ProjectScript {
            name: "Build".into(),
            command: "echo build".into()
        }]
    );
    change(&first, &linked, save(Some("Build"), "Check", "echo check"))
        .await
        .unwrap();
    assert!(
        change(&first, &repo, save(Some("Build"), "Stale", "echo stale"))
            .await
            .is_err()
    );
    assert!(
        change(&first, &repo, save(None, "Check", "echo duplicate"))
            .await
            .is_err()
    );
    let expected = scripts(&first, &repo).await;
    assert_eq!(
        expected,
        [ProjectScript {
            name: "Check".into(),
            command: "echo check".into()
        }]
    );
    drop(first);
    let reopened = service(directory.path());
    assert_eq!(scripts(&reopened, &linked).await, expected);
    change(
        &reopened,
        &repo,
        WorkspaceAction::RemoveScript {
            name: "Check".into(),
        },
    )
    .await
    .unwrap();
    assert!(scripts(&reopened, &linked).await.is_empty());
    assert!(
        crate::git::text(&repo, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::git::text(&linked, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn plain_folder_scripts_are_isolated_and_failed_writes_keep_the_previous_script() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let service = service(directory.path());
    change(&service, &first, save(None, "Run", "echo one"))
        .await
        .unwrap();
    assert!(scripts(&service, &second).await.is_empty());
    service.inner.store.reject_state_writes(true);
    assert!(
        change(&service, &first, save(Some("Run"), "Changed", "echo two"))
            .await
            .is_err()
    );
    assert!(
        change(
            &service,
            &first,
            WorkspaceAction::RemoveScript { name: "Run".into() }
        )
        .await
        .is_err()
    );
    assert_eq!(
        scripts(&service, &first).await,
        [ProjectScript {
            name: "Run".into(),
            command: "echo one".into()
        }]
    );
    service.inner.store.reject_state_writes(false);
    let (a, b) = tokio::join!(
        change(&service, &second, save(None, "A", "echo a")),
        change(&service, &second, save(None, "B", "echo b"))
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(scripts(&service, &second).await.len(), 2);
    assert!(std::fs::read_dir(&first).unwrap().next().is_none());
}

#[tokio::test]
async fn script_validation_and_corrupt_storage_fail_without_replacing_saved_data() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    for (name, command) in [
        ("", "echo a"),
        ("a\nb", "echo a"),
        ("a", " "),
        ("a", "a\0b"),
    ] {
        assert!(
            change(&service, directory.path(), save(None, name, command))
                .await
                .is_err()
        );
    }
    assert!(validate_script("a".repeat(121), "echo a".into()).is_err());
    assert!(validate_script("a".into(), "x".repeat(16385)).is_err());
    assert!(validate_script("a".repeat(120), "x".repeat(16384)).is_ok());
    let key = script_key(directory.path()).unwrap();
    service
        .inner
        .store
        .save_state(&key, &vec!["invalid record"])
        .unwrap();
    assert!(
        service
            .review_workspace(&ReviewWorkspace {
                cwd: directory.path().to_string_lossy().into_owned(),
                base_ref: None
            })
            .await
            .is_err()
    );
    assert!(
        change(&service, directory.path(), save(None, "valid", "echo a"))
            .await
            .is_err()
    );
    assert_eq!(
        service.inner.store.load_state::<Vec<String>>(&key).unwrap(),
        ["invalid record"]
    );
}
