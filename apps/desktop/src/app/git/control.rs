use agent_core::state::Intent;
use agent_core::view::git::GitAction;
use crate::app::ui;

/// The one intent a toolbar button sends for a stacked action.
pub(super) fn action_intent(
    cwd: String,
    action: GitAction,
    project_id: Option<String>,
    thread_id: Option<String>,
) -> Intent {
    Intent::RunVcsAction {
        action_id: format!("desktop-git:{}", ui::now_ms()),
        cwd,
        action: action.as_str().into(),
        commit_message: None,
        feature_branch: false,
        file_paths: None,
        thread_id,
        project_id,
    }
}

pub(super) fn pull_intent(cwd: String) -> Intent {
    Intent::PullVcs { cwd }
}
