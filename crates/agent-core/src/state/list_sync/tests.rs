use super::*;
use agent_protocol::{
    conversation::records,
    orchestration::shell::{ProjectShell, ShellLocation, ThreadShell},
};

fn address(index: usize) -> crate::session::SessionRef {
    crate::session::SessionRef::new(crate::session::ProviderKind::Codex, format!("task-{index}"))
        .unwrap()
}
fn row(index: usize, project: bool) -> ThreadShell {
    crate::test_conversations::shell_thread(crate::models::Thread {
        id: Some(address(index)),
        name: Some(format!("Task {index}")),
        updated_at: Some(index as f64 / 1000.0),
        project_id: if project {
            crate::models::ProjectMembership::Assigned("project".into())
        } else {
            Default::default()
        },
        ..Default::default()
    })
}
fn group(id: &str, title: &str) -> Arc<ProjectShell> {
    crate::test_conversations::shell(
        crate::models::ThreadList {
            projects: vec![crate::models::Project {
                id: id.into(),
                name: title.into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        0,
    )
    .projects
    .remove(0)
}
fn delta(thread: ThreadShell, sequence: u64) -> ShellStreamItem {
    ShellStreamItem::ThreadUpdated {
        sequence,
        location: ShellLocation::Active,
        thread,
    }
}
fn list() -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.lists.shell = Some(Arc::new(ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: 10,
        threads: (0..24)
            .map(|index| Arc::new(row(index, index >= 20)))
            .collect(),
        projects: vec![group("project", "Project")],
        archived_threads: vec![],
    }));
    snapshot.lists.generation = 1;
    snapshot.lists.subscription = Some(uuid::Uuid::new_v4());
    project(&mut snapshot, false);
    snapshot
}
fn completed(thread: &mut ThreadShell) {
    activity_fixture(
        thread,
        crate::state::list_sync::ActivityFacts {
            completion: Some("completed".into()),
            latest: Some(crate::models::TurnStatus::Completed),
            ..Default::default()
        },
    );
}

#[test]
fn pagination_and_project_disclosure_are_local_without_changing_the_list_cursor() {
    let snapshot = list();
    let before = snapshot.lists.shell.clone().unwrap();
    assert_eq!(snapshot.thread_list().unwrap().threads.len(), 5);
    assert!(
        snapshot.thread_list().unwrap().projects[0]
            .threads
            .is_empty()
    );
    let (expanded, effects) =
        reduce_intent(&snapshot, "", Intent::ExpandThreadList { project_id: None });
    assert!(effects.is_empty());
    assert_eq!(expanded.thread_list().unwrap().threads.len(), 15);
    assert!(Arc::ptr_eq(expanded.lists.shell.as_ref().unwrap(), &before));
    let (closed, effects) = reduce_intent(
        &expanded,
        "",
        Intent::SetProjectExpanded {
            project_id: "project".into(),
            expanded: false,
        },
    );
    assert!(effects.is_empty());
    assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
    let (reopened, effects) = reduce_intent(
        &closed,
        "",
        Intent::SetProjectExpanded {
            project_id: "project".into(),
            expanded: true,
        },
    );
    assert!(effects.is_empty());
    assert_eq!(reopened.thread_list().unwrap().projects[0].threads.len(), 4);
    assert_eq!(
        reopened.lists.shell.as_ref().unwrap().snapshot_sequence,
        before.snapshot_sequence
    );
}

#[test]
fn changes_update_titles_and_unread_without_reloading_or_losing_drafts() {
    let mut snapshot = list();
    snapshot.connection.connected = true;
    let id = address(0);
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
        Some(id.clone());
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        id.clone().into(),
        Arc::new(Draft {
            text: "Keep typing".into(),
            ..Default::default()
        }),
    );
    let mut updated = row(0, false);
    updated.thread.title = "New title".into();
    updated.thread.updated_at = "2026-11-14T00:00:01Z".into();
    completed(&mut updated);
    let message = delta(updated, 11);
    let subscription = snapshot.lists.subscription.unwrap();
    let (next, effects) = received(&snapshot, subscription, message.clone());
    assert_eq!(next.thread_list().unwrap().threads[0].title, "New title");
    assert_eq!(next.input.drafts[&id.clone().into()].text, "Keep typing");
    assert_eq!(
        effects.len(),
        1,
        "the visible completion is acknowledged once"
    );
    assert_eq!(
        next.lists.task_activity.as_ref().unwrap().unread,
        vec![(id, "completed".into())]
    );
    let (duplicate, effects) = received(&next, subscription, message);
    assert!(effects.is_empty());
    assert!(Arc::ptr_eq(
        duplicate.lists.shell.as_ref().unwrap(),
        next.lists.shell.as_ref().unwrap()
    ));
}

#[test]
fn a_side_pane_does_not_mark_the_conversation_read_for_other_devices() {
    let mut snapshot = list();
    snapshot.connection.connected = true;
    snapshot.open_pane("side".into(), String::new());
    let id = address(0);
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.get_mut("side").unwrap()).navigation)
        .thread_id = Some(id.clone());
    let mut updated = row(0, false);
    completed(&mut updated);
    let subscription = snapshot.lists.subscription.unwrap();
    let (next, effects) = received(&snapshot, subscription, delta(updated, 11));
    assert!(
        effects.is_empty(),
        "viewing a side pane does not publish a read receipt"
    );
    assert!(next.lists.activity.unread.contains(&id));
}

#[test]
fn bad_frames_reopen_once_and_a_new_store_replaces_every_old_row() {
    let snapshot = list();
    let subscription = snapshot.lists.subscription.unwrap();
    let from = snapshot.lists.shell.as_ref().unwrap().snapshot_sequence;
    let bad = ShellStreamItem::Snapshot {
        snapshot: ShellSnapshot {
            schema_version: 2,
            snapshot_sequence: 9,
            projects: vec![],
            threads: vec![],
            archived_threads: vec![],
        },
        resolved_repository_identity_roots: None,
    };
    let (reopening, effects) = received(&snapshot, subscription, bad.clone());
    assert_eq!(effects.len(), 1);
    assert!(reopening.lists.subscription.is_none());
    assert!(reopening.lists.shell.is_none());
    let (_, effects) = received(&reopening, subscription, bad);
    assert!(effects.is_empty());
    let mut replacement = reopening;
    replacement.lists.generation = 2;
    let opened = ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: 1,
        threads: vec![Arc::new(row(99, false))],
        projects: vec![],
        archived_threads: vec![],
    };
    SubscribeCatalog { refresh: false }.apply(
        &mut replacement,
        "",
        (
            2,
            Some(from),
            opened,
            uuid::Uuid::new_v4(),
            Default::default(),
        ),
    );
    assert_eq!(replacement.thread_list().unwrap().threads.len(), 1);
    assert_eq!(
        replacement.thread_list().unwrap().threads[0].title,
        "Task 99"
    );
    assert!(replacement.thread_list().unwrap().projects.is_empty());
    assert_eq!(
        replacement.lists.shell.as_ref().unwrap().snapshot_sequence,
        1
    );
}

#[tokio::test]
async fn list_subscription_survives_navigation_and_pagination_does_not_read_another_catalog() {
    use serde_json::json;
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        let initial=Snapshot::default();
        let (peer,mut reader,writer)=crate::test_host::connect(&initial).await;
        let store=crate::store::Store::new(peer,initial,None);
        let mut list_request=None;
        for _ in 0..3 {
            let request=reader.read_request().await.unwrap().unwrap();
            let result=match request["method"].as_str().unwrap() {
                "host/list/subscribe"=>{list_request=Some(request);continue},
                "host/model/list"=>json!({"data":[],"nextCursor":null}),
                "host/account/list"=>json!({"accounts":[],"selected":{}}),
                method=>panic!("unexpected bootstrap request {method}"),
            };
            writer.reply(&request,json!({"result":result})).await.unwrap();
        }
        store.dispatch(Intent::NewChat {cwd:String::new()}).await.unwrap();
        let fixture=list();
        writer.reply(&list_request.unwrap(),json!({"result":records::OpenedList {subscription_id:uuid::Uuid::nil(),provider_errors:Default::default(),initial:vec![ShellStreamItem::Snapshot {snapshot:fixture.lists.shell.as_ref().unwrap().as_ref().clone(),resolved_repository_identity_roots:None},ShellStreamItem::Synchronized]}})).await.unwrap();
        let mut updates=store.subscribe();
        while store.snapshot().lists.subscription.is_none() {updates.changed().await.unwrap();}
        assert_eq!(store.snapshot().thread_list().unwrap().threads.len(),5);
        store.dispatch(Intent::ExpandThreadList {project_id:None}).await.unwrap();
        assert_eq!(store.snapshot().thread_list().unwrap().threads.len(),15);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(40),reader.read_request()).await.is_err());
        store.close().await.unwrap();
    }).await.expect("list subscription lifecycle completes");
}

#[rstest::rstest]
fn child_activity_updates_the_fleet_and_discovers_new_children_once(
    #[values(
        crate::models::SessionStatus::Running,
        crate::models::SessionStatus::Unknown,
        crate::models::SessionStatus::Unavailable
    )]
    status: crate::models::SessionStatus,
) {
    let mut snapshot = list();
    snapshot.connection.connected = true;
    let root = address(0);
    Arc::make_mut(snapshot.panes.primary_mut()).observed_agents = Some(root.clone());
    let mut child = row(99, false);
    child.thread.lineage.parent_thread_id = Some(root.application_id());
    child.thread.lineage.relationship_to_parent = Some("subagent".into());
    child.native_status = Some(match status {
        crate::models::SessionStatus::Running => {
            agent_protocol::orchestration::ProviderThreadStatus::Active
        }
        _ => agent_protocol::orchestration::ProviderThreadStatus::Unknown,
    });
    let subscription = snapshot.lists.subscription.unwrap();
    let (mut next, effects) = received(&snapshot, subscription, delta(child, 11));
    assert_eq!(
        next.lists.activity.active.get(&address(99)).copied(),
        (status == crate::models::SessionStatus::Running).then_some(true)
    );
    assert_eq!(effects.len(), 1, "a new child triggers one fleet discovery");
    assert_eq!(
        next.lists.activity.active.get(&root),
        Some(&false),
        "an uncertain child must not mark its healthy provider or parent unavailable"
    );
    assert!(
        !next
            .lists
            .task_activity
            .as_ref()
            .unwrap()
            .statuses
            .iter()
            .any(|(id, _)| id == &address(99)),
        "children do not create top-level activity notifications"
    );
    assert!(
        project(&mut next, false).is_empty(),
        "known children do not trigger repeated discovery"
    );
}

#[test]
fn unavailable_providers_keep_saved_project_membership_and_host_owned_operations() {
    let mut snapshot = list();
    snapshot.lists.provider_errors = Arc::new(
        [(
            String::from("codex"),
            serde_json::json!({"message":"provider failed"}),
        )]
        .into_iter()
        .collect(),
    );
    project(&mut snapshot, false);
    let thread = &snapshot.lists.project_threads["project"].data[0];
    assert_eq!(thread.status, crate::models::SessionStatus::Unavailable);
    assert_eq!(thread.project_id.as_deref(), Some("project"));
    assert_eq!(
        thread.history_read_state.as_ref().unwrap().kind,
        crate::session::HistoryReadKind::Unavailable
    );
    let capabilities = thread.capabilities.unwrap();
    assert!(capabilities.rename && capabilities.fork);
    assert!(!capabilities.steer && !capabilities.model_change);
}

#[test]
fn paging_during_initial_subscription_keeps_its_in_flight_operation() {
    let mut state = Snapshot::default();
    Arc::make_mut(&mut state.operations).insert(
        op::OperationKey::SessionList,
        op::OperationState {
            generation: 1,
            phase: op::OperationPhase::Running,
        },
    );
    let (next, effects) = reduce_intent(&state, "", Intent::ExpandProjects);
    assert!(effects.is_empty());
    assert_eq!(next.operations, state.operations);
    assert!(next.lists.list_query.project_limit > state.lists.list_query.project_limit);
    let (next, effects) = reduce_intent(&next, "", Intent::ExpandThreadList { project_id: None });
    assert!(effects.is_empty());
    assert_eq!(next.operations, state.operations);
    assert!(next.lists.list_query.limit > state.lists.list_query.limit);
}

#[test]
fn search_projects_expand_page_and_collapse_without_changing_normal_disclosures() {
    let mut state = list();
    let mut shell = state.lists.shell.as_ref().unwrap().as_ref().clone();
    shell.snapshot_sequence += 1;
    shell
        .threads
        .extend((24..48).map(|index| Arc::new(row(index, true))));
    shell.projects.push(group("other", "Other"));
    for index in 0..7 {
        let id = format!("extra-{index}");
        shell.projects.push(group(&id, &id));
        let mut child = row(100 + index, true);
        child.thread.project_id = Some(id);
        child.thread.updated_at = "1970-01-01T00:00:00Z".into();
        shell.threads.push(Arc::new(child));
    }
    state = received(
        &state,
        state.lists.subscription.unwrap(),
        ShellStreamItem::Snapshot {
            snapshot: shell,
            resolved_repository_identity_roots: None,
        },
    )
    .0;
    let query = crate::models::ListQuery {
        search_term: "Task".into(),
        ..Default::default()
    };
    let (searched, _) = super::query(state, query, false);
    assert_eq!(searched.thread_list().unwrap().projects[0].threads.len(), 5);
    let (searched, _) = reduce_intent(&searched, "", Intent::ExpandProjects);
    assert_eq!(searched.thread_list().unwrap().projects.len(), 8);
    assert!(
        searched
            .thread_list()
            .unwrap()
            .projects
            .iter()
            .all(|project| !project.threads.is_empty())
    );
    let (more, _) = reduce_intent(
        &searched,
        "",
        Intent::ExpandThreadList {
            project_id: Some("project".into()),
        },
    );
    assert_eq!(more.thread_list().unwrap().projects[0].threads.len(), 15);
    let (mut closed, _) = reduce_intent(
        &more,
        "",
        Intent::SetProjectExpanded {
            project_id: "project".into(),
            expanded: false,
        },
    );
    project(&mut closed, false);
    assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
    let (normal, _) = super::query(closed, Default::default(), false);
    assert!(
        normal
            .thread_list()
            .unwrap()
            .projects
            .iter()
            .all(|project| project.threads.is_empty())
    );
}

#[test]
fn an_idle_catalog_update_releases_an_unselected_subscription_after_completion() {
    let mut state = list();
    let id = address(0);
    Arc::make_mut(&mut state.sessions.subscriptions).insert(id.clone(), uuid::Uuid::new_v4());
    Arc::make_mut(&mut state.sessions.conversations).insert(
        id.clone(),
        Arc::new(crate::models::Thread {
            id: Some(id.clone()),
            status: crate::models::SessionStatus::Idle,
            ..Default::default()
        }),
    );
    Arc::make_mut(&mut state.lists.activity)
        .active
        .insert(id.clone(), true);
    let (next, _) = received(
        &state,
        state.lists.subscription.unwrap(),
        delta(row(0, false), 11),
    );
    assert!(!next.sessions.subscriptions.contains_key(&id));
}

#[test]
fn empty_projects_keep_registration_order_and_subagents_stay_out_of_search() {
    let mut state = Snapshot::default();
    let mut child = row(99, true);
    child.thread.title = "needle child".into();
    child.thread.lineage.parent_thread_id = Some(address(0).application_id());
    child.thread.lineage.relationship_to_parent = Some("subagent".into());
    state.lists.shell = Some(Arc::new(ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: 10,
        projects: ["z", "project", "m"]
            .into_iter()
            .map(|id| group(id, id))
            .collect(),
        threads: vec![Arc::new(child)],
        archived_threads: vec![],
    }));
    state.lists.subscription = Some(uuid::Uuid::new_v4());
    project(&mut state, false);
    let page = state.lists.threads.as_ref().unwrap();
    assert_eq!(
        page.projects
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["z", "project", "m"]
    );
    assert!(page.data.is_empty());
    assert!(
        state
            .lists
            .project_threads
            .values()
            .all(|page| page.data.is_empty())
    );
    Arc::make_mut(&mut state.lists.list_query).search_term = "needle".into();
    project(&mut state, false);
    let page = state.lists.threads.as_ref().unwrap();
    // T3 ThreadSearch filters subagents before matching/limits. Local title
    // matching must not reintroduce a row excluded by the canonical search.
    assert!(page.projects.is_empty());
    assert!(page.data.is_empty());
    assert!(
        state
            .lists
            .project_threads
            .values()
            .all(|page| page.data.is_empty())
    );
}

#[test]
fn late_bootstraps_preserve_newer_live_metadata_and_old_generations_cannot_replace_it() {
    let mut state = list();
    state.lists.generation = 2;
    let mut live = state.lists.shell.as_ref().unwrap().as_ref().clone();
    live.snapshot_sequence = 12;
    Arc::make_mut(&mut live.threads[0]).thread.title = "live title".into();
    state.lists.shell = Some(Arc::new(live));
    let incoming = list().lists.shell.unwrap().as_ref().clone();
    let subscription = uuid::Uuid::new_v4();
    SubscribeCatalog { refresh: false }.apply(
        &mut state,
        "",
        (
            2,
            Some(10),
            incoming.clone(),
            subscription,
            Default::default(),
        ),
    );
    assert_eq!(state.lists.shell.as_ref().unwrap().snapshot_sequence, 12);
    assert_eq!(
        state.lists.shell.as_ref().unwrap().threads[0].thread.title,
        "live title"
    );
    assert_eq!(state.lists.subscription, Some(subscription));
    let shell = state.lists.shell.clone().unwrap();
    SubscribeCatalog { refresh: false }.apply(
        &mut state,
        "",
        (
            1,
            Some(10),
            incoming,
            uuid::Uuid::new_v4(),
            Default::default(),
        ),
    );
    assert!(Arc::ptr_eq(&shell, state.lists.shell.as_ref().unwrap()));
    assert_eq!(state.lists.subscription, Some(subscription));
}

#[test]
fn bootstrap_validation_requires_a_snapshot_without_a_cached_cursor_and_a_final_marker() {
    use agent_protocol::operations::RpcMethod;
    let mut request = records::SubscribeList {
        refresh: false,
        subscription: uuid::Uuid::nil(),
        after_sequence: None,
    };
    let mut output = records::OpenedList {
        subscription_id: uuid::Uuid::nil(),
        provider_errors: Default::default(),
        initial: vec![ShellStreamItem::Synchronized],
    };
    assert!(request.validate(&output).is_err());
    request.after_sequence = Some(10);
    assert!(request.validate(&output).is_ok());
    output.initial.clear();
    assert!(request.validate(&output).is_err());
    output.initial = vec![
        ShellStreamItem::Snapshot {
            snapshot: list().lists.shell.unwrap().as_ref().clone(),
            resolved_repository_identity_roots: None,
        },
        ShellStreamItem::Synchronized,
    ];
    request.after_sequence = None;
    assert!(request.validate(&output).is_ok());
    output.initial.push(ShellStreamItem::Synchronized);
    assert!(request.validate(&output).is_err());
}

#[test]
fn forks_remain_independent_threads_while_subagents_and_side_chats_remain_nested() {
    let mut snapshot = list();
    let subscription = snapshot.lists.subscription.unwrap();
    for (index, relationship, side_chat) in [
        (99, "fork", false),
        (100, "subagent", false),
        (101, "fork", true),
    ] {
        let mut branch = row(index, false);
        branch.thread.lineage.parent_thread_id = Some(address(0).application_id());
        branch.thread.lineage.relationship_to_parent = Some(relationship.into());
        branch.thread.lineage.side_chat = Some(side_chat);
        branch.native_status = Some(agent_protocol::orchestration::ProviderThreadStatus::Active);
        snapshot = received(&snapshot, subscription, delta(branch, 11)).0;
    }
    let displayed = snapshot.thread_list().unwrap();
    assert!(
        displayed
            .threads
            .iter()
            .any(|thread| thread.id == address(99)),
        "the user must be able to find their fork as an independent conversation"
    );
    for index in [100, 101] {
        assert!(
            !displayed
                .threads
                .iter()
                .any(|thread| thread.id == address(index))
        );
    }
    let activity = snapshot.lists.task_activity.as_ref().unwrap();
    assert!(activity.statuses.iter().any(|(id, _)| id == &address(99)));
    for index in [100, 101] {
        assert!(
            !activity
                .statuses
                .iter()
                .any(|(id, _)| id == &address(index))
        );
    }
}

#[test]
fn canonical_search_results_do_not_reintroduce_short_token_substring_false_positives() {
    let mut state = list();
    let shell = Arc::make_mut(state.lists.shell.as_mut().unwrap());
    let row = Arc::make_mut(&mut shell.threads[0]);
    row.thread.title = "drafts".into();
    Arc::make_mut(&mut state.lists.list_query).search_term = "fts".into();
    state.lists.search_results = Some(("fts".into(), Arc::new(ThreadList::default())));
    project(&mut state, false);
    assert!(
        state.lists.threads.as_ref().unwrap().data.is_empty(),
        "T3 short tokens must start a word: a canonical empty result cannot be undone by client substring matching"
    );
}

#[test]
fn ranked_search_order_reaches_the_rendered_conversation_list() {
    use crate::state::operations::Operation;
    let mut state = list();
    let shell = Arc::make_mut(state.lists.shell.as_mut().unwrap());
    for (i, title) in ["Storage", "Storage recovery", "A storage task"]
        .into_iter()
        .enumerate()
    {
        Arc::make_mut(&mut shell.threads[i]).thread.title = title.into();
    }
    Arc::make_mut(&mut state.lists.list_query).search_term = "storage".into();
    let data = (0..3)
        .map(|i| {
            agent_protocol::orchestration::shell::summary(
                &state.lists.shell.as_ref().unwrap().threads[i],
                None,
            )
            .unwrap()
        })
        .collect();
    SearchCatalog {
        term: "storage".into(),
    }
    .apply(
        &mut state,
        "",
        ThreadList {
            data,
            ..Default::default()
        },
    );
    // The pinned reference's first captured title case puts the exact title
    // before its two whole-word matches, regardless of recent header updates.
    let expected = vec![address(0), address(1), address(2)];
    assert_eq!(
        state
            .lists
            .threads
            .as_ref()
            .unwrap()
            .data
            .iter()
            .map(|thread| thread.id.clone().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        state
            .thread_list()
            .unwrap()
            .threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn search_reply_distinguishes_saved_message_matches_from_stale_titles_and_removed_headers() {
    use crate::state::operations::Operation;
    // T3 search hits identify their source. An old title match is not proof
    // that a saved body still matches; a real body match can outlive a rename.
    let mut base = list();
    Arc::make_mut(&mut base.lists.list_query).search_term = "needle".into();
    let shell = Arc::make_mut(base.lists.shell.as_mut().unwrap());
    Arc::make_mut(&mut shell.threads[0]).thread.title = "needle old title".into();
    let old = agent_protocol::orchestration::shell::summary(&shell.threads[0], None).unwrap();
    Arc::make_mut(&mut shell.threads[0]).thread.title = "current opaque title".into();
    let mut differences = Vec::new();
    for (body_match, removed, expected) in [
        (false, false, false),
        (true, false, true),
        (true, true, false),
    ] {
        let mut state = base.clone();
        if removed {
            Arc::make_mut(state.lists.shell.as_mut().unwrap())
                .threads
                .remove(0);
        }
        let mut wire = serde_json::to_value(ThreadList {
            data: vec![old.clone()],
            ..Default::default()
        })
        .unwrap();
        wire["messageMatches"] = serde_json::json!(if body_match {
            vec![address(0)]
        } else {
            Vec::new()
        });
        let result = serde_json::from_value(wire).unwrap();
        SearchCatalog {
            term: "needle".into(),
        }
        .apply(&mut state, "", result);
        let rows = &state.lists.threads.as_ref().unwrap().data;
        if rows.iter().any(|row| row.id.as_ref() == Some(&address(0))) != expected {
            differences.push((body_match, removed));
        }
        if expected && let Some(row) = rows.first() {
            assert_eq!(row.name.as_deref(), Some("current opaque title"));
        }
    }
    assert!(
        differences.is_empty(),
        "canonical source/header fences differ for (body_match,removed): {differences:?}"
    );
}

#[test]
fn placed_saved_shortcuts_use_word_boundaries_without_requiring_a_canonical_header() {
    let mut snapshot = Snapshot::default();
    seed(
        &mut snapshot,
        serde_json::from_value(serde_json::json!({
            "data":[],"projects":[{"id":"project","name":"Workspace","roots":[]}],
            "limit":5,"hasMore":false,"hasMoreProjects":false
        }))
        .unwrap(),
    );
    let id = crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "saved".into())
        .unwrap();
    snapshot.lists.project_preferences = Arc::new(
        [(
            snapshot.connection.storage_scope.clone(),
            [(
                "project".into(),
                crate::state::projects::ProjectPreferences {
                    placed_chats: vec![crate::state::projects::PlacedChat {
                        id: id.clone(),
                        title: "Drafts".into(),
                    }],
                    ..Default::default()
                },
            )]
            .into(),
        )]
        .into(),
    );
    for (term, expected) in [("draft", vec![id.clone()]), ("fts", vec![])] {
        Arc::make_mut(&mut snapshot.lists.list_query).search_term = term.into();
        let rows = snapshot
            .thread_list()
            .unwrap()
            .projects
            .into_iter()
            .find(|p| p.id == "project")
            .unwrap()
            .threads;
        assert_eq!(
            rows.into_iter().map(|row| row.id).collect::<Vec<_>>(),
            expected
        );
    }
}
