use super::*;

fn clone_request(repository: &Path, destination: &Path) -> CloneProject {
    CloneProject {
        id: uuid::Uuid::new_v4().to_string(),
        repository: repository.to_str().unwrap().into(),
        destination: destination.to_str().unwrap().into(),
    }
}
fn repository(root: &Path) -> PathBuf {
    let path = root.join("source");
    std::fs::create_dir(&path).unwrap();
    for args in [
        vec!["init"],
        vec!["add", "file.txt"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-m",
            "Fixture",
        ],
    ] {
        std::fs::write(path.join("file.txt"), "cloned contents").unwrap();
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    path
}
async fn terminal(owner: &Clones, id: &str) -> ProjectCloneState {
    let mut changes = owner.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let state = owner
                .read()
                .await
                .unwrap()
                .clones
                .into_iter()
                .find(|entry| entry.request.id == id)
                .unwrap();
            if !state.phase.active() {
                return state;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("clone finishes")
}
#[tokio::test]
async fn clone_survives_requester_drop_registers_once_and_can_be_dismissed() {
    let directory = tempfile::tempdir().unwrap();
    let source = repository(directory.path());
    let projects = ProjectStore::new(directory.path().join("worktrees.json"));
    let owner = Clones::new(projects.clone());
    let request = clone_request(&source, &directory.path().join("checkout"));
    let started = owner.start(request.clone()).await.unwrap();
    assert_eq!(started.clones.len(), 1);
    assert_eq!(owner.start(request.clone()).await.unwrap().clones.len(), 1);
    let finished = terminal(&owner, &request.id).await;
    assert_eq!(
        finished.phase,
        ProjectClonePhase::Completed,
        "{:?}",
        finished.error
    );
    assert!(Path::new(&request.destination).join(".git").is_dir());
    assert_eq!(
        std::fs::read_to_string(Path::new(&request.destination).join("file.txt")).unwrap(),
        "cloned contents"
    );
    assert_eq!(projects.load().await.unwrap().projects.len(), 1);
    let reopened = Clones::new(projects);
    assert_eq!(reopened.read().await.unwrap().clones[0], finished);
    let dismissed = reopened
        .change(ChangeProjectClone {
            id: request.id,
            action: ProjectCloneAction::Dismiss,
        })
        .await
        .unwrap();
    assert!(dismissed.clones.is_empty());
    assert!(Path::new(&request.destination).is_dir());
    owner.shutdown().await;
}
#[tokio::test]
async fn failed_clone_retries_without_replacing_existing_paths_or_duplicating_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let owner = Clones::new(ProjectStore::new(directory.path().join("worktrees.json")));
    let request = clone_request(&source, &directory.path().join("checkout"));
    owner.start(request.clone()).await.unwrap();
    let failed = terminal(&owner, &request.id).await;
    assert_eq!(failed.phase, ProjectClonePhase::Failed);
    assert!(failed.error.unwrap().starts_with("Git clone failed"));
    assert!(!Path::new(&request.destination).exists());
    repository(directory.path());
    let changed = owner
        .change(ChangeProjectClone {
            id: request.id.clone(),
            action: ProjectCloneAction::Retry,
        })
        .await
        .unwrap();
    assert_eq!(changed.clones.len(), 1);
    assert_eq!(
        terminal(&owner, &request.id).await.phase,
        ProjectClonePhase::Completed
    );
    let another = clone_request(&source, Path::new(&request.destination));
    assert!(owner.start(another).await.is_err());
    owner.shutdown().await;
}
#[tokio::test]
async fn cancellation_releases_the_process_and_restart_preserves_retry_state() {
    let directory = tempfile::tempdir().unwrap();
    let source = repository(directory.path());
    let projects = ProjectStore::new(directory.path().join("worktrees.json"));
    let owner = Clones::new(projects.clone());
    let request = clone_request(&source, &directory.path().join("checkout"));
    owner.start(request.clone()).await.unwrap();
    owner
        .change(ChangeProjectClone {
            id: request.id.clone(),
            action: ProjectCloneAction::Cancel,
        })
        .await
        .unwrap();
    assert_eq!(
        terminal(&owner, &request.id).await.phase,
        ProjectClonePhase::Cancelled
    );
    assert!(!Path::new(&request.destination).exists());
    owner.shutdown().await;
    // An interrupted Host leaves a durable active checkpoint, without a new job id.
    let saved = vec![Entry {
        state: ProjectCloneState {
            request: request.clone(),
            phase: ProjectClonePhase::Fetching,
            progress: None,
            error: None,
        },
        staging: None,
    }];
    agent_local_host::save_private_json(&owner.path(), &saved).unwrap();
    let restarted = Clones::new(projects);
    let state = restarted.read().await.unwrap();
    assert_eq!(state.clones[0].request.id, request.id);
    assert_eq!(state.clones[0].phase, ProjectClonePhase::Failed);
    restarted
        .change(ChangeProjectClone {
            id: request.id.clone(),
            action: ProjectCloneAction::Retry,
        })
        .await
        .unwrap();
    assert_eq!(
        terminal(&restarted, &request.id).await.phase,
        ProjectClonePhase::Completed
    );
    restarted.shutdown().await;
}
#[tokio::test]
async fn corrupt_ledger_and_unowned_staging_are_not_discarded() {
    let directory = tempfile::tempdir().unwrap();
    let owner = Clones::new(ProjectStore::new(directory.path().join("worktrees.json")));
    tokio::fs::write(owner.path(), b"invalid").await.unwrap();
    assert!(owner.read().await.is_err());
    assert_eq!(tokio::fs::read(owner.path()).await.unwrap(), b"invalid");
    tokio::fs::write(owner.path(), b"[]").await.unwrap();
    assert!(
        owner.read().await.unwrap().clones.is_empty(),
        "repairing the ledger recovers without restarting the Host"
    );
    let source = repository(directory.path());
    let staging = directory.path().join(".bex-clone-unowned");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("owner"), "someone else").unwrap();
    let mut entry = Entry {
        state: ProjectCloneState {
            request: clone_request(&source, &directory.path().join("checkout")),
            phase: ProjectClonePhase::Failed,
            progress: None,
            error: None,
        },
        staging: Some(staging.clone()),
    };
    assert!(process::cleanup(&mut entry).await.is_err());
    assert!(staging.join("owner").exists());
}

#[tokio::test]
async fn retry_after_registration_failure_keeps_the_acquired_checkout() {
    let directory = tempfile::tempdir().unwrap();
    let source = repository(directory.path());
    let projects = ProjectStore::new(directory.path().join("worktrees.json"));
    let owner = Clones::new(projects.clone());
    let request = clone_request(&source, &directory.path().join("checkout"));
    let registrations = directory.path().join("bex-projects.json");
    tokio::fs::write(&registrations, b"invalid").await.unwrap();
    owner.start(request.clone()).await.unwrap();
    assert_eq!(
        terminal(&owner, &request.id).await.phase,
        ProjectClonePhase::Failed
    );
    let file = Path::new(&request.destination).join("file.txt");
    tokio::fs::write(&file, b"preserved local edits")
        .await
        .unwrap();
    tokio::fs::write(&registrations, b"[]").await.unwrap();
    // Reopen the Host's ledger as well as retrying registration.
    owner.shutdown().await;
    let reopened = Clones::new(projects.clone());
    reopened
        .change(ChangeProjectClone {
            id: request.id.clone(),
            action: ProjectCloneAction::Retry,
        })
        .await
        .unwrap();
    assert_eq!(
        terminal(&reopened, &request.id).await.phase,
        ProjectClonePhase::Completed
    );
    assert_eq!(
        tokio::fs::read_to_string(file).await.unwrap(),
        "preserved local edits"
    );
    assert_eq!(projects.load().await.unwrap().projects.len(), 1);
    reopened.shutdown().await;
}
