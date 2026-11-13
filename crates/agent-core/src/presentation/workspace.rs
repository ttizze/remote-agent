use crate::{
    models::WorkspaceControls,
    state::{Snapshot, operations::OperationKey},
};

/// Shared availability for controls that operate on the selected workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceActions {
    pub controls: WorkspaceControls,
    pub can_change: bool,
    pub can_push: bool,
}

pub fn workspace_actions(snapshot: &Snapshot, pane_id: &str) -> WorkspaceActions {
    let Some(pane) = snapshot.panes.get(pane_id) else {
        return WorkspaceActions::default();
    };
    if snapshot.selected_directory(pane).is_empty() {
        return WorkspaceActions::default();
    }
    let ready = snapshot.connection.connected
        && !snapshot.operation_running(OperationKey::WorkspaceAction {
            pane: pane_id.into(),
        });
    let review = pane
        .workspace
        .review
        .as_ref()
        .filter(|_| pane.workspace.review_cwd.as_ref() == Some(&pane.navigation.cwd));
    let controls = review.map(|r| r.controls.clone()).unwrap_or_default();
    WorkspaceActions {
        can_change: ready && review.is_some(),
        can_push: ready && controls.can_push && !controls.remotes.is_empty(),
        controls,
    }
}

/// Offer automatic comparison and exact Git references, including HEAD for
/// uncommitted changes. Keep a selected reference visible if it was deleted.
pub fn comparison_references(controls: &WorkspaceControls, selected: Option<&str>) -> Vec<String> {
    let mut references = vec!["HEAD".into()];
    references.extend(controls.references.iter().cloned());
    if let Some(selected) = selected {
        references.push(selected.into());
    }
    let mut seen = std::collections::BTreeSet::new();
    references.retain(|reference| seen.insert(reference.clone()));
    references
}
