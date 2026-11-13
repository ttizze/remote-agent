use super::*;
use crate::{
    models::{ListQuery, ProjectMembership, SessionStatus, Thread},
    session::{ProviderKind, SessionRef},
    state::{ProjectPreferences, projects::PlacedChat},
};
use std::sync::Arc;

fn session(id: &str) -> SessionRef {
    SessionRef::new(ProviderKind::Codex, id.into()).unwrap()
}

fn fixture() -> Snapshot {
    let mut snapshot = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            storage_scope: "active".into(),
            ..Default::default()
        },
        lists: crate::state::Lists {
            threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[],"projects":[{"id":"destination","name":"Destination","roots":[]},{"id":"source","name":"Source","roots":[]}],
                "limit":5,"hasMore":false,"hasMoreProjects":false
            })).unwrap())),
            ..Default::default()
        },
        ..Default::default()
    };
    for (key, name) in [
        ("live", "Live latest"),
        ("inactive", "Inactive latest"),
        ("child", "Child latest"),
        ("anonymous", "Anonymous latest"),
        ("archived", "Archived latest"),
        ("active-archived", "Active archived"),
    ] {
        let mut thread = Thread {
            id: Some(session(key)),
            name: Some(name.into()),
            updated_at: Some(1.0),
            project_id: ProjectMembership::Assigned("source".into()),
            status: SessionStatus::Idle,
            ..Default::default()
        };
        match key {
            "live" => {
                thread.list_stale = Some(true);
                thread.has_changes = Some(false);
            }
            "anonymous" => thread.id = None,
            "child" => thread.parent_id = Some(session("parent")),
            _ => {}
        }
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(session(key), Arc::new(thread));
    }
    let preferences = ProjectPreferences {
        placed_chats: [
            "live",
            "missing",
            "inactive",
            "child",
            "anonymous",
            "archived",
            "active-archived",
        ]
        .map(|id| PlacedChat {
            id: session(id),
            title: if id == "missing" {
                "Missing saved".into()
            } else {
                format!("Old saved {id}")
            },
        })
        .into(),
        archived_before: Some(100),
        archived_ids: [session("archived"), session("active-archived")].into(),
        ..Default::default()
    };
    snapshot.lists.project_preferences = Arc::new(
        [
            (
                "active".into(),
                [("destination".into(), preferences)].into(),
            ),
            (
                "other".into(),
                [(
                    "destination".into(),
                    ProjectPreferences {
                        placed_chats: vec![PlacedChat {
                            id: session("other"),
                            title: "Other scope".into(),
                        }],
                        ..Default::default()
                    },
                )]
                .into(),
            ),
        ]
        .into(),
    );
    snapshot.lists.expanded_projects = Arc::new([("destination".into(), 5)].into());
    let activity = Arc::make_mut(&mut snapshot.lists.activity);
    activity.active.insert(session("live"), true);
    activity.active.insert(session("active-archived"), true);
    activity.unread.insert(session("missing"));
    snapshot.lists.task_activity =
        Some(Arc::new(agent_protocol::live_activity::TaskActivityState {
            attention: Vec::new(),
            statuses: vec![],
            waiting: vec![session("live")],
            background: Vec::new(),
            unread: vec![],
            display: agent_protocol::live_activity::TaskActivitySummary::default().display(),
        }));
    snapshot
}

fn placed(snapshot: &Snapshot) -> Vec<ThreadSummary> {
    snapshot
        .thread_list()
        .unwrap()
        .projects
        .into_iter()
        .find(|project| project.id == "destination")
        .unwrap()
        .threads
}

#[test]
fn placed_rows_keep_live_metadata_saved_fallbacks_and_activity_in_the_destination() {
    let snapshot = fixture();
    let rows = placed(&snapshot);
    assert_eq!(
        rows.iter().map(|row| &row.id).collect::<Vec<_>>(),
        [
            &session("live"),
            &session("missing"),
            &session("inactive"),
            &session("active-archived")
        ]
    );
    assert!(
        rows.iter()
            .all(|row| row.project_id.as_deref() == Some("destination"))
    );
    assert_eq!(rows[0].title, "Live latest（保存済み・未確認）");
    assert!(rows[0].active && rows[0].waiting && !rows[0].unread);
    assert_eq!(rows[0].has_changes, Some(false));
    assert_eq!(rows[1].title, "Missing saved");
    assert!(!rows[1].active && !rows[1].waiting && rows[1].unread);
    assert_eq!(rows[2].title, "Inactive latest");
    assert!(!rows[2].active);
    assert!(rows[3].active);
    assert_eq!(
        snapshot.sessions.conversations[&session("live")]
            .project_id
            .as_deref(),
        Some("source")
    );
}

#[test]
fn placed_search_uses_plain_live_or_saved_titles_and_the_active_storage_scope() {
    let mut snapshot = fixture();
    for (search, ids) in [
        ("LIVE LATEST", vec![session("live")]),
        ("保存済み", vec![]),
        ("Old saved", vec![]),
        ("MISSING", vec![session("missing")]),
    ] {
        snapshot.lists.list_query = Arc::new(ListQuery {
            search_term: search.into(),
            ..Default::default()
        });
        assert_eq!(
            placed(&snapshot)
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            ids
        );
    }
    snapshot.lists.list_query = Arc::default();
    snapshot.connection.storage_scope = "other".into();
    let rows = placed(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, session("other"));
    assert_eq!(rows[0].title, "Other scope");
}

#[test]
fn project_preferences_keep_pinned_section_order_ties_and_hidden_labels() {
    let projects = [
        "plain-a",
        "hidden",
        "empty-sec",
        "none-sec",
        "pinned",
        "unicode",
        "ordered",
        "plain-b",
    ]
    .map(|id| crate::models::Project {
        id: id.into(),
        name: id.into(),
        ..Default::default()
    })
    .into();
    let input = crate::models::ThreadList {
        limit: 5,
        data: vec![],
        projects,
        has_more: false,
        has_more_projects: false,
        provider_errors: None,
    };
    let mut snapshot = Snapshot {
        lists: crate::state::Lists {
            threads: Some(Arc::new(input)),
            ..Default::default()
        },
        ..Default::default()
    };
    snapshot.lists.project_preferences = Arc::new(
        [(
            snapshot.connection.storage_scope.clone(),
            [
                (
                    "hidden".into(),
                    ProjectPreferences {
                        hidden: true,
                        pinned: true,
                        name: Some("Hidden custom".into()),
                        section: Some("a".into()),
                        ..Default::default()
                    },
                ),
                (
                    "empty-sec".into(),
                    ProjectPreferences {
                        section: Some(String::new()),
                        ..Default::default()
                    },
                ),
                ("none-sec".into(), ProjectPreferences::default()),
                (
                    "pinned".into(),
                    ProjectPreferences {
                        pinned: true,
                        name: Some("Pinned custom".into()),
                        section: Some("z".into()),
                        order: Some(999),
                        ..Default::default()
                    },
                ),
                (
                    "unicode".into(),
                    ProjectPreferences {
                        section: Some("あ".into()),
                        ..Default::default()
                    },
                ),
                (
                    "ordered".into(),
                    ProjectPreferences {
                        order: Some(0),
                        ..Default::default()
                    },
                ),
            ]
            .into(),
        )]
        .into(),
    );
    let list = snapshot.thread_list().unwrap();
    assert_eq!(
        list.projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<Vec<_>>(),
        [
            "pinned",
            "ordered",
            "plain-a",
            "empty-sec",
            "none-sec",
            "plain-b",
            "unicode"
        ]
    );
    assert_eq!(list.projects[0].name, "Pinned custom");
    assert_eq!(list.projects[0].monogram, "PD");
    assert_eq!(list.projects[0].icon_color, 0x60a5fa);
    assert!(list.projects[0].pinned);
    assert_eq!(list.projects[0].section.as_deref(), Some("z"));
    assert_eq!(list.projects[3].section.as_deref(), Some(""));
    assert_eq!(list.projects[4].section, None);
    assert_eq!(list.hidden_projects.len(), 1);
    assert_eq!(list.hidden_projects[0].id, "hidden");
    assert_eq!(list.hidden_projects[0].name, "Hidden custom");
    assert_eq!(list.hidden_projects[0].monogram, "HN");
    assert_eq!(list.hidden_projects[0].icon_color, 0xf472b6);
    assert!(!list.hidden_projects[0].pinned);
    assert_eq!(list.hidden_projects[0].section, None);
}

#[test]
fn project_override_keeps_catalogue_membership_and_does_not_create_row_ids() {
    let row = Thread {
        id: Some(session("thread")),
        project_id: ProjectMembership::Assigned("native".into()),
        ..Default::default()
    };
    let projects = ["native", "placed", ""].into();
    for (project, expected) in [
        (None, Some("native")),
        (Some("placed"), Some("placed")),
        (Some("unknown"), None),
        (Some(""), Some("")),
    ] {
        let rows = thread_summaries(
            crate::localization::UiLanguage::Japanese,
            std::iter::once(&row),
            &BTreeMap::new(),
            &BTreeSet::new(),
            TaskMarks::default(),
            &projects,
            project,
        );
        assert_eq!(rows[0].project_id.as_deref(), expected);
    }
    let invalid = [
        Thread::default(),
        Thread {
            parent_id: Some(session("parent")),
            ..row
        },
    ];
    assert!(
        thread_summaries(
            crate::localization::UiLanguage::Japanese,
            invalid.iter(),
            &BTreeMap::new(),
            &BTreeSet::new(),
            TaskMarks::default(),
            &projects,
            Some("placed")
        )
        .is_empty()
    );
}

#[test]
fn large_placement_lists_keep_scope_hidden_and_dangling_destinations_in_every_view() {
    let mut snapshot = fixture();
    Arc::make_mut(&mut snapshot.lists.list_query).limit = 256;
    snapshot.sessions.conversations = Arc::new(
        [
            (
                session("chat-2"),
                Arc::new(Thread {
                    id: Some(session("chat-2")),
                    name: Some("Read latest".into()),
                    ..Default::default()
                }),
            ),
            (
                session("chat-3"),
                Arc::new(Thread {
                    id: None,
                    name: Some("Read anonymous".into()),
                    ..Default::default()
                }),
            ),
        ]
        .into(),
    );
    let list = Arc::make_mut(snapshot.lists.threads.as_mut().unwrap());
    list.data = (0..140)
        .map(|index| Thread {
            id: Some(session(&format!("chat-{index}"))),
            name: Some(format!("Chat {index}")),
            updated_at: Some(index as f64),
            ..Default::default()
        })
        .collect();
    list.projects.push(Project {
        id: "a-hidden".into(),
        name: "Hidden".into(),
        ..Default::default()
    });
    let mut page = list.clone();
    page.data = [0, 1, 2, 4, 130]
        .map(|index| Thread {
            id: Some(session(&format!("chat-{index}"))),
            name: Some(format!("Source {index}")),
            project_id: ProjectMembership::Assigned("source".into()),
            updated_at: Some(index as f64),
            ..Default::default()
        })
        .into();
    snapshot.lists.project_threads = Arc::new([("source".into(), Arc::new(page))].into());
    snapshot.lists.expanded_projects =
        Arc::new([("source".into(), 256), ("destination".into(), 256)].into());
    let mut preferences = BTreeMap::<String, ProjectPreferences>::new();
    for (index, project) in (0..128).map(|index| {
        (
            index,
            match index {
                0 => "a-hidden",
                1 => "b-dangling",
                _ => "destination",
            },
        )
    }) {
        preferences
            .entry(project.into())
            .or_default()
            .placed_chats
            .push(PlacedChat {
                id: session(&format!("chat-{index}")),
                title: format!("Saved {index}"),
            });
    }
    preferences.get_mut("a-hidden").unwrap().hidden = true;
    preferences
        .get_mut("destination")
        .unwrap()
        .placed_chats
        .push(PlacedChat {
            id: session("chat-0"),
            title: "Duplicate later project".into(),
        });
    preferences
        .get_mut("destination")
        .unwrap()
        .placed_chats
        .push(PlacedChat {
            id: session("missing-row"),
            title: "Saved missing row".into(),
        });
    snapshot.lists.project_preferences = Arc::new([("active".into(), preferences)].into());
    let list = snapshot.thread_list().unwrap();
    assert_eq!(
        list.threads
            .iter()
            .map(|row| row.id.id.as_str())
            .collect::<Vec<_>>(),
        (128..140)
            .map(|index| format!("chat-{index}"))
            .collect::<Vec<_>>()
    );
    let source = list.projects.iter().find(|p| p.id == "source").unwrap();
    assert_eq!(
        source
            .threads
            .iter()
            .map(|row| row.id.id.as_str())
            .collect::<Vec<_>>(),
        ["chat-130"]
    );
    let destination = list
        .projects
        .iter()
        .find(|p| p.id == "destination")
        .unwrap();
    assert_eq!(
        destination
            .threads
            .iter()
            .find(|row| row.id == session("chat-2"))
            .unwrap()
            .title,
        "Read latest"
    );
    assert!(
        !destination
            .threads
            .iter()
            .any(|row| row.id == session("chat-3"))
    );
    assert_eq!(
        destination
            .threads
            .iter()
            .find(|row| row.id == session("chat-4"))
            .unwrap()
            .title,
        "Chat 4"
    );
    assert_eq!(
        destination
            .threads
            .iter()
            .find(|row| row.id == session("missing-row"))
            .unwrap()
            .title,
        "Saved missing row"
    );
    assert_eq!(list.hidden_projects.len(), 1);
    let recent = snapshot.recent_thread_list().unwrap();
    assert_eq!(recent.threads.len(), 139);
    assert!(!recent.threads.iter().any(|row| row.id == session("chat-0")));
    for (id, project) in [
        ("chat-1", None),
        ("chat-2", Some("destination")),
        ("chat-130", Some("source")),
    ] {
        assert_eq!(
            recent
                .threads
                .iter()
                .find(|row| row.id == session(id))
                .unwrap()
                .project_id
                .as_deref(),
            project
        );
    }
    snapshot.connection.storage_scope = "other".into();
    let list = snapshot.thread_list().unwrap();
    assert_eq!(list.threads.len(), 140);
    assert!(list.hidden_projects.is_empty());
    assert_eq!(
        list.projects
            .iter()
            .find(|p| p.id == "source")
            .unwrap()
            .threads
            .len(),
        5
    );
    assert!(
        list.projects
            .iter()
            .find(|p| p.id == "destination")
            .unwrap()
            .threads
            .is_empty()
    );
    assert_eq!(snapshot.recent_thread_list().unwrap().threads.len(), 140);
}

#[test]
fn large_archived_projects_keep_actual_metadata_membership_and_active_overrides() {
    let mut snapshot = fixture();
    let count = 512;
    snapshot.sessions.conversations = Arc::default();
    let shortcuts: Vec<_> = (0..count)
        .map(|index| PlacedChat {
            id: session(&format!("saved{index}")),
            title: format!("Saved {index}"),
        })
        .collect();
    for (index, chat) in shortcuts.iter().enumerate() {
        let mut thread = Thread {
            id: Some(chat.id.clone()),
            name: Some(chat.title.clone()),
            updated_at: Some(10.),
            status: SessionStatus::Idle,
            ..Default::default()
        };
        match index {
            0 => thread.id = None,
            1 => thread.id = Some(session("native1")),
            2 => thread.id = Some(session("saved3")),
            12 => thread.parent_id = Some(session("parent")),
            _ => {}
        }
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(chat.id.clone(), Arc::new(thread));
    }
    let mut page = snapshot.lists.threads.as_deref().unwrap().clone();
    page.data = (0..count)
        .map(|index| Thread {
            id: Some(session(&format!("native{index}"))),
            name: Some(format!("Native {index}")),
            project_id: ProjectMembership::Assigned("destination".into()),
            updated_at: match index {
                8 => Some(f64::NAN),
                9 => None,
                10 => Some(f64::INFINITY),
                11 => Some(101.),
                _ => Some(10.),
            },
            status: SessionStatus::Idle,
            ..Default::default()
        })
        .collect();
    snapshot.lists.project_threads = Arc::new([("destination".into(), Arc::new(page))].into());
    snapshot.lists.expanded_projects = Arc::new([("destination".into(), count as u32)].into());
    snapshot.lists.project_preferences = Arc::new(
        [(
            "active".into(),
            [(
                "destination".into(),
                ProjectPreferences {
                    archived_before: Some(100),
                    archived_ids: [session("saved4"), session("saved5")].into(),
                    placed_chats: shortcuts,
                    ..Default::default()
                },
            )]
            .into(),
        )]
        .into(),
    );
    Arc::make_mut(&mut snapshot.lists.activity)
        .active
        .extend([(session("native0"), true), (session("saved5"), true)]);
    let mut expected: Vec<_> = (2..count)
        .filter(|index| !matches!(index, 4 | 12))
        .map(|index| session(&format!("saved{}", if index == 2 { 3 } else { index })))
        .collect();
    expected.extend([0, 8, 9, 10, 11].map(|index| session(&format!("native{index}"))));
    let list = snapshot.thread_list().unwrap();
    let project = list
        .projects
        .iter()
        .find(|project| project.id == "destination")
        .unwrap();
    assert_eq!(
        project
            .threads
            .iter()
            .map(|thread| &thread.id)
            .collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert!(
        project
            .threads
            .iter()
            .find(|thread| thread.id == session("saved5"))
            .unwrap()
            .active
    );
    let recent = snapshot.recent_thread_list().unwrap();
    assert_eq!(
        recent
            .threads
            .iter()
            .map(|thread| &thread.id)
            .collect::<BTreeSet<_>>(),
        [0, 8, 9, 10, 11]
            .map(|index| session(&format!("native{index}")))
            .iter()
            .collect()
    );
}

#[test]
fn archive_filters_apply_with_only_a_timestamp_or_only_explicit_ids() {
    for (before, ids, expected) in [
        (Some(100), vec![], vec!["new", "undated"]),
        (None, vec![session("old")], vec!["new", "undated"]),
        (None, vec![], vec!["old", "new", "undated"]),
    ] {
        let mut snapshot = fixture();
        let mut page = snapshot.lists.threads.as_deref().unwrap().clone();
        page.data = [("old", Some(1.)), ("new", Some(101.)), ("undated", None)]
            .map(|(id, updated_at)| Thread {
                id: Some(session(id)),
                updated_at,
                project_id: ProjectMembership::Assigned("destination".into()),
                status: SessionStatus::Idle,
                ..Default::default()
            })
            .into();
        snapshot.lists.project_threads = Arc::new([("destination".into(), Arc::new(page))].into());
        snapshot.lists.project_preferences = Arc::new(
            [(
                "active".into(),
                [(
                    "destination".into(),
                    ProjectPreferences {
                        archived_before: before,
                        archived_ids: ids.into_iter().collect(),
                        ..Default::default()
                    },
                )]
                .into(),
            )]
            .into(),
        );
        let list = snapshot.thread_list().unwrap();
        let rows = list
            .projects
            .into_iter()
            .find(|project| project.id == "destination")
            .unwrap()
            .threads;
        assert_eq!(
            rows.iter()
                .map(|row| row.id.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        let recent = snapshot.recent_thread_list().unwrap();
        assert_eq!(
            recent
                .threads
                .iter()
                .map(|row| row.id.id.as_str())
                .collect::<BTreeSet<_>>(),
            expected.into_iter().collect()
        );
    }
}

#[test]
fn project_decorations_preserve_the_loaded_rows_and_connection_notice() {
    use crate::state::operations::{OperationKey, OperationPhase, OperationState};
    for connected in [false, true] {
        for cached in [false, true] {
            for expanded in [false, true] {
                let mut snapshot = fixture();
                snapshot.connection.connected = connected;
                snapshot.lists.expanded_projects = Arc::new(if expanded {
                    [("destination".into(), 5)].into()
                } else {
                    BTreeMap::new()
                });
                snapshot.lists.project_threads = Arc::new(if cached {
                    [(
                        "destination".into(),
                        snapshot.lists.threads.as_ref().unwrap().clone(),
                    )]
                    .into()
                } else {
                    BTreeMap::new()
                });
                let expected =
                    (expanded && !connected && !cached).then_some("接続するとタスクを読み込めます");
                let project = snapshot
                    .thread_list()
                    .unwrap()
                    .projects
                    .into_iter()
                    .find(|project| project.id == "destination")
                    .unwrap();
                assert_eq!(
                    project.error.as_ref().map(|error| error.message.as_str()),
                    expected
                );
                let key = OperationKey::ListDecorations {
                    scope: agent_protocol::operations::ListDecorationScope::Project {
                        project_id: "destination".into(),
                    },
                };
                Arc::make_mut(&mut snapshot.operations).insert(
                    key.clone(),
                    OperationState {
                        generation: 1,
                        phase: OperationPhase::Running,
                    },
                );
                let project = snapshot
                    .thread_list()
                    .unwrap()
                    .projects
                    .into_iter()
                    .find(|project| project.id == "destination")
                    .unwrap();
                assert_eq!(
                    project.error.as_ref().map(|error| error.message.as_str()),
                    expected
                );
                Arc::make_mut(&mut snapshot.operations)
                    .get_mut(&key)
                    .unwrap()
                    .phase = OperationPhase::Failed {
                    message: "read failed".into(),
                };
                let project = snapshot
                    .thread_list()
                    .unwrap()
                    .projects
                    .into_iter()
                    .find(|project| project.id == "destination")
                    .unwrap();
                assert_eq!(
                    project.error.as_ref().map(|error| error.message.as_str()),
                    expected
                );
            }
        }
    }
}

#[test]
fn translated_list_copy_preserves_titles_paths_and_provider_names() {
    use crate::{localization::UiLanguage, presentation::view::Presentation};
    let mut snapshot = Snapshot::default();
    snapshot.lists.threads = Some(Arc::new(serde_json::from_value(serde_json::json!({
        "data":[
            {"id":{"provider":"codex","id":"named"},"name":"設定\n{0}","cwd":"/設定/{0}","listStale":true},
            {"id":{"provider":"codex","id":"unnamed"}},
            {"id":{"provider":"codex","id":"literal"},"name":"無題のタスク"}
        ],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,
        "providerErrors":{"設定":"provider failure"}
    })).unwrap()));
    let japanese = Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap();
    snapshot.catalog.ui_language = UiLanguage::English;
    let english = Presentation::project(Arc::new(snapshot), "", Some(&japanese)).unwrap();
    let list = english.list.as_ref().unwrap();
    assert!(!Arc::ptr_eq(list, japanese.list.as_ref().unwrap()));
    assert_eq!(list.threads[0].title, "設定\n{0} (saved · unverified)");
    assert_eq!(list.threads[0].copy_items[0].label, "Path");
    assert_eq!(
        list.threads[0].copy_items[0].value.as_deref(),
        Some("/設定/{0}")
    );
    assert_eq!(list.threads[1].title, "Untitled task");
    assert_eq!(list.threads[2].title, "無題のタスク");
    assert_eq!(
        list.notice.as_deref(),
        Some(
            "The conversation list is incomplete (設定). Saved results from unavailable providers may be out of date."
        )
    );
    let same = Presentation::project(english.snapshot.clone(), "", Some(&english)).unwrap();
    assert!(Arc::ptr_eq(list, same.list.as_ref().unwrap()));
}
