use super::*;
use agent_protocol::session::{ProviderKind, SessionRef};

#[tokio::test]
async fn update_without_an_installed_codex_records_no_interrupted_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let codex = crate::providers::codex::Codex::new(Err("not installed".into()));
    let service = HostRpcService::new([codex.into()], directory.path().join("projects.json"));
    service.set_history_scope(directory.path().to_owned(), true);
    service.prepare_update().await.unwrap();
    let saved: Recovery =
        serde_json::from_slice(&std::fs::read(directory.path().join(RECOVERY_FILE)).unwrap())
            .unwrap();
    assert!(saved.tasks.is_empty());
}

#[tokio::test]
async fn an_uncertain_update_continuation_is_reported_without_resending() {
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("bex-worktrees.json"));
    let target = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let path = directory.path().join(RECOVERY_FILE);
    agent_local_host::save_private_json(
        &path,
        &Recovery {
            restart_marker: None,
            tasks: vec![InterruptedTask {
                native_session: SessionRef::new(ProviderKind::Codex, "native".into()).unwrap(),
                cwd: None,
                submission: op::Submission {
                    thread_id: target.clone(),
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    input: vec![],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                last_turn: None,
                delivery: ContinuationDelivery::Sending,
            }],
        },
    )
    .unwrap();
    service.resume_after_update(directory.path()).await.unwrap();
    let saved = std::fs::read(&path).unwrap();
    let report: Recovery = serde_json::from_slice(&saved).unwrap();
    assert!(
        matches!(&report.tasks[0].delivery, ContinuationDelivery::Failed(message)
        if message == "継続の配送結果を確認できません。会話を開いて確認してください。")
    );
    assert!(
        service
            .inner
            .conversation
            .storage
            .thread_header(&target)
            .unwrap()
            .is_none()
    );
    service.resume_after_update(directory.path()).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), saved);
}

#[tokio::test]
async fn a_report_write_failure_rolls_back_both_update_gates_and_preserves_existing_contents() {
    let directory = tempfile::tempdir().unwrap();
    let reports = directory.path().join("reports");
    let path = reports.join(RECOVERY_FILE);
    std::fs::create_dir_all(&path).unwrap();
    let marker = path.join("keep");
    std::fs::write(&marker, b"existing contents").unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    service.set_history_scope(reports, true);
    assert!(service.prepare_update().await.is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), b"existing contents");
    assert!(
        !service
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(!*service.inner.conversation.update_report.lock().await);
}

#[test]
fn an_update_restart_marker_is_consumed_once_and_expires_after_one_minute() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(RECOVERY_FILE);
    let now = crate::host_rpc::runtime::now().max(0) as u64;
    for (marker, expected) in [(now, true), (now.saturating_sub(60_000), false)] {
        agent_local_host::save_private_json(
            &path,
            &Recovery {
                tasks: vec![],
                restart_marker: Some(marker),
            },
        )
        .unwrap();
        assert_eq!(consume_restart_marker(&path).unwrap(), expected);
        assert!(!consume_restart_marker(&path).unwrap());
        let report: Recovery = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(report.restart_marker.is_none());
    }
}

#[tokio::test]
async fn only_the_actual_continuation_provider_turn_confirms_restart_delivery() {
    use crate::host_rpc::runtime::{native_fixture::NativeAgent, t3_contract::running};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (target, _, _) = running(&service, &agent, directory.path().to_str().unwrap()).await;
    let mut state = service
        .inner
        .conversation
        .storage
        .thread_state(&target.application_id(), &[])
        .unwrap();
    let mut continuation = state.active_run().unwrap().clone();
    let source = continuation.id.clone();
    state.runs.get_mut(&source).unwrap().status = model::RunStatus::Interrupted;
    assert_eq!(
        continuation_delivery(&state, &source),
        ContinuationDelivery::Completed
    );
    continuation.id = "continuation".into();
    continuation.restart_continuation_of_run_id = Some(source.clone());
    continuation.active_attempt_id = Some("continuation-attempt".into());
    state
        .runs
        .insert(continuation.id.clone(), continuation.clone());
    assert_eq!(
        continuation_delivery(&state, &source),
        ContinuationDelivery::Pending
    );
    let mut attempt = state.run_attempts.values().next().unwrap().clone();
    attempt.id = "continuation-attempt".into();
    attempt.run_id = continuation.id.clone();
    attempt.provider_turn_id = None;
    state
        .run_attempts
        .insert(attempt.id.clone(), attempt.clone());
    assert_eq!(
        continuation_delivery(&state, &source),
        ContinuationDelivery::Pending
    );
    attempt.provider_turn_id = Some("actual-continuation-turn".into());
    state.run_attempts.insert(attempt.id.clone(), attempt);
    assert_eq!(
        continuation_delivery(&state, &source),
        ContinuationDelivery::Accepted(continuation.id.into())
    );
}
