//! One transient search per pane, independent of Explorer state and ownership.
use super::*;
use crate::composer::completions::{
    PathSearch, PathSelection, PathTarget, insert_path, path_query, path_request, path_root,
    path_search_available,
};
use agent_protocol::file_tree::TreeEntries;

pub(crate) fn query_composer_paths(
    snapshot: &mut Snapshot,
    pane_id: &str,
    draft: &DraftKey,
    text: &str,
    cursor: u32,
) -> Vec<Effect> {
    let pane = snapshot.panes.get(pane_id).expect("admitted pane exists");
    let query = path_query(text, cursor).map(|(_, query)| crate::text::trim(query));
    let active = snapshot.composer_key(
        pane.navigation.thread_id.as_ref(),
        &pane.navigation.draft_key,
    );
    if *draft != active {
        return Vec::new();
    }
    let root = path_root(
        &pane.navigation.cwd,
        pane.navigation.thread_id.is_some(),
        snapshot
            .input
            .drafts
            .get(draft)
            .and_then(|draft| draft.workspace.as_ref()),
    );
    let target = query
        .filter(|query| path_search_available(snapshot.connection.connected, root, query))
        .map(|query| PathTarget {
            draft: draft.clone(),
            root: root.into(),
            query: query.into(),
        });
    // A no-op read must retain the publication's identity as well as its values.
    if pane
        .composer_paths
        .as_ref()
        .map(|search| search.target.as_ref())
        == target.as_ref()
    {
        return Vec::new();
    }
    let pane = Arc::make_mut(
        snapshot
            .panes
            .get_mut(pane_id)
            .expect("admitted pane exists"),
    );
    let Some(target) = target else {
        pane.composer_paths = None;
        return Vec::new();
    };
    let target = Arc::new(target);
    pane.composer_paths = Some(Arc::new(PathSearch {
        target: target.clone(),
        result: None,
    }));
    vec![Effect::execute(DebounceComposerPaths(target))]
}

pub(crate) fn insert_composer_path(
    snapshot: &mut Snapshot,
    pane_id: &str,
    expected_text: String,
    cursor: u32,
    selection: PathSelection,
) {
    let Some(draft) = snapshot.input.drafts.get(&selection.search.draft) else {
        return;
    };
    if draft.text != expected_text {
        return;
    }
    let pane = snapshot.panes.get(pane_id).expect("admitted pane exists");
    let active = snapshot.composer_key(
        pane.navigation.thread_id.as_ref(),
        &pane.navigation.draft_key,
    );
    let root = path_root(
        &pane.navigation.cwd,
        pane.navigation.thread_id.is_some(),
        draft.workspace.as_ref(),
    );
    if let Some(insertion) = insert_path(&expected_text, cursor, &active, root, &selection) {
        crate::state::set_draft_text(snapshot, selection.search.draft, insertion.text);
    }
}

fn current(snapshot: &Snapshot, pane: &str, target: &Arc<PathTarget>) -> bool {
    snapshot.connection.connected
        && snapshot.panes.get(pane).is_some_and(|pane| {
            path_root(
                &pane.navigation.cwd,
                pane.navigation.thread_id.is_some(),
                snapshot
                    .input
                    .drafts
                    .get(&target.draft)
                    .and_then(|draft| draft.workspace.as_ref()),
            ) == target.root
                && snapshot.composer_key(
                    pane.navigation.thread_id.as_ref(),
                    &pane.navigation.draft_key,
                ) == target.draft
                && pane
                    .composer_paths
                    .as_ref()
                    .is_some_and(|search| Arc::ptr_eq(&search.target, target))
        })
}

#[derive(Debug)]
struct DebounceComposerPaths(Arc<PathTarget>);
impl Operation for DebounceComposerPaths {
    no_input!();
    const BACKGROUND: bool = true;
    type Output = ();
    async fn run(&self, _: (), _: &mut Execution<'_>) -> Result<(), PeerError> {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, pane: &str, _: ()) -> Vec<Effect> {
        if current(snapshot, pane, &self.0) {
            vec![Effect::continuation(ReadComposerPaths(self.0))]
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug)]
struct ReadComposerPaths(Arc<PathTarget>);
impl Operation for ReadComposerPaths {
    no_input!();
    const BACKGROUND: bool = true;
    type Output = Result<TreeEntries, String>;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        Ok(context
            .call(&path_request(&self.0.root, &self.0.query))
            .await
            .map_err(|reason| reason.to_string()))
    }
    fn rejected_output(&self, _: ()) -> Option<Self::Output> {
        Some(Err("Workspace search is unavailable.".into()))
    }
    fn apply(self, snapshot: &mut Snapshot, pane: &str, result: Self::Output) -> Vec<Effect> {
        if current(snapshot, pane, &self.0) {
            let pane = Arc::make_mut(snapshot.panes.get_mut(pane).expect("current pane exists"));
            pane.composer_paths = Some(Arc::new(PathSearch {
                target: self.0,
                result: Some(result),
            }));
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
            "/repo".into();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
            "draft".into();
        snapshot
    }

    fn query(snapshot: &mut Snapshot, text: &str) -> Arc<PathTarget> {
        assert_eq!(
            query_composer_paths(snapshot, "", &"draft".into(), text, text.len() as u32).len(),
            1
        );
        snapshot
            .panes
            .primary()
            .composer_paths
            .as_ref()
            .unwrap()
            .target
            .clone()
    }

    #[test]
    fn superseded_timers_and_replies_cannot_revive_an_earlier_identical_query() {
        let mut snapshot = state();
        let old = query(&mut snapshot, "@first");
        query(&mut snapshot, "@second");
        let current = query(&mut snapshot, "@first");
        assert_eq!(old, current);
        assert!(
            DebounceComposerPaths(old.clone())
                .apply(&mut snapshot, "", ())
                .is_empty()
        );
        assert_eq!(
            DebounceComposerPaths(current.clone())
                .apply(&mut snapshot, "", ())
                .len(),
            1
        );
        ReadComposerPaths(old).apply(&mut snapshot, "", Ok(TreeEntries::default()));
        assert!(
            snapshot
                .panes
                .primary()
                .composer_paths
                .as_ref()
                .unwrap()
                .result
                .is_none()
        );
        ReadComposerPaths(current).apply(&mut snapshot, "", Err("Host refused this read".into()));
        assert_eq!(
            snapshot
                .panes
                .primary()
                .composer_paths
                .as_ref()
                .unwrap()
                .result,
            Some(Err("Host refused this read".into()))
        );
        query(&mut snapshot, "@retry");
        assert!(
            snapshot
                .panes
                .primary()
                .composer_paths
                .as_ref()
                .unwrap()
                .result
                .is_none()
        );
    }

    #[test]
    fn unrelated_draft_edits_do_not_clear_the_current_panes_search() {
        let mut snapshot = state();
        let active = query(&mut snapshot, "@src");
        assert!(
            query_composer_paths(&mut snapshot, "", &"background".into(), "hello", 5).is_empty()
        );
        assert!(current(&snapshot, "", &active));
        assert!(query_composer_paths(&mut snapshot, "", &"draft".into(), "@src", 4).is_empty());
        assert!(
            query_composer_paths(&mut snapshot, "", &"draft".into(), "no trigger", 10).is_empty()
        );
        assert!(!current(&snapshot, "", &active));
    }

    #[test]
    fn deduplicated_and_unavailable_queries_do_not_publish_a_false_state_change() {
        let mut snapshot = state();
        query(&mut snapshot, "@src");
        let before = snapshot.clone();
        assert!(query_composer_paths(&mut snapshot, "", &"draft".into(), "@src", 4).is_empty());
        assert!(
            !snapshot.changes_from(&before).any,
            "a deduplicated read must not wake its view"
        );
        let mut snapshot = state();
        snapshot.connection.connected = false;
        let before = snapshot.clone();
        assert!(query_composer_paths(&mut snapshot, "", &"draft".into(), "@src", 4).is_empty());
        assert!(
            !snapshot.changes_from(&before).any,
            "an unavailable read must not wake its view"
        );
    }

    #[test]
    fn read_only_requests_reject_stale_text_without_editing_the_draft() {
        let mut snapshot = state();
        crate::state::set_draft_text(&mut snapshot, "draft".into(), "@current".into());
        let (unchanged, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(Box::new(Intent::QueryComposerPaths {
                thread_id: "draft".into(),
                text: "@stale".into(),
                cursor: 6,
            })),
        );
        assert_eq!(unchanged.input.drafts, snapshot.input.drafts);
        assert!(unchanged.panes.primary().composer_paths.is_none());
        let (next, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(Box::new(Intent::QueryComposerPaths {
                thread_id: "draft".into(),
                text: "@current".into(),
                cursor: 8,
            })),
        );
        assert_eq!(next.input.drafts, snapshot.input.drafts);
        assert_eq!(
            next.panes
                .primary()
                .composer_paths
                .as_ref()
                .unwrap()
                .target
                .query,
            "current"
        );
    }

    fn selection(path: &str) -> PathSelection {
        PathSelection {
            search: PathTarget {
                draft: "draft".into(),
                root: "/repo".into(),
                query: "src".into(),
            },
            path: path.into(),
        }
    }

    #[test]
    fn selected_file_and_folder_links_apply_to_the_original_draft_without_a_search_cache() {
        for (path, expected) in [
            ("src/main.rs", "[main.rs](src/main.rs) "),
            ("src", "[src](src) "),
        ] {
            let mut snapshot = state();
            crate::state::set_draft_text(&mut snapshot, "draft".into(), "@src".into());
            let (after, _) = crate::state::reduce(
                &snapshot,
                "",
                crate::state::Event::Intent(Box::new(Intent::InsertComposerPath {
                    expected_text: "@src".into(),
                    cursor: 4,
                    selection: selection(path),
                })),
            );
            assert_eq!(after.input.drafts[&DraftKey::from("draft")].text, expected);
            assert_eq!(snapshot.input.drafts[&DraftKey::from("draft")].text, "@src");
            assert!(after.panes.primary().composer_paths.is_none());
        }
    }

    #[test]
    fn a_delayed_path_selection_keeps_new_suffix_text_root_and_active_draft() {
        for changed in ["suffix", "root", "draft"] {
            let mut snapshot = state();
            crate::state::set_draft_text(&mut snapshot, "draft".into(), "@src".into());
            match changed {
                "suffix" => crate::state::set_draft_text(
                    &mut snapshot,
                    "draft".into(),
                    "@src newer text".into(),
                ),
                "root" => {
                    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
                        "/another".into()
                }
                _ => {
                    Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation)
                        .draft_key = "another".into()
                }
            }
            let (after, _) = crate::state::reduce(
                &snapshot,
                "",
                crate::state::Event::Intent(Box::new(Intent::InsertComposerPath {
                    expected_text: "@src".into(),
                    cursor: 4,
                    selection: selection("src/main.rs"),
                })),
            );
            assert_eq!(after.input.drafts, snapshot.input.drafts, "{changed}");
        }
    }

    #[test]
    fn selecting_an_existing_worktree_changes_the_request_and_projection_root() {
        let mut snapshot = state();
        query(&mut snapshot, "@src");
        let old = snapshot
            .panes
            .primary()
            .composer_paths
            .as_ref()
            .unwrap()
            .target
            .clone();
        crate::state::set_draft_text(&mut snapshot, "draft".into(), "@src".into());
        let (mut snapshot, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(Box::new(Intent::SelectWorkspace {
                thread_id: "draft".into(),
                workspace: Some(
                    agent_protocol::workspace::WorkspaceStrategy::ExistingWorktree {
                        path: "/worktree".into(),
                    },
                ),
            })),
        );
        let view =
            crate::presentation::view::Presentation::project(Arc::new(snapshot.clone()), "", None)
                .unwrap();
        let source = view.composer.completion_source();
        assert_eq!(source.root, "/worktree");
        let request = source.suggestions("@src", 4).unwrap().request.unwrap();
        let (next, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(Box::new(request)),
        );
        snapshot = next;
        let target = snapshot
            .panes
            .primary()
            .composer_paths
            .as_ref()
            .unwrap()
            .target
            .clone();
        assert_eq!(target.root, "/worktree");
        ReadComposerPaths(old).apply(&mut snapshot, "", Ok(TreeEntries::default()));
        assert!(
            snapshot
                .panes
                .primary()
                .composer_paths
                .as_ref()
                .unwrap()
                .result
                .is_none()
        );
        assert_eq!(path_request(&target.root, &target.query).root, "/worktree");
    }

    #[test]
    fn navigation_and_disconnect_release_the_composer_search_without_touching_explorer() {
        let mut snapshot = state();
        query(&mut snapshot, "@src");
        let explorer = snapshot.files.workspace.file_trees.clone();
        let (next, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(Box::new(Intent::NewChat {
                cwd: "/other".into(),
            })),
        );
        assert!(next.panes.primary().composer_paths.is_none());
        assert_eq!(next.files.workspace.file_trees, explorer);
        let (next, _) = crate::state::reduce(&snapshot, "", crate::state::Event::Disconnected);
        assert!(next.panes.primary().composer_paths.is_none());
    }

    proptest::proptest! {
        #[test]
        fn path_replies_need_the_original_request_owner_and_current_pane(
            same_request in proptest::bool::ANY, connected in proptest::bool::ANY,
            same_root in proptest::bool::ANY, same_draft in proptest::bool::ANY,
            epoch_current in proptest::bool::ANY,
        ) {
            let mut snapshot = state();
            let original = query(&mut snapshot, "@src");
            let request = if same_request { original } else { Arc::new((*original).clone()) };
            snapshot.connection.connected = connected;
            let navigation = Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation);
            if !same_root { navigation.cwd = "/elsewhere".into(); }
            if !same_draft { navigation.draft_key = "elsewhere".into(); }
            ReadComposerPaths(request).complete(&mut snapshot, "", Ok(TreeEntries::default()), epoch_current).unwrap();
            proptest::prop_assert_eq!(snapshot.panes.primary().composer_paths.as_ref().unwrap().result.is_some(),
                same_request && connected && same_root && same_draft && epoch_current);
        }
    }
}
