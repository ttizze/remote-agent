//! The thread header: the project crumb and title, the rename rule, and the
//! panel toggles and header actions with their availability and labels.
use crate::commands::workflows::latest_merge_back_run;
use crate::models::Project;
use crate::state::Snapshot;
use agent_domain::{ThreadId, Workspace};

pub const EMPTY_TITLE_WARNING: &str = "Thread title cannot be empty";
pub const RENAME_FAILED: &str = "Failed to rename thread";

/// What committing an inline rename does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RenameCommit {
    Commit {
        title: String,
    },
    /// The caller warns with [`EMPTY_TITLE_WARNING`].
    RejectEmpty,
    Noop,
}

/// Trim, reject an empty title, and skip a title that did not change.
pub fn resolve_rename_commit(title: &str, original_title: &str) -> RenameCommit {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        RenameCommit::RejectEmpty
    } else if trimmed == original_title {
        RenameCommit::Noop
    } else {
        RenameCommit::Commit {
            title: trimmed.into(),
        }
    }
}

/// The project crumb that leads the header; pressing it starts a new thread
/// in the project.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectCrumb {
    pub id: String,
    pub name: String,
    /// "New thread in <project>", the crumb's accessibility label and tooltip.
    pub new_thread_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PanelToggle {
    pub available: bool,
    pub open: bool,
    pub accessibility_label: String,
    /// Clients append their shortcut, as in "Toggle terminal drawer (⌘J)",
    /// while the toggle is available.
    pub tooltip: String,
    /// A dot on the toggle that asks the user to look inside.
    pub attention: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum HeaderActionKind {
    Files,
    Terminal,
    MergeBack,
}

/// A compact header button.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HeaderAction {
    pub kind: HeaderActionKind,
    pub accessibility_label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadHeaderView {
    pub title: String,
    pub project: Option<ProjectCrumb>,
    /// "<project> · <environment>", for compact headers.
    pub subtitle: String,
    /// A thread the Host has; a draft has no action menu.
    pub is_server_thread: bool,
    /// "Thread actions for <title>": the title opens the action menu.
    pub title_menu_label: Option<String>,
    /// The worktree, or else the project root, that files and terminals open in.
    pub cwd: Option<String>,
    pub thread_panel: PanelToggle,
    pub terminal: PanelToggle,
    pub right_panel: PanelToggle,
    /// Compact header buttons in order: files, terminal, merge back.
    pub actions: Vec<HeaderAction>,
}

/// Panels the client has open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HeaderPanelState {
    pub thread_panel_open: bool,
    pub terminal_open: bool,
    pub right_panel_open: bool,
    pub files_open: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct HeaderInput<'a> {
    pub title: &'a str,
    pub project: Option<&'a Project>,
    pub workspace: Option<&'a Workspace>,
    pub is_server_thread: bool,
    pub environment_label: Option<&'a str>,
    pub environment_unavailable: bool,
    /// The thread is a fork with a finished run to merge back.
    pub merge_back_available: bool,
}

pub fn thread_header(input: &HeaderInput<'_>, panels: &HeaderPanelState) -> ThreadHeaderView {
    let project_root = input
        .project
        .and_then(|project| project.roots.first())
        .map(|root| root.path.clone())
        .filter(|path| !path.is_empty());
    let cwd = input
        .workspace
        .and_then(|workspace| {
            workspace
                .worktree_path
                .clone()
                .or_else(|| Some(workspace.cwd.clone()))
        })
        .filter(|path| !path.is_empty())
        .or_else(|| project_root.clone());
    let has_project = input.project.is_some();
    let mut actions = vec![];
    if cwd.is_some() {
        actions.push(HeaderAction {
            kind: HeaderActionKind::Files,
            accessibility_label: if panels.files_open {
                "Close files"
            } else {
                "Open files"
            }
            .into(),
            selected: panels.files_open,
        });
    }
    if project_root.is_some() {
        actions.push(HeaderAction {
            kind: HeaderActionKind::Terminal,
            accessibility_label: "Open terminal".into(),
            selected: false,
        });
    }
    if input.merge_back_available {
        actions.push(HeaderAction {
            kind: HeaderActionKind::MergeBack,
            accessibility_label: "Merge back to source".into(),
            selected: false,
        });
    }
    ThreadHeaderView {
        title: input.title.into(),
        project: input.project.map(|project| ProjectCrumb {
            id: project.id.clone(),
            name: project.name.clone(),
            new_thread_label: format!("New thread in {}", project.name),
        }),
        subtitle: [
            input.project.map(|project| project.name.as_str()),
            input.environment_label,
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · "),
        is_server_thread: input.is_server_thread,
        title_menu_label: input
            .is_server_thread
            .then(|| format!("Thread actions for {}", input.title)),
        cwd,
        thread_panel: PanelToggle {
            available: true,
            open: panels.thread_panel_open,
            accessibility_label: "Toggle thread details panel".into(),
            tooltip: "Toggle thread details".into(),
            attention: input.environment_unavailable,
        },
        terminal: PanelToggle {
            available: has_project,
            open: panels.terminal_open,
            accessibility_label: "Toggle terminal drawer".into(),
            tooltip: if has_project {
                "Toggle terminal drawer"
            } else {
                "Terminal drawer is unavailable"
            }
            .into(),
            attention: false,
        },
        right_panel: PanelToggle {
            available: has_project,
            open: panels.right_panel_open,
            accessibility_label: "Toggle right panel".into(),
            tooltip: if has_project {
                "Toggle right panel"
            } else {
                "Right panel is unavailable"
            }
            .into(),
            attention: false,
        },
        actions,
    }
}

/// The header of a thread the Host has, from its folded state or else its
/// list row.
pub fn snapshot_thread_header(
    snapshot: &Snapshot,
    thread: &ThreadId,
    panels: &HeaderPanelState,
) -> Option<ThreadHeaderView> {
    let state = snapshot.thread_state(thread);
    let (title, project, workspace) = match state.and_then(|state| state.thread.as_ref()) {
        Some(thread) => (&thread.title, &thread.project, thread.workspace.as_ref()),
        None => {
            let row = snapshot.thread_row(thread)?;
            (&row.title, &row.project, row.workspace.as_ref())
        }
    };
    let merge_back_available = state.is_some_and(|state| {
        state
            .thread
            .as_ref()
            .is_some_and(|thread| thread.parent.is_some() && thread.fork_boundary.is_some())
            && latest_merge_back_run(state).is_some()
    });
    Some(thread_header(
        &HeaderInput {
            title,
            project: snapshot
                .shell_projects()
                .iter()
                .find(|candidate| &candidate.id == project),
            workspace,
            is_server_thread: true,
            environment_label: snapshot.host_name.as_deref(),
            environment_unavailable: !snapshot.connected,
            merge_back_available,
        },
        panels,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProjectRoot;

    #[test]
    fn commits_a_trimmed_changed_title() {
        assert_eq!(
            resolve_rename_commit("  New title ", "Old"),
            RenameCommit::Commit {
                title: "New title".into()
            }
        );
    }

    #[test]
    fn rejects_empty_and_whitespace_only_titles() {
        assert_eq!(
            resolve_rename_commit("   ", "Old"),
            RenameCommit::RejectEmpty
        );
    }

    #[test]
    fn no_ops_when_the_trimmed_title_is_unchanged() {
        assert_eq!(resolve_rename_commit(" Old ", "Old"), RenameCommit::Noop);
    }

    fn project(root: &str) -> Project {
        Project {
            id: "app".into(),
            name: "App".into(),
            roots: vec![ProjectRoot { path: root.into() }],
            scripts: vec![],
            repository_identity: None,
            favicon_path: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn input<'a>(
        project: Option<&'a Project>,
        workspace: Option<&'a Workspace>,
    ) -> HeaderInput<'a> {
        HeaderInput {
            title: "Fix the build",
            project,
            workspace,
            is_server_thread: true,
            environment_label: Some("Mac mini"),
            environment_unavailable: false,
            merge_back_available: false,
        }
    }

    #[test]
    fn the_project_leads_the_header_and_opens_a_new_thread() {
        let project = project("/repo");
        let header = thread_header(&input(Some(&project), None), &HeaderPanelState::default());
        assert_eq!(
            header.project,
            Some(ProjectCrumb {
                id: "app".into(),
                name: "App".into(),
                new_thread_label: "New thread in App".into(),
            })
        );
        assert_eq!(header.subtitle, "App · Mac mini");
        assert_eq!(
            header.title_menu_label.as_deref(),
            Some("Thread actions for Fix the build")
        );
        assert!(header.terminal.available && header.right_panel.available);
        assert_eq!(header.terminal.tooltip, "Toggle terminal drawer");
        assert_eq!(header.cwd.as_deref(), Some("/repo"));
        assert_eq!(
            header
                .actions
                .iter()
                .map(|action| (action.kind, action.accessibility_label.as_str()))
                .collect::<Vec<_>>(),
            [
                (HeaderActionKind::Files, "Open files"),
                (HeaderActionKind::Terminal, "Open terminal"),
            ]
        );
    }

    #[test]
    fn a_draft_title_has_no_action_menu() {
        let header = thread_header(
            &HeaderInput {
                is_server_thread: false,
                ..input(None, None)
            },
            &HeaderPanelState::default(),
        );
        assert_eq!(header.title_menu_label, None);
    }

    #[test]
    fn panels_need_a_project() {
        let header = thread_header(&input(None, None), &HeaderPanelState::default());
        assert!(!header.terminal.available && !header.right_panel.available);
        assert_eq!(header.terminal.tooltip, "Terminal drawer is unavailable");
        assert_eq!(header.right_panel.tooltip, "Right panel is unavailable");
        assert_eq!(header.subtitle, "Mac mini");
        assert!(header.actions.is_empty());
        assert_eq!(header.cwd, None);
    }

    #[test]
    fn files_open_in_the_thread_worktree() {
        let project = project("/repo");
        let workspace = Workspace {
            cwd: "/repo".into(),
            worktree_path: Some("/worktrees/fix".into()),
            branch: Some("fix".into()),
        };
        let header = thread_header(
            &HeaderInput {
                merge_back_available: true,
                environment_unavailable: true,
                ..input(Some(&project), Some(&workspace))
            },
            &HeaderPanelState {
                files_open: true,
                ..HeaderPanelState::default()
            },
        );
        assert_eq!(header.cwd.as_deref(), Some("/worktrees/fix"));
        assert!(header.thread_panel.attention);
        assert_eq!(
            header
                .actions
                .iter()
                .map(|action| (
                    action.kind,
                    action.accessibility_label.as_str(),
                    action.selected
                ))
                .collect::<Vec<_>>(),
            [
                (HeaderActionKind::Files, "Close files", true),
                (HeaderActionKind::Terminal, "Open terminal", false),
                (HeaderActionKind::MergeBack, "Merge back to source", false),
            ]
        );
    }

    #[test]
    fn a_fork_with_a_finished_run_offers_merge_back() {
        use crate::sync::{ThreadSync, fixtures};
        use agent_domain::RunStatus;
        use std::sync::Arc;
        let mut state = fixtures::thread_state("Fork");
        state
            .runs
            .push(fixtures::run("one", 1, RunStatus::Completed));
        let thread = state.thread.as_mut().unwrap();
        thread.project = "app".into();
        thread.parent = Some(ThreadId::new("source").unwrap());
        let snapshot_with = |state: agent_domain::State| {
            let mut sync = ThreadSync::default();
            sync.state = Some(Arc::new(state));
            let mut snapshot = Snapshot {
                host_name: Some("Mac mini".into()),
                connected: true,
                ..Snapshot::default()
            };
            snapshot
                .threads
                .insert(fixtures::thread_id(), Arc::new(sync));
            snapshot
        };
        let merge_back = |snapshot: &Snapshot| {
            snapshot_thread_header(
                snapshot,
                &fixtures::thread_id(),
                &HeaderPanelState::default(),
            )
            .unwrap()
            .actions
            .iter()
            .any(|action| action.kind == HeaderActionKind::MergeBack)
        };
        assert!(!merge_back(&snapshot_with(state.clone())));
        state.thread.as_mut().unwrap().fork_boundary = Some(1);
        let snapshot = snapshot_with(state);
        assert!(merge_back(&snapshot));
        let header = snapshot_thread_header(
            &snapshot,
            &fixtures::thread_id(),
            &HeaderPanelState::default(),
        )
        .unwrap();
        assert_eq!(header.title, "Fork");
        assert_eq!(header.subtitle, "Mac mini");
        assert!(!header.thread_panel.attention);
        assert_eq!(
            snapshot_thread_header(
                &snapshot,
                &ThreadId::new("missing").unwrap(),
                &HeaderPanelState::default()
            ),
            None
        );
    }
}
