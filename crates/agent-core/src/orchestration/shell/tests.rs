use super::*;
use std::sync::Arc;

fn thread(id: &str) -> ThreadShell {
    crate::test_conversations::shell_thread(crate::models::Thread {
        id: Some(crate::session::SessionRef::new(None, id.into()).unwrap()),
        name: Some(id.into()),
        ..Default::default()
    })
}
fn snapshot() -> ShellSnapshot {
    ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: 9,
        projects: vec![],
        threads: vec![Arc::new(thread("a")), Arc::new(thread("b"))],
        archived_threads: vec![],
    }
}
fn update(thread: ThreadShell, sequence: u64, location: ShellLocation) -> ShellStreamItem {
    ShellStreamItem::ThreadUpdated {
        thread: thread.into(),
        sequence,
        location,
    }
}
#[test]
fn equal_sequence_deltas_move_locations_without_losing_other_threads() {
    let previous = snapshot();
    let removed = receive(
        Some(&previous),
        ShellStreamItem::ThreadRemoved {
            sequence: 10,
            location: ShellLocation::Active,
            thread_id: "a".into(),
        },
        false,
    )
    .unwrap()
    .unwrap();
    let next = receive(
        Some(&removed),
        update(thread("a"), 10, ShellLocation::Archive),
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next.snapshot_sequence, 10);
    assert_eq!(next.threads.len(), 1);
    assert_eq!(next.archived_threads[0].thread.id, "a");
    assert!(Arc::ptr_eq(&next.threads[0], &previous.threads[1]));
    let moved = receive(
        Some(&next),
        update(thread("a"), 10, ShellLocation::Active),
        false,
    )
    .unwrap()
    .unwrap();
    assert!(moved.archived_threads.is_empty());
    assert_eq!(moved.threads.len(), 2);
}
#[test]
fn stale_deltas_and_identical_updates_reuse_saved_metadata() {
    let previous = snapshot();
    let mut stale = thread("a");
    stale.thread.title = "obsolete".into();
    let unchanged = receive(
        Some(&previous),
        update(stale, 8, ShellLocation::Archive),
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(unchanged, previous);
    let next = receive(
        Some(&previous),
        update(thread("a"), 9, ShellLocation::Active),
        false,
    )
    .unwrap()
    .unwrap();
    assert!(Arc::ptr_eq(&next.threads[0], &previous.threads[0]));
    assert!(Arc::ptr_eq(&next.threads[1], &previous.threads[1]));
}
#[test]
fn authoritative_bootstrap_can_reset_a_cursor_but_live_snapshots_cannot() {
    let previous = snapshot();
    let mut fresh = snapshot();
    fresh.snapshot_sequence = 1;
    fresh.threads.clear();
    let message = ShellStreamItem::Snapshot {
        snapshot: fresh.clone(),
        resolved_repository_identity_roots: None,
    };
    assert!(receive(Some(&previous), message.clone(), false).is_err());
    assert_eq!(
        receive(Some(&previous), message, true).unwrap(),
        Some(fresh)
    );
    assert_eq!(
        receive(Some(&previous), ShellStreamItem::SnapshotRequired, false).unwrap(),
        None
    );
    assert!(receive(None, update(thread("a"), 1, ShellLocation::Active), false).is_err());
}
#[test]
fn invalid_snapshots_and_empty_delta_ids_are_rejected_before_mutation() {
    let previous = snapshot();
    for case in 0..3 {
        let mut invalid = previous.clone();
        match case {
            0 => invalid.schema_version = 1,
            1 => invalid.archived_threads.push(invalid.threads[0].clone()),
            _ => Arc::make_mut(&mut invalid.threads[0]).thread.id.clear(),
        }
        assert!(
            receive(
                Some(&previous),
                ShellStreamItem::Snapshot {
                    snapshot: invalid,
                    resolved_repository_identity_roots: None
                },
                true
            )
            .is_err()
        );
    }
    let mut empty = thread("a");
    empty.thread.id.clear();
    assert!(
        receive(
            Some(&previous),
            update(empty, 10, ShellLocation::Active),
            false
        )
        .is_err()
    );
    for item in [
        ShellStreamItem::ThreadRemoved {
            sequence: 10,
            location: ShellLocation::Active,
            thread_id: String::new(),
        },
        ShellStreamItem::ProjectRemoved {
            sequence: 10,
            project_id: String::new(),
        },
    ] {
        assert!(receive(Some(&previous), item, false).is_err());
    }
    assert_eq!(previous.threads[0].thread.id, "a");
}
#[test]
fn native_public_addresses_round_trip_without_decoding_arbitrary_application_ids() {
    for provider in [
        crate::session::ProviderKind::Codex,
        crate::session::ProviderKind::Claude,
        crate::session::ProviderKind::DeepSeek,
    ] {
        let id =
            crate::session::SessionRef::new(provider, "日本語:raw%20value/child".into()).unwrap();
        assert_eq!(public_session_ref(&id.application_id()).unwrap(), id);
        let prefix = crate::session::SessionRef::new(provider, "probe".into())
            .unwrap()
            .application_id();
        let literal =
            crate::session::SessionRef::new(None, prefix.strip_suffix("probe").unwrap().into())
                .unwrap();
        assert_eq!(
            public_session_ref(&literal.application_id()).unwrap(),
            literal
        );
    }
    let id = crate::session::SessionRef::new(None, "bex:literal%20id".into()).unwrap();
    assert_eq!(public_session_ref(&id.application_id()).unwrap(), id);
}
#[test]
fn background_completion_waits_only_for_awaited_commands_agents_and_background_tasks() {
    use agent_protocol::live_activity::TaskState;
    let mut shell = thread("a");
    for (kind, awaited, holds) in [
        ("command", None, false),
        ("monitor", None, false),
        ("command", Some(true), true),
        ("subagent", None, true),
        ("background_task", Some(false), true),
    ] {
        shell.pending_background_tasks = vec![
            serde_json::from_value(
                serde_json::json!({"taskId":"task","kind":kind,"awaited":awaited}),
            )
            .unwrap(),
        ];
        assert_eq!(
            shell.task_state(),
            TaskState {
                status: agent_protocol::execution::SessionStatus::Idle,
                background: Some(holds),
                ..Default::default()
            }
        );
    }
}
#[test]
fn unread_uses_completion_time_and_excludes_rollbacks_and_subagent_notifications() {
    let mut shell = thread("a");
    shell.status = ShellStatus::Completed;
    shell.latest_run_id = Some("run".into());
    shell.latest_run_completed_at = Some("2026-11-14T01:00:00+01:00".into());
    assert!(shell.unread());
    shell.thread.last_visited_at = Some("2026-11-14T00:00:00Z".into());
    assert!(!shell.unread());
    shell.thread.last_visited_at = Some("2026-11-13T23:59:59Z".into());
    assert!(shell.unread());
    for status in [
        ShellStatus::RolledBack,
        ShellStatus::Failed,
        ShellStatus::Interrupted,
        ShellStatus::Cancelled,
    ] {
        shell.status = status;
        assert!(!shell.unread());
    }
    shell.status = ShellStatus::Completed;
    shell.thread.lineage.relationship_to_parent = Some("subagent".into());
    assert!(activity([&shell]).unwrap().unread.is_empty());
}

#[test]
fn compact_native_shell_retains_the_existing_one_kibibyte_wire_budget() {
    use agent_protocol::{
        orchestration as model,
        session::{ProviderKind, SessionRef},
    };
    let native = SessionRef::new(ProviderKind::Codex, "p5-18".into()).unwrap();
    let mut shell = crate::test_conversations::shell_thread(crate::models::Thread {
        id: Some(native),
        name: Some("Project 05 conversation 18".into()),
        cwd: Some("/private/var/folders/aa/fixture-user-cache/T/.tmpfixture/project-5".into()),
        project_id: crate::models::ProjectMembership::Assigned("project-5".into()),
        ..Default::default()
    });
    shell.thread.created_by = model::Actor::System;
    shell.thread.creation_source = model::CreationSource::Server;
    shell.thread.active_provider_thread_id = Some(model::derived_id(
        "provider-thread",
        &[
            "provider",
            "codex",
            "provider-instance",
            "codex",
            "native-thread",
            "p5-18",
        ],
    ));
    let bytes = agent_protocol::protocol::encode(&shell).unwrap();
    assert!(
        bytes.len() < 1024,
        "native metadata requires {} bytes",
        bytes.len()
    );
    let decoded: ThreadShell = agent_protocol::protocol::decode(&bytes).unwrap();
    assert_eq!(decoded, shell);
}

#[test]
fn native_unknown_and_busy_statuses_do_not_require_invented_application_runs() {
    use agent_protocol::{
        execution::SessionStatus,
        orchestration::{ProviderThreadStatus, RunStatus},
    };
    let mut shell = thread("native");
    shell.native_status = Some(ProviderThreadStatus::Unknown);
    assert!(shell.latest_run_id.is_none());
    assert!(shell.active_run_id.is_none());
    assert_eq!(shell.task_state().status, SessionStatus::Unknown);
    shell.native_status = Some(ProviderThreadStatus::Active);
    assert_eq!(shell.task_state().status, SessionStatus::Running);
    shell.activity_run_status = Some(RunStatus::Preparing);
    assert_eq!(shell.task_state().status, SessionStatus::Preparing);
    shell.native_status = Some(ProviderThreadStatus::Unknown);
    assert_eq!(shell.task_state().status, SessionStatus::Preparing);
    shell.activity_run_status = None;
    shell.native_status = Some(ProviderThreadStatus::Idle);
    assert_eq!(shell.task_state().status, SessionStatus::Idle);
}

#[rstest::rstest]
#[case("fork", false, false)]
#[case("subagent", false, true)]
#[case("fork", true, true)]
fn detail_and_sidebar_agree_about_nested_lineage_without_losing_fork_provenance(
    #[case] relationship: &str,
    #[case] side_chat: bool,
    #[case] nested: bool,
) {
    use agent_protocol::{orchestration as model, session};
    let mut shell = thread("branch");
    shell.thread.lineage = model::Lineage {
        parent_thread_id: Some("parent".into()),
        relationship_to_parent: Some(relationship.into()),
        root_thread_id: "parent".into(),
        side_chat: Some(side_chat),
    };
    shell.thread.forked_from = Some(model::ThreadForkSource::Run {
        thread_id: "parent".into(),
        run_id: "source-run".into(),
    });
    let snapshot = model::projection::ThreadSnapshot {
        projection: model::projection::ThreadProjection {
            thread: shell.thread.clone(),
            runs: vec![],
            attempts: vec![],
            nodes: vec![],
            subagents: vec![],
            provider_sessions: vec![],
            provider_threads: vec![],
            provider_turns: vec![],
            runtime_requests: vec![],
            plans: vec![],
            turn_items: vec![],
            checkpoint_scopes: vec![],
            checkpoints: vec![],
            context_handoffs: vec![],
            context_transfers: vec![],
            visible_turn_items: vec![],
            updated_at: shell.thread.updated_at.clone(),
        },
        snapshot_sequence: 0,
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: None,
    };
    let identity = session::SessionRef::new(None, "branch".into()).unwrap();
    let detail = super::super::project(
        &snapshot,
        &identity,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        None,
        &Default::default(),
    )
    .unwrap();
    let list = summary(&shell, None).unwrap();
    let expected_parent = nested.then(|| session::SessionRef::new(None, "parent".into()).unwrap());
    assert_eq!(list.parent_id, expected_parent);
    assert_eq!(detail.thread.parent_id, expected_parent);
    assert_eq!(
        snapshot.projection.thread.forked_from,
        shell.thread.forked_from
    );
}
