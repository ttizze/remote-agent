use super::*;
use crate::{
    session::{ProviderKind, SessionRef},
    state::{ProjectPreferences, projects::PlacedChat},
};
use std::sync::Arc;

fn id(name: &str) -> SessionRef {
    SessionRef::new(ProviderKind::Codex, name.into()).unwrap()
}

#[test]
fn standalone_rows_keep_unknown_memberships_while_recent_chats_include_assigned_rows() {
    let mut snapshot = Snapshot {
        lists: crate::state::Lists {
            threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[
                    {"id":id("assigned"),"name":"Assigned","projectId":"native"},
                    {"id":id("unknown"),"name":"Unknown project","projectId":"missing","listStale":true},
                    {"id":id("standalone"),"name":"Standalone","projectId":null},
                    {"id":id("placed"),"name":"Placed","projectId":null},
                    {"id":id("empty-project"),"name":"Empty project","projectId":""},
                    {"id":id("child"),"name":"Child","parentId":id("standalone")},
                    {"name":"No identity"}
                ],
                "projects":[{"id":"native","name":"Native","roots":[]},{"id":"","name":"Empty native ID","roots":[]}],
                "hasMore":true,"hasMoreProjects":false,"limit":10
            })).unwrap())),
            ..Default::default()
        },
        ..Default::default()
    };
    Arc::make_mut(&mut snapshot.lists.project_preferences).insert(
        snapshot.connection.storage_scope.clone(),
        [(
            "native".into(),
            ProjectPreferences {
                placed_chats: vec![PlacedChat {
                    id: id("placed"),
                    title: "Saved placed".into(),
                }],
                ..Default::default()
            },
        )]
        .into(),
    );
    Arc::make_mut(&mut snapshot.lists.activity)
        .active
        .insert(id("standalone"), true);
    Arc::make_mut(&mut snapshot.lists.activity)
        .unread
        .insert(id("unknown"));
    let list = snapshot.thread_list().unwrap();
    assert_eq!(
        list.threads.iter().map(|row| &row.id).collect::<Vec<_>>(),
        [&id("unknown"), &id("standalone")]
    );
    assert_eq!(list.threads[0].title, "Unknown project（保存済み・未確認）");
    assert!(list.threads[0].unread && !list.threads[0].active);
    assert!(list.threads[1].active && !list.threads[1].unread);
    assert!(list.threads.iter().all(|row| row.project_id.is_none()));
    assert!(list.has_more);
    assert!(
        list.projects
            .iter()
            .all(|project| project.threads.is_empty())
    );
    let recent = snapshot.recent_thread_list().unwrap();
    assert_eq!(recent.threads.len(), 5);
    for (name, project) in [
        ("assigned", Some("native")),
        ("unknown", None),
        ("standalone", None),
        ("placed", Some("native")),
        ("empty-project", Some("")),
    ] {
        assert_eq!(
            recent
                .threads
                .iter()
                .find(|row| row.id == id(name))
                .unwrap()
                .project_id
                .as_deref(),
            project
        );
    }
    assert!(recent.projects == list.projects && recent.hidden_projects == list.hidden_projects);
    assert!(recent.has_more);
    snapshot.lists.threads = None;
    assert!(snapshot.thread_list().is_none());
    assert!(snapshot.recent_thread_list().is_none());
}
