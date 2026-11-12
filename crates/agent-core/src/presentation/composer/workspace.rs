use super::*;
use agent_protocol::workspace::WorkspaceStrategy;

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkspaceSelection {
    pub mode: String,
    pub label: String,
    pub base_ref: String,
    pub start_from_origin: bool,
    pub existing_path: String,
    pub refs: Vec<String>,
    pub worktrees: Vec<String>,
    pub is_repository: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_draft_choices_override_host_defaults_and_are_hidden_for_existing_conversations() {
        let mut snapshot = Snapshot::default();
        let mut pane = crate::state::PaneState::default();
        Arc::make_mut(&mut pane.navigation).cwd = "/project".into();
        Arc::make_mut(&mut snapshot.files.workspace).options.insert(
            "/project".into(),
            Arc::new(agent_protocol::workspace::WorkspaceOptions {
                is_repository: true,
                default_worktree: true,
                default_base: "origin/main".into(),
                refs: vec!["origin/main".into(), "feature/local".into()],
                worktrees: vec!["/existing".into()],
            }),
        );
        let mut draft = Draft::default();
        let automatic = project(&snapshot, &pane, &draft).unwrap();
        assert_eq!(automatic.mode, "worktree");
        assert_eq!(automatic.base_ref, "origin/main");
        assert!(automatic.start_from_origin);
        draft.workspace = Some(WorkspaceStrategy::Root);
        assert_eq!(project(&snapshot, &pane, &draft).unwrap().mode, "root");
        draft.workspace = Some(WorkspaceStrategy::Worktree {
            base_ref: "feature/local".into(),
            start_from_origin: false,
        });
        let local = project(&snapshot, &pane, &draft).unwrap();
        assert_eq!(local.base_ref, "feature/local");
        assert!(!local.start_from_origin);
        draft.workspace = Some(WorkspaceStrategy::ExistingWorktree {
            path: "/existing".into(),
        });
        assert_eq!(
            project(&snapshot, &pane, &draft).unwrap().existing_path,
            "/existing"
        );
        Arc::make_mut(&mut pane.navigation).thread_id = Some(
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "existing".into())
                .unwrap(),
        );
        assert!(project(&snapshot, &pane, &draft).is_none());
        Arc::make_mut(&mut pane.navigation).thread_id = None;
        Arc::make_mut(&mut pane.navigation).cwd.clear();
        assert!(project(&snapshot, &pane, &draft).is_none());
    }
}

pub(super) fn project(
    snapshot: &Snapshot,
    pane: &crate::state::PaneState,
    draft: &Draft,
) -> Option<WorkspaceSelection> {
    if pane.navigation.thread_id.is_some() || pane.navigation.cwd.is_empty() {
        return None;
    }
    let options = snapshot.files.workspace.options.get(&pane.navigation.cwd);
    let default_worktree = options.map_or_else(
        || {
            snapshot
                .files
                .workspace
                .settings
                .as_ref()
                .is_some_and(|settings| settings.create_on_new_session)
        },
        |options| options.default_worktree,
    );
    let default_base = options
        .map(|options| options.default_base.clone())
        .unwrap_or_default();
    let (mode, base, fetch, path, label) = match &draft.workspace {
        Some(WorkspaceStrategy::Root) => ("root", default_base, true, String::new(), "ローカル"),
        Some(WorkspaceStrategy::ExistingWorktree { path }) => (
            "existing",
            default_base,
            false,
            path.clone(),
            "既存のワークツリー",
        ),
        Some(WorkspaceStrategy::Worktree {
            base_ref,
            start_from_origin,
        }) => (
            "worktree",
            if base_ref.is_empty() {
                default_base
            } else {
                base_ref.clone()
            },
            *start_from_origin,
            String::new(),
            "ワークツリー",
        ),
        None if default_worktree => (
            "worktree",
            default_base,
            true,
            String::new(),
            "ワークツリー",
        ),
        None => ("root", default_base, true, String::new(), "ローカル"),
    };
    Some(WorkspaceSelection {
        mode: mode.into(),
        label: snapshot.catalog.ui_language.text(label).into(),
        base_ref: base,
        start_from_origin: fetch,
        existing_path: path,
        refs: options
            .map(|options| options.refs.clone())
            .unwrap_or_default(),
        worktrees: options
            .map(|options| options.worktrees.clone())
            .unwrap_or_default(),
        is_repository: options.is_none_or(|options| options.is_repository),
    })
}
