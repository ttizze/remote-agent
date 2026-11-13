use super::*;
use agent_protocol::{
    conversation::{
        self as decision,
        records::{Details, Header, Record, Row},
    },
    sync::{Cursor, Message, Position},
};

fn address(index: usize) -> crate::session::SessionRef {
    crate::session::SessionRef::new(crate::session::ProviderKind::Codex, format!("task-{index}"))
        .unwrap()
}
fn row(index: usize, project: bool, version: u64) -> Row {
    Row {
        key: Key::Thread(address(index)),
        version,
        position: Position::Unordered,
        content: Content::Full(Arc::new(Record::Thread(Header {
            state: decision::Thread {
                name: format!("Task {index}"),
                workspace: decision::Workspace::Missing,
                provider: crate::session::ProviderKind::Codex,
                queue_held: false,
            },
            details: Details {
                updated_at: index as i64,
                project: if project {
                    crate::models::ProjectMembership::Assigned("project".into())
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        }))),
    }
}
fn list() -> Snapshot {
    let mut snapshot = Snapshot::default();
    let mut rows = (0..24)
        .map(|index| row(index, index >= 20, 10))
        .collect::<Vec<_>>();
    rows.push(Row {
        key: Key::Project("project".into()),
        version: 10,
        position: Position::Project { position: 0 },
        content: Content::Full(Arc::new(Record::Project(crate::models::Project {
            id: "project".into(),
            name: "Project".into(),
            ..Default::default()
        }))),
    });
    let message = Message::Snapshot {
        cursor: Cursor {
            store: uuid::Uuid::from_u128(1),
            sequence: 10,
        },
        window: None,
        rows,
    };
    snapshot.lists.replica = Some(Arc::new(
        records::Replica::new(Scope::List, 1)
            .apply(1, message)
            .unwrap(),
    ));
    snapshot.lists.subscription = Some(uuid::Uuid::new_v4());
    project(&mut snapshot, false);
    snapshot
}

#[test]
fn pagination_and_project_disclosure_are_local_without_changing_the_list_cursor() {
    let snapshot = list();
    let before = snapshot.lists.replica.clone().unwrap();
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
    assert!(Arc::ptr_eq(
        expanded.lists.replica.as_ref().unwrap(),
        &before
    ));
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
        reopened.lists.replica.as_ref().unwrap().cursor(),
        before.cursor()
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
    let mut updated = row(0, false, 11);
    let Content::Full(record) = &mut updated.content else {
        unreachable!()
    };
    let Record::Thread(header) = Arc::make_mut(record) else {
        unreachable!()
    };
    header.state.name = "New title".into();
    header.details.updated_at = 1000;
    header.details.activity.completion = Some("completed".into());
    header.details.activity.latest = Some(crate::models::TurnStatus::Completed);
    let from = snapshot.lists.replica.as_ref().unwrap().cursor().unwrap();
    let message = Message::Events {
        through: Cursor {
            sequence: 11,
            ..from
        },
        events: vec![agent_protocol::sync::Event {
            sequence: (Cursor {
                sequence: 11,
                ..from
            })
            .sequence,
            rows: vec![updated],
            removed: Vec::new(),
        }],
    };
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
        duplicate.lists.replica.as_ref().unwrap(),
        next.lists.replica.as_ref().unwrap()
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
    let mut updated = row(0, false, 11);
    let Content::Full(record) = &mut updated.content else {
        unreachable!()
    };
    let Record::Thread(header) = Arc::make_mut(record) else {
        unreachable!()
    };
    header.details.activity.completion = Some("completed".into());
    header.details.activity.latest = Some(crate::models::TurnStatus::Completed);
    let from = snapshot.lists.replica.as_ref().unwrap().cursor().unwrap();
    let subscription = snapshot.lists.subscription.unwrap();
    let (next, effects) = received(
        &snapshot,
        subscription,
        Message::Events {
            through: Cursor {
                sequence: 11,
                ..from
            },
            events: vec![agent_protocol::sync::Event {
                sequence: (Cursor {
                    sequence: 11,
                    ..from
                })
                .sequence,
                rows: vec![updated],
                removed: vec![],
            }],
        },
    );
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
    let from = snapshot.lists.replica.as_ref().unwrap().cursor().unwrap();
    let bad = Message::Snapshot {
        cursor: Cursor {
            sequence: 9,
            ..from
        },
        window: None,
        rows: Vec::new(),
    };
    let (reopening, effects) = received(&snapshot, subscription, bad.clone());
    assert_eq!(effects.len(), 1);
    assert!(reopening.lists.subscription.is_none());
    assert_eq!(
        reopening.lists.replica.as_ref().unwrap().cursor(),
        Some(from)
    );
    let (_, effects) = received(&reopening, subscription, bad);
    assert!(effects.is_empty());
    let mut replacement = reopening;
    replacement.lists.replica = Some(Arc::new(
        replacement
            .lists
            .replica
            .as_ref()
            .unwrap()
            .resubscribe(2)
            .unwrap(),
    ));
    let cursor = Cursor {
        store: uuid::Uuid::from_u128(2),
        sequence: 1,
    };
    let opened = records::Replica::new(Scope::List, 2)
        .apply(
            2,
            Message::Snapshot {
                cursor,
                window: None,
                rows: vec![row(99, false, 1)],
            },
        )
        .unwrap();
    SubscribeCatalog { refresh: false }.apply(
        &mut replacement,
        "",
        (opened, uuid::Uuid::new_v4(), Default::default()),
    );
    assert_eq!(replacement.thread_list().unwrap().threads.len(), 1);
    assert_eq!(
        replacement.thread_list().unwrap().threads[0].title,
        "Task 99"
    );
    assert!(replacement.thread_list().unwrap().projects.is_empty());
    assert_eq!(
        replacement.lists.replica.as_ref().unwrap().cursor(),
        Some(cursor)
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
        writer.reply(&list_request.unwrap(),json!({"result":records::OpenedList {subscription_id:uuid::Uuid::nil(),provider_errors:Default::default(),initial:fixture.lists.replica.as_ref().unwrap().snapshot().unwrap()}})).await.unwrap();
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
    let mut child = row(99, false, 11);
    let Content::Full(record) = &mut child.content else {
        panic!("full row")
    };
    let Record::Thread(header) = Arc::make_mut(record) else {
        panic!("header")
    };
    header.details.parent = Some(root.clone());
    header.details.activity.status = status;
    let cursor = snapshot.lists.replica.as_ref().unwrap().cursor().unwrap();
    let subscription = snapshot.lists.subscription.unwrap();
    let (mut next, effects) = received(
        &snapshot,
        subscription,
        Message::Events {
            through: Cursor {
                sequence: 11,
                ..cursor
            },
            events: vec![agent_protocol::sync::Event {
                sequence: (Cursor {
                    sequence: 11,
                    ..cursor
                })
                .sequence,
                rows: vec![child],
                removed: vec![],
            }],
        },
    );
    assert_eq!(
        next.lists.activity.active.get(&address(99)).copied(),
        (status == crate::models::SessionStatus::Running).then_some(true)
    );
    assert_eq!(effects.len(), 1, "a new child triggers one fleet discovery");
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
    let from = state.lists.replica.as_ref().unwrap().cursor().unwrap();
    let through = Cursor {
        sequence: from.sequence + 1,
        ..from
    };
    let mut rows = (24..48)
        .map(|index| row(index, true, through.sequence))
        .collect::<Vec<_>>();
    rows.push(Row {
        key: Key::Project("other".into()),
        version: through.sequence,
        position: Position::Project { position: 1 },
        content: Content::Full(Arc::new(Record::Project(crate::models::Project {
            id: "other".into(),
            name: "Other".into(),
            ..Default::default()
        }))),
    });
    for index in 0..7 {
        let id = format!("extra-{index}");
        rows.push(Row {
            key: Key::Project(id.clone()),
            version: through.sequence,
            position: Position::Project {
                position: index as u64 + 2,
            },
            content: Content::Full(Arc::new(Record::Project(crate::models::Project {
                id: id.clone(),
                name: id.clone(),
                ..Default::default()
            }))),
        });
        let mut child = row(100 + index, true, through.sequence);
        let Content::Full(record) = &mut child.content else {
            unreachable!()
        };
        let Record::Thread(header) = Arc::make_mut(record) else {
            unreachable!()
        };
        header.details.project = crate::models::ProjectMembership::Assigned(id);
        header.details.updated_at = 0;
        rows.push(child);
    }
    state = received(
        &state,
        state.lists.subscription.unwrap(),
        Message::Events {
            through,
            events: vec![agent_protocol::sync::Event {
                sequence: through.sequence,
                rows,
                removed: Vec::new(),
            }],
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
    let from = state.lists.replica.as_ref().unwrap().cursor().unwrap();
    let through = Cursor {
        sequence: from.sequence + 1,
        ..from
    };
    let (next, _) = received(
        &state,
        state.lists.subscription.unwrap(),
        Message::Events {
            through,
            events: vec![agent_protocol::sync::Event {
                sequence: through.sequence,
                rows: vec![row(0, false, through.sequence)],
                removed: Vec::new(),
            }],
        },
    );
    assert!(!next.sessions.subscriptions.contains_key(&id));
}

#[test]
fn empty_projects_keep_registration_order_and_child_matches_only_show_in_search() {
    let mut state = Snapshot::default();
    let mut rows = Vec::new();
    for (position, id) in ["z", "project", "m"].into_iter().enumerate() {
        rows.push(Row {
            key: Key::Project(id.into()),
            version: 10,
            position: Position::Project {
                position: position as u64,
            },
            content: Content::Full(Arc::new(Record::Project(crate::models::Project {
                id: id.into(),
                name: id.into(),
                ..Default::default()
            }))),
        });
    }
    let mut child = row(99, true, 10);
    let Content::Full(record) = &mut child.content else {
        panic!("full record")
    };
    let Record::Thread(header) = Arc::make_mut(record) else {
        panic!("thread")
    };
    header.state.name = "needle child".into();
    header.details.parent = Some(address(0));
    rows.push(child);
    state.lists.replica = Some(Arc::new(
        records::Replica::new(Scope::List, 1)
            .apply(
                1,
                Message::Snapshot {
                    cursor: Cursor {
                        store: uuid::Uuid::from_u128(1),
                        sequence: 10,
                    },
                    window: None,
                    rows,
                },
            )
            .unwrap(),
    ));
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
    assert_eq!(
        page.projects
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["project"]
    );
    assert_eq!(
        state.lists.project_threads["project"].data[0].id,
        Some(address(99))
    );
}
