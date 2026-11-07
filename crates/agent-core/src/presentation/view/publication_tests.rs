use super::view::{Presentation, PresentationChanges};
use crate::state::Snapshot;
use std::sync::Arc;

fn changes(snapshot: &Snapshot, previous: Arc<Snapshot>) -> PresentationChanges {
    let previous = Presentation::project(previous, None);
    Presentation::project(Arc::new(snapshot.clone()), Some(&previous)).changes(previous)
}

#[test]
fn list_publication_tracks_visible_operation_phases_without_reacting_to_generations() {
    use crate::{
        models::{ListPart, Project, ThreadList},
        state::operations::{OperationKey, OperationPhase, OperationState},
    };
    for count in [0, 1, 16, 512] {
        let projects: Vec<_> = (0..count)
            .map(|index| Project {
                id: format!("project{index}"),
                ..Default::default()
            })
            .collect();
        let keys: Vec<_> = [
            OperationKey::SessionList,
            OperationKey::SessionPage {
                part: ListPart::Projects,
            },
            OperationKey::SessionPage {
                part: ListPart::Chats,
            },
        ]
        .into_iter()
        .chain(projects.iter().map(|project| OperationKey::ProjectList {
            project_id: project.id.clone(),
        }))
        .collect();
        let previous = Arc::new(Snapshot {
            lists: crate::state::Lists {
                threads: Some(Arc::new(ThreadList {
                    revision: 1,
                    limit: 5,
                    data: Vec::new(),
                    projects,
                    has_more: false,
                    has_more_projects: false,
                    project_pages: Default::default(),
                })),
                ..Default::default()
            },
            operations: keys
                .iter()
                .cloned()
                .map(|key| {
                    (
                        key,
                        OperationState {
                            generation: 1,
                            phase: OperationPhase::Running,
                        },
                    )
                })
                .collect(),
            ..Default::default()
        });
        assert!(!changes(&previous, previous.clone()).list);
        for key in keys.iter().take(3).chain(keys.last().filter(|_| count > 0)) {
            let mut next = previous.as_ref().clone();
            let state = next.operations.get_mut(key).unwrap();
            state.generation += 1;
            let generation = changes(&next, previous.clone());
            assert!(generation.changed && !generation.list);
            next.operations.get_mut(key).unwrap().phase = OperationPhase::Failed {
                message: "first failure".into(),
            };
            let failed = Arc::new(next.clone());
            let phase = changes(&next, previous.clone());
            assert!(phase.list && !phase.models && !phase.conversation);
            next.operations.get_mut(key).unwrap().phase = OperationPhase::Failed {
                message: "different failure".into(),
            };
            assert!(changes(&next, failed).list);
            next.operations.remove(key);
            assert!(changes(&next, previous.clone()).list);
        }
        for key in [
            OperationKey::WorkspaceReview,
            OperationKey::ProjectList {
                project_id: "not-visible".into(),
            },
        ] {
            let mut next = previous.as_ref().clone();
            next.operations.insert(
                key,
                OperationState {
                    generation: 2,
                    phase: OperationPhase::Failed {
                        message: "unrelated".into(),
                    },
                },
            );
            let changes = changes(&next, previous.clone());
            assert!(changes.changed && !changes.list && !changes.models && !changes.conversation);
        }
    }
}

#[test]
fn waiting_membership_refreshes_lists_without_republishing_other_surfaces() {
    use agent_protocol::{
        live_activity::{TaskActivityState, TaskActivitySummary},
        models::SessionStatus,
        session::{ProviderKind, SessionRef},
    };
    let id = SessionRef::new(ProviderKind::Codex, "question".into()).unwrap();
    let mut snapshot = Snapshot {
        lists: crate::state::Lists {
            threads: Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[{"id":id,"status":"running"}],
                    "projects":[],"hasMore":false,"hasMoreProjects":false,
                    "projectPages":{},"limit":5
                }))
                .unwrap(),
            )),
            ..Default::default()
        },
        ..Default::default()
    };
    Arc::make_mut(&mut snapshot.lists.activity)
        .active
        .insert(id.clone(), true);
    for (revision, waiting, changed) in [
        (1, false, false),
        (2, true, true),
        (3, true, false),
        (4, false, true),
    ] {
        let before = Arc::new(snapshot.clone());
        let _ = snapshot.accept_task_activity(TaskActivityState {
            revision,
            statuses: vec![(id.clone(), SessionStatus::Running)],
            waiting: if waiting {
                vec![id.clone()]
            } else {
                Vec::new()
            },
            unread: Vec::new(),
            display: TaskActivitySummary {
                waiting: u32::from(waiting),
                running: u32::from(!waiting),
                ..Default::default()
            }
            .display(),
        });
        assert!(Arc::ptr_eq(
            &snapshot.lists.activity,
            &before.lists.activity
        ));
        assert!(Arc::ptr_eq(
            snapshot.lists.threads.as_ref().unwrap(),
            before.lists.threads.as_ref().unwrap()
        ));
        let publication = changes(&snapshot, before);
        assert_eq!(publication.list, changed, "waiting={waiting}");
        assert!(!publication.models && !publication.conversation);
        assert_eq!(snapshot.thread_list().unwrap().threads[0].waiting, waiting);
    }
}

#[test]
fn publication_changes_keep_independent_surfaces_and_connection_epochs_distinct() {
    let before = Arc::new(Snapshot {
        lists: crate::state::Lists {
            threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            })).unwrap())),
            ..Default::default()
        },
        ..Default::default()
    });
    let duplicate = changes(&before, before.clone());
    assert!(!duplicate.changed && !duplicate.list && !duplicate.models && !duplicate.conversation);
    assert!(!duplicate.reconnected);
    let mut next = (*before).clone();
    next.catalog.models = Arc::new(Vec::new());
    let catalog = changes(&next, before.clone());
    assert!(catalog.changed && catalog.models);
    assert!(!catalog.list && !catalog.conversation && !catalog.reconnected);
    next = (*before).clone();
    Arc::make_mut(&mut next.pane).error = Some("failed".into());
    assert!(changes(&next, before.clone()).changed);
    next.connection.connection_generation += 1;
    assert!(
        !changes(&next, before.clone()).reconnected,
        "a disconnected epoch is not recovery"
    );
    next.connection.connected = true;
    let connected = changes(&next, before);
    assert!(connected.reconnected && connected.list);
    assert!(!connected.models && !connected.conversation);
}
#[test]
fn waiting_publication_keeps_distinct_state_and_empty_state_comparisons() {
    use crate::session::{ProviderKind, SessionRef};
    use agent_protocol::live_activity::{TaskActivityState, TaskActivitySummary};
    let activity = |count| {
        Arc::new(TaskActivityState {
            revision: 1,
            statuses: vec![],
            waiting: (0..count)
                .map(|index| SessionRef::new(ProviderKind::Codex, format!("task{index}")).unwrap())
                .collect(),
            unread: vec![],
            display: TaskActivitySummary::default().display(),
        })
    };
    for count in [0, 1, 128, 4000] {
        let previous = Arc::new(Snapshot {
            lists: crate::state::Lists {
                task_activity: Some(activity(count)),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut next = previous.as_ref().clone();
        assert!(next.list_source().equivalent(&previous.list_source()));
        Arc::make_mut(next.lists.task_activity.as_mut().unwrap()).revision += 1;
        assert!(next.list_source().equivalent(&previous.list_source()));
        if count > 0 {
            Arc::make_mut(next.lists.task_activity.as_mut().unwrap())
                .waiting
                .last_mut()
                .unwrap()
                .provider = ProviderKind::Claude;
            assert!(!next.list_source().equivalent(&previous.list_source()));
        }
        next.lists.task_activity = None;
        assert_eq!(
            next.list_source().equivalent(&previous.list_source()),
            count == 0
        );
    }
}
