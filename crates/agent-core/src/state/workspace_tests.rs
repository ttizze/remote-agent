use super::*;
use crate::presentation::workspace::{comparison_references, workspace_actions};
use operations::{Operation, OperationKey, OperationPhase, OperationState};

fn workspace() -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    let pane = Arc::make_mut(snapshot.panes.primary_mut());
    Arc::make_mut(&mut pane.navigation).cwd = "/project".into();
    let workspace = Arc::make_mut(&mut pane.workspace);
    workspace.review_cwd = Some("/project".into());
    workspace.review = Some(Arc::new(crate::models::WorkspaceReview {
        options: None,
        controls: crate::models::WorkspaceControls {
            can_push: true,
            remotes: vec!["origin".into()],
            ..Default::default()
        },
        branch: "task".into(),
        base_ref: Some("origin/main".into()),
        additions: 2,
        deletions: 1,
        files: Vec::new(),
        diff: String::new(),
    }));
    snapshot
}

#[test]
fn comparison_survives_refresh_and_stale_panels_cannot_replace_it() {
    let (mut selected, effects) = reduce(
        &workspace(),
        "",
        Event::Intent(Intent::SetWorkspaceComparison {
            cwd: "/project".into(),
            base_ref: Some("HEAD".into()),
        }),
    );
    assert!(!effects.is_empty());
    assert_eq!(
        selected.panes.primary().workspace.comparison.as_deref(),
        Some("HEAD")
    );
    let mut refresh = operations::ReviewWorkspace {
        cwd: "/project".into(),
        base_ref: None,
    };
    refresh.prepare(&mut selected, "").unwrap();
    assert_eq!(refresh.base_ref.as_deref(), Some("HEAD"));
    let (stale, effects) = reduce(
        &selected,
        "",
        Event::Intent(Intent::SetWorkspaceComparison {
            cwd: "/old".into(),
            base_ref: Some("other".into()),
        }),
    );
    assert!(effects.is_empty());
    assert_eq!(
        stale.panes.primary().workspace,
        selected.panes.primary().workspace
    );
    let mut other = operations::ReviewWorkspace {
        cwd: "/old".into(),
        base_ref: Some("main".into()),
    };
    other.prepare(&mut selected, "").unwrap();
    assert_eq!(other.base_ref.as_deref(), Some("main"));
    assert!(selected.panes.primary().workspace.review.is_none());
    let (automatic, _) = reduce(
        &selected,
        "",
        Event::Intent(Intent::SetWorkspaceComparison {
            cwd: "/project".into(),
            base_ref: None,
        }),
    );
    assert!(automatic.panes.primary().workspace.comparison.is_none());
    let (next, _) = reduce(
        &stale,
        "",
        Event::Intent(Intent::NewChat {
            cwd: "/project".into(),
        }),
    );
    assert!(next.panes.primary().workspace.comparison.is_none());
}

#[test]
fn workspace_actions_require_a_current_review_connection_and_idle_action() {
    let initial = workspace();
    let actions = workspace_actions(&initial, "");
    assert!(actions.can_change && actions.can_push);
    assert_eq!(actions.controls.remotes, ["origin"]);
    assert_eq!(workspace_actions(&initial, "closed"), Default::default());
    let mut offline = initial.clone();
    offline.connection.connected = false;
    assert!(!workspace_actions(&offline, "").can_change);
    assert!(!workspace_actions(&offline, "").can_push);
    let mut stale = initial.clone();
    Arc::make_mut(&mut Arc::make_mut(stale.panes.primary_mut()).navigation).cwd = "/other".into();
    assert_eq!(workspace_actions(&stale, ""), Default::default());
    let mut busy = initial.clone();
    busy.operations = Arc::new(
        [(
            OperationKey::WorkspaceAction {
                pane: String::new(),
            },
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        )]
        .into(),
    );
    assert!(!workspace_actions(&busy, "").can_change);
    assert!(!workspace_actions(&busy, "").can_push);
    let controls = crate::models::WorkspaceControls {
        references: vec!["HEAD".into(), "main".into(), "main".into()],
        ..Default::default()
    };
    assert_eq!(
        comparison_references(&controls, Some("removed")),
        ["HEAD", "main", "removed"]
    );
    assert_eq!(
        comparison_references(&controls, Some("main")),
        ["HEAD", "main"]
    );
}

#[test]
fn unassigned_conversation_hides_execution_directory_and_rejects_workspace_controls() {
    use crate::{
        models::{ProjectMembership, Thread},
        session::{ProviderKind, SessionRef},
    };
    let mut snapshot = workspace();
    let id = SessionRef::new(ProviderKind::Codex, "unassigned".into()).unwrap();
    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
        Some(id.clone());
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        id.clone(),
        Arc::new(Thread {
            id: Some(id.clone()),
            cwd: Some("/project".into()),
            project_id: ProjectMembership::Unassigned {},
            ..Default::default()
        }),
    );
    assert_eq!(snapshot.selected_directory(snapshot.panes.primary()), "");
    assert_eq!(workspace_actions(&snapshot, ""), Default::default());
    let mut action = operations::ChangeWorkspace {
        cwd: "/project".into(),
        branch: "task".into(),
        action: agent_protocol::operations::WorkspaceAction::CreateBranch {
            name: "other".into(),
        },
    };
    assert!(action.prepare(&mut snapshot, "").is_err());
    let (mut snapshot, effects) = reduce(
        &snapshot,
        "",
        Event::Intent(Intent::SetWorkspaceComparison {
            cwd: "/project".into(),
            base_ref: Some("HEAD".into()),
        }),
    );
    assert!(effects.is_empty());
    assert!(snapshot.panes.primary().workspace.comparison.is_none());
    assert!(operations::review_workspace(&mut snapshot, "").is_none());
    assert!(snapshot.panes.primary().workspace.review.is_none());
    assert!(snapshot.panes.primary().workspace.review_cwd.is_none());
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&id)
            .unwrap(),
    )
    .project_id = ProjectMembership::Assigned("project".into());
    assert_eq!(
        snapshot.selected_directory(snapshot.panes.primary()),
        "/project"
    );
    assert!(operations::review_workspace(&mut snapshot, "").is_some());
}

#[test]
fn empty_draft_has_no_workspace_controls_or_comparison() {
    let snapshot = Snapshot::default();
    assert_eq!(workspace_actions(&snapshot, ""), Default::default());
    let (next, effects) = reduce(
        &snapshot,
        "",
        Event::Intent(Intent::SetWorkspaceComparison {
            cwd: String::new(),
            base_ref: Some("HEAD".into()),
        }),
    );
    assert!(effects.is_empty());
    assert!(next.panes.primary().workspace.comparison.is_none());
}
