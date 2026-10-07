//! The new-thread screen: the hero headline over the draft's composer, and
//! the workspace and branch the first run uses.
use crate::commands::build::WorkspaceChoice;
use crate::state::{RefScope, Snapshot};
use crate::view::composer::hero::{DraftHeroHeadline, DraftHeroInput, draft_hero_state};
use crate::view::composer::view::{ComposerOptions, ComposerView, composer_view};
use crate::view::projects::selection::{
    ThreadWorkspaceMode, resolve_project_thread_creation_branch,
};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NewThreadView {
    pub hero: DraftHeroHeadline,
    /// The composer sits centred under the headline.
    pub show_hero: bool,
    pub composer: ComposerView,
    pub project_id: Option<String>,
    /// The thread this draft's send is creating; it opens once the Host has it.
    pub launching_thread_id: Option<String>,
    /// Absent for a thread without a project, which always runs locally.
    pub workspace: Option<NewThreadWorkspaceView>,
}

/// One branch the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BranchChoice {
    pub name: String,
    pub current: bool,
    pub is_default: bool,
    /// The worktree that has it checked out.
    pub worktree_path: Option<String>,
    pub selected: bool,
}

/// The new-thread workspace and branch controls.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NewThreadWorkspaceView {
    pub mode: ThreadWorkspaceMode,
    /// "Current checkout", "Current worktree" or "New worktree".
    pub workspace_label: String,
    /// The draft runs in an existing worktree of the project.
    pub in_worktree: bool,
    /// "Choose branch", the branch, or "From <base>" for a new worktree.
    pub branch_label: String,
    /// "Branch", or "Base branch" for a new worktree.
    pub branch_role: String,
    /// "Loading branches…" shows instead of the label.
    pub branches_loading: bool,
    /// The project's local branches matching the picker's query.
    pub branches: Vec<BranchChoice>,
    /// More branches follow (`Intent::LoadMoreNewThreadBranches`).
    pub has_more_branches: bool,
    pub branches_loading_more: bool,
    pub start_from_origin: bool,
    pub branch_error: Option<String>,
    /// Why the draft cannot start yet, e.g. a worktree without a base branch.
    pub blocked_reason: Option<String>,
}

/// "Current checkout" or "Current worktree" locally; "New worktree" otherwise.
pub fn new_task_workspace_label(
    mode: ThreadWorkspaceMode,
    worktree_path: Option<&str>,
) -> &'static str {
    match (mode, worktree_path) {
        (ThreadWorkspaceMode::Worktree, _) => "New worktree",
        (ThreadWorkspaceMode::Local, Some(_)) => "Current worktree",
        (ThreadWorkspaceMode::Local, None) => "Current checkout",
    }
}

/// The worktree a local thread on `branch` runs in; none for a new worktree or
/// the project's own checkout.
pub fn branch_worktree_path(
    mode: ThreadWorkspaceMode,
    project_root: &str,
    branch_worktree_path: Option<&str>,
) -> Option<String> {
    match (mode, branch_worktree_path) {
        (ThreadWorkspaceMode::Local, Some(path)) if !path.is_empty() && path != project_root => {
            Some(path.into())
        }
        _ => None,
    }
}

/// "Choose branch", the local branch, or `From [origin/]<base>`.
pub fn new_task_branch_label(
    branch: Option<&str>,
    start_from_origin: bool,
    mode: ThreadWorkspaceMode,
) -> String {
    match (branch, mode) {
        (None, _) => "Choose branch".into(),
        (Some(branch), ThreadWorkspaceMode::Local) => branch.into(),
        (Some(branch), ThreadWorkspaceMode::Worktree) if start_from_origin => {
            format!("From origin/{branch}")
        }
        (Some(branch), ThreadWorkspaceMode::Worktree) => format!("From {branch}"),
    }
}

/// The branch a new thread works on: the picked one; for a new worktree the
/// default branch, else the current one; locally the checked-out branch.
pub(crate) fn new_thread_branch(snapshot: &Snapshot) -> Option<String> {
    let workspace = snapshot.new_thread_workspace();
    let root = snapshot.new_thread_project_root()?;
    let refs = snapshot
        .sources
        .refs(&root, RefScope::All)
        .and_then(|entry| entry.list.as_ref());
    let fallback = || {
        let refs = refs?;
        refs.refs
            .iter()
            .find(|branch| branch.is_default)
            .or_else(|| {
                refs.refs
                    .iter()
                    .find(|branch| branch.current && !branch.is_remote)
            })
            .map(|branch| branch.name.clone())
    };
    let selected = workspace.branch.clone().or_else(|| match workspace.mode {
        ThreadWorkspaceMode::Worktree => fallback(),
        ThreadWorkspaceMode::Local => None,
    });
    let checkout = snapshot
        .sources
        .vcs_status
        .get(&root)
        .and_then(|status| status.ref_name.as_deref());
    resolve_project_thread_creation_branch(workspace.mode, selected.as_deref(), checkout)
}

/// Where the new thread's first run works.
pub(crate) fn new_thread_launch_workspace(snapshot: &Snapshot) -> Result<WorkspaceChoice, String> {
    if snapshot.new_thread_project_root().is_none() {
        return Ok(WorkspaceChoice::Local {
            branch: None,
            worktree_path: None,
        });
    }
    let workspace = snapshot.new_thread_workspace();
    let branch = new_thread_branch(snapshot);
    match workspace.mode {
        ThreadWorkspaceMode::Local => Ok(WorkspaceChoice::Local {
            branch,
            worktree_path: workspace.worktree_path,
        }),
        ThreadWorkspaceMode::Worktree => match branch {
            Some(base_branch) => Ok(WorkspaceChoice::NewWorktree {
                base_branch,
                branch: None,
                start_from_origin: workspace.start_from_origin,
            }),
            None => Err("Select a base branch before creating a worktree.".into()),
        },
    }
}

fn workspace_view(snapshot: &Snapshot) -> Option<NewThreadWorkspaceView> {
    let root = snapshot.new_thread_project_root()?;
    let workspace = snapshot.new_thread_workspace();
    let refs = snapshot.sources.refs(&root, RefScope::All);
    let branch = new_thread_branch(snapshot);
    let branches: Vec<BranchChoice> = refs
        .and_then(|entry| entry.list.as_ref())
        .map(|list| {
            list.refs
                .iter()
                .filter(|candidate| !candidate.is_remote)
                .map(|candidate| BranchChoice {
                    selected: branch.as_deref() == Some(candidate.name.as_str()),
                    name: candidate.name.clone(),
                    current: candidate.current,
                    is_default: candidate.is_default,
                    worktree_path: candidate.worktree_path.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let loading = refs.is_some_and(|entry| entry.in_flight) && branches.is_empty();
    Some(NewThreadWorkspaceView {
        mode: workspace.mode,
        workspace_label: new_task_workspace_label(
            workspace.mode,
            workspace.worktree_path.as_deref(),
        )
        .into(),
        in_worktree: workspace.mode == ThreadWorkspaceMode::Worktree
            || workspace.worktree_path.is_some(),
        branch_label: if loading {
            "Loading branches…".into()
        } else {
            new_task_branch_label(
                branch.as_deref(),
                workspace.start_from_origin,
                workspace.mode,
            )
        },
        branch_role: match workspace.mode {
            ThreadWorkspaceMode::Local => "Branch",
            ThreadWorkspaceMode::Worktree => "Base branch",
        }
        .into(),
        branches_loading: loading,
        has_more_branches: refs
            .and_then(|entry| entry.list.as_ref())
            .is_some_and(|list| list.next_cursor.is_some()),
        branches_loading_more: refs.is_some_and(|entry| entry.in_flight) && !branches.is_empty(),
        branches,
        start_from_origin: workspace.start_from_origin,
        branch_error: refs.and_then(|entry| entry.error.clone()),
        blocked_reason: new_thread_launch_workspace(snapshot).err(),
    })
}

pub fn new_thread_view(snapshot: &Snapshot, options: &ComposerOptions) -> NewThreadView {
    let key = snapshot.new_thread_draft_key();
    let launching = snapshot
        .outbox
        .pending_launches()
        .find(|entry| {
            entry.navigate
                && entry
                    .restore
                    .as_ref()
                    .is_some_and(|restore| restore.draft_key == key)
        })
        .map(|entry| entry.thread.to_string());
    NewThreadView {
        hero: snapshot.draft_hero_headline(),
        show_hero: draft_hero_state(DraftHeroInput {
            is_draft: true,
            dock_requested: launching.is_some(),
            ..DraftHeroInput::default()
        }),
        composer: composer_view(snapshot, None, options),
        project_id: snapshot.selected_project.clone(),
        launching_thread_id: launching,
        workspace: workspace_view(snapshot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Project, ProjectRoot};
    use crate::view::composer::hero::DraftHeroHeadlineKind;
    use crate::view::composer::prompt::DEFAULT_COMPOSER_PLACEHOLDER;

    #[test]
    fn a_new_thread_draft_shows_the_hero_over_its_composer() {
        let mut snapshot = Snapshot {
            selected_project: Some("app".into()),
            ..Snapshot::default()
        };
        snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
            agent_protocol::conversation::ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![Project {
                    id: "app".into(),
                    name: "App".into(),
                    roots: vec![ProjectRoot {
                        path: "/repo".into(),
                    }],
                    scripts: vec![],
                    repository_identity: None,
                    favicon_path: None,
                    created_at: None,
                    updated_at: None,
                }],
                threads: vec![],
            },
        ));
        snapshot.drafts.insert(
            "new:app".into(),
            crate::state::Draft {
                text: "Sketch the API".into(),
                ..Default::default()
            },
        );
        let view = new_thread_view(&snapshot, &ComposerOptions::default());
        assert!(view.show_hero);
        assert_eq!(view.hero.kind, DraftHeroHeadlineKind::BuildIn);
        assert_eq!(view.hero.heading_label, "What should we build in App?");
        assert_eq!(view.project_id.as_deref(), Some("app"));
        assert_eq!(view.composer.draft_key, "new:app");
        assert_eq!(view.composer.text, "Sketch the API");
        assert_eq!(
            view.composer.editor.placeholder,
            DEFAULT_COMPOSER_PLACEHOLDER
        );
        assert_eq!(view.composer.context_meter, None);
        assert_eq!(view.launching_thread_id, None);
        let workspace = view.workspace.unwrap();
        assert_eq!(workspace.workspace_label, "Current checkout");
        assert_eq!(workspace.branch_label, "Choose branch");
    }

    fn vcs_ref(
        name: &str,
        current: bool,
        default: bool,
        worktree: Option<&str>,
    ) -> agent_protocol::workspace::VcsRef {
        agent_protocol::workspace::VcsRef {
            name: name.into(),
            is_remote: false,
            remote_name: None,
            current,
            is_default: default,
            worktree_path: worktree.map(Into::into),
        }
    }

    fn project_snapshot() -> Snapshot {
        let mut snapshot = Snapshot {
            selected_project: Some("app".into()),
            ..Snapshot::default()
        };
        snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
            agent_protocol::conversation::ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![Project {
                    id: "app".into(),
                    name: "App".into(),
                    roots: vec![ProjectRoot {
                        path: "/repo".into(),
                    }],
                    ..Project::default()
                }],
                threads: vec![],
            },
        ));
        snapshot.sources.refs.insert(
            ("/repo".into(), RefScope::All),
            crate::state::RefsEntry {
                query: String::new(),
                list: Some(agent_protocol::workspace::RefList {
                    refs: vec![
                        vcs_ref("feature", true, false, Some("/repo")),
                        vcs_ref("main", false, true, None),
                        vcs_ref("fix", false, false, Some("/trees/fix")),
                    ],
                    is_repo: true,
                    has_primary_remote: true,
                    next_cursor: None,
                    total_count: 3,
                }),
                error: None,
                in_flight: false,
            },
        );
        snapshot
    }

    // new-task-context-presentation.ts and projectThreadCreationValidation.ts.
    #[test]
    fn a_local_draft_works_on_the_checked_out_branch_and_a_worktree_starts_from_the_default() {
        let mut snapshot = project_snapshot();
        let local = workspace_view(&snapshot).unwrap();
        assert_eq!(
            (local.workspace_label.as_str(), local.branch_label.as_str()),
            ("Current checkout", "feature")
        );
        assert!(
            local
                .branches
                .iter()
                .any(|branch| branch.name == "feature" && branch.selected)
        );
        assert!(matches!(
            new_thread_launch_workspace(&snapshot),
            Ok(WorkspaceChoice::Local { branch: Some(branch), worktree_path: None }) if branch == "feature"
        ));
        snapshot.drafts.insert(
            "new:app".into(),
            crate::state::Draft {
                workspace: Some(crate::state::DraftWorkspace {
                    mode: ThreadWorkspaceMode::Worktree,
                    branch: None,
                    worktree_path: None,
                    start_from_origin: true,
                }),
                ..Default::default()
            },
        );
        let worktree = workspace_view(&snapshot).unwrap();
        assert_eq!(
            (
                worktree.workspace_label.as_str(),
                worktree.branch_label.as_str(),
                worktree.branch_role.as_str()
            ),
            ("New worktree", "From origin/main", "Base branch")
        );
        assert!(matches!(
            new_thread_launch_workspace(&snapshot),
            Ok(WorkspaceChoice::NewWorktree { base_branch, start_from_origin: true, .. }) if base_branch == "main"
        ));
        snapshot.sources.refs.clear();
        assert_eq!(
            new_thread_launch_workspace(&snapshot).err().as_deref(),
            Some("Select a base branch before creating a worktree.")
        );
    }

    #[test]
    fn a_branch_checked_out_in_another_worktree_runs_there_locally() {
        assert_eq!(
            branch_worktree_path(ThreadWorkspaceMode::Local, "/repo", Some("/trees/fix"))
                .as_deref(),
            Some("/trees/fix")
        );
        assert_eq!(
            branch_worktree_path(ThreadWorkspaceMode::Local, "/repo", Some("/repo")),
            None
        );
        assert_eq!(
            branch_worktree_path(ThreadWorkspaceMode::Worktree, "/repo", Some("/trees/fix")),
            None
        );
        assert_eq!(
            new_task_workspace_label(ThreadWorkspaceMode::Local, Some("/trees/fix")),
            "Current worktree"
        );
        assert_eq!(
            new_task_branch_label(Some("main"), false, ThreadWorkspaceMode::Worktree),
            "From main"
        );
    }
}
