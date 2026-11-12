//! Native form data; the application owns its lifetime and Core owns Host operations.
use crate::{Connection, application::WorktreeAction};
use nsh_core::prelude::*;
use serde_json::{Value, json};

pub(crate) fn presenter(open_chat: impl Fn() + 'static) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    native("bex.details")
        .prop("data", move || {
            connection.state.with(|state| {
                let editor = state
                    .worktrees
                    .as_ref()
                    .filter(|editor| editor.host == state.host);
                let data = editor.map(|editor| {
                    let p = state.publications.get(&editor.host);
                    editor_data(
                        editor,
                        &state.hosts,
                        p.into_iter()
                            .flat_map(|p| p.list.iter())
                            .flat_map(|list| list.projects.iter().chain(&list.hidden_projects))
                            .map(|project| (project.id.as_str(), project.name.as_str())),
                        p.and_then(|p| p.snapshot.files.workspace.worktrees.as_deref())
                            .map(Vec::as_slice),
                        &p.map(|p| p.context.selected_directory())
                            .unwrap_or_default(),
                        p.is_some_and(|p| p.settings.connected()),
                    )
                });
                json!({"worktreeEditor": {"data": data}})
            })
        })
        .on_native_event(move |event: &NativeEvent| {
            let (Some(token), Some(action)) =
                (event.data["token"].as_str(), event.data["event"].as_str())
            else {
                return;
            };
            let Ok(action) = serde_json::from_str::<WorktreeAction>(action) else {
                return;
            };
            if connection
                .application
                .with(|app| app.worktree_action(token, action))
            {
                open_chat();
            }
        })
        .w(px(0.0))
        .h(px(0.0))
}

pub(crate) fn open() {
    use_context::<Connection>()
        .unwrap()
        .application
        .with(|app| app.open_worktrees());
}

fn editor_data<'a>(
    editor: &crate::application::WorktreeEditor,
    hosts: &[crate::application::Host],
    projects: impl Iterator<Item = (&'a str, &'a str)>,
    rows: Option<&[agent_protocol::models::Worktree]>,
    cwd: &str,
    connected: bool,
) -> Value {
    json!({"editor": editor,
        "hosts": hosts.iter().map(|host| json!({"id": host.id, "name": host.name})).collect::<Vec<_>>(),
        "projects": projects.map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "connected": connected,
        "rows": rows.map(|rows| rows.iter().map(|row| json!({
            "path": row.path, "project": agent_core::presentation::markdown::file_basename(&row.project_path),
            "branch": row.branch, "threads": row.threads,
            "reason": agent_core::presentation::worktrees::worktree_removal_reason(row.blocked_reason.clone(), cwd.into(), row.path.clone()),
        })).collect::<Vec<_>>()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{Host, WorktreeEditor};
    use agent_protocol::models::{Worktree, WorktreeSettings, WorktreeThread};

    #[test]
    fn worktree_native_contract_preserves_scopes_settings_and_removal_guards() {
        let editor = WorktreeEditor {
            id: "fixture-editor".into(),
            host: "remote".into(),
            pending: false,
            error: Some("コピー対象を確認してください。".into()),
            receipt: None,
            settings: Some(WorktreeSettings {
                create_on_new_session: true,
                copy_on_create: true,
                copy_paths: vec![".env".into(), "config/日本語".into()],
                worktree_directory: "/fixture/work trees".into(),
                delete_unchanged: false,
                project_delete_unchanged: [("project".into(), true)].into(),
                ..Default::default()
            }),
        };
        let row = Worktree {
            path: "/fixture/work trees/作業".into(),
            project_path: "/fixture/日本語 👋".into(),
            branch: "feature/native".into(),
            blocked_reason: None,
            threads: vec![WorktreeThread {
                id: serde_json::from_value(json!({"provider": "codex", "id": "chat"})).unwrap(),
                name: "作業中の会話".into(),
                active: true,
            }],
        };
        let data = editor_data(
            &editor,
            &[Host {
                id: "remote".into(),
                name: "検証環境 👋".into(),
            }],
            [("project", "日本語 👋")].into_iter(),
            Some(&[row]),
            "/fixture/work trees/作業/src",
            true,
        );
        let fixture: Value = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/worktree-editor.json"
        ))
        .unwrap();
        assert_eq!(json!({"worktreeEditor": {"data": data}}), fixture);
        let empty = editor_data(&editor, &[], std::iter::empty(), None, "", false);
        assert_eq!(empty["rows"], Value::Null);
        assert_eq!(empty["connected"], false);
    }

    #[test]
    fn swift_actions_decode_into_typed_host_operations() {
        let actions: Vec<WorktreeAction> = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/worktree-actions.json"
        ))
        .unwrap();
        assert!(
            matches!(&actions[0], WorktreeAction::Save {id, settings} if id == "submission" && settings.copy_paths == ["  .env ", "", "config/日本語"] && settings.project_delete_unchanged["project"])
        );
        assert!(matches!(&actions[1], WorktreeAction::Open {thread} if thread.id == "chat"));
        assert!(
            matches!(&actions[2], WorktreeAction::Remove {path} if path == "/fixture/work trees/作業")
        );
        assert!(matches!(&actions[3], WorktreeAction::Load {}));
        assert!(matches!(&actions[4], WorktreeAction::Close {}));
        assert!(matches!(&actions[5], WorktreeAction::Host {id} if id == "remote"));
        assert!(
            matches!(&actions[6], WorktreeAction::Review {path} if path == "/fixture/work trees/作業")
        );
    }
}
