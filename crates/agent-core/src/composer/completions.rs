//! The Source menu combines provider invocations with workspace paths. The
//! classic invocation-only clients keep the same catalog and filtering owner.
use super::{CompletionCatalog, ComposerInsertion};
use agent_protocol::{
    composer::Invocation,
    file_tree::{EntryKind, TreeEntries},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PathTarget {
    pub draft: crate::state::DraftKey,
    pub root: String,
    // Full trimmed query identifies the result. Only the RPC truncates it.
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PathSelection {
    pub search: PathTarget,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathSearch {
    pub target: Arc<PathTarget>,
    pub result: Option<Result<TreeEntries, String>>,
}

pub(crate) fn path_query(text: &str, cursor: u32) -> Option<(std::ops::Range<usize>, &str)> {
    let before = text.get(..cursor as usize)?;
    let start = super::source_token_start(before);
    Some((start..before.len(), before[start..].strip_prefix('@')?))
}

pub(crate) fn path_root<'a>(
    cwd: &'a str,
    existing_thread: bool,
    workspace: Option<&'a agent_protocol::workspace::WorkspaceStrategy>,
) -> &'a str {
    match workspace {
        Some(agent_protocol::workspace::WorkspaceStrategy::ExistingWorktree { path })
            if !existing_thread =>
        {
            path
        }
        _ => cwd,
    }
}

pub(crate) fn path_request(
    root: &str,
    query: &str,
) -> agent_protocol::file_tree::SearchTreeEntries {
    let mut units = 0;
    let query = query
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= 256
        })
        .collect();
    agent_protocol::file_tree::SearchTreeEntries {
        root: root.into(),
        query,
        limit: 80,
        files_only: false,
    }
}

pub(crate) fn path_search_available(connected: bool, root: &str, query: &str) -> bool {
    connected && path_request(root, query).validate().is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSource {
    pub catalog: CompletionCatalog,
    pub connected: bool,
    pub draft: crate::state::DraftKey,
    pub root: String,
    pub paths: Option<Arc<PathSearch>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CompletionTarget {
    Invocation(Invocation),
    Path(PathSelection),
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletionItem {
    pub key: String,
    pub label: String,
    pub description: String,
    pub icon: &'static str,
    pub target: CompletionTarget,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionMenu {
    pub query_key: String,
    pub status: Option<String>,
    pub candidates: Vec<CompletionItem>,
    pub request: Option<crate::state::Intent>,
}

fn invocation_item(candidate: agent_protocol::composer::ComposerCandidate) -> CompletionItem {
    let invocation = candidate.invocation;
    CompletionItem {
        // Host retains distinct paths for a shared name, and dedupes full
        // invocations. Position must never be part of their identity.
        key: serde_json::json!([
            invocation.provider,
            invocation.kind,
            invocation.name,
            invocation.path
        ])
        .to_string(),
        label: super::candidate_label(&invocation.name),
        description: candidate.description,
        icon: if invocation.kind == agent_protocol::composer::InvocationKind::Skill {
            "✦"
        } else {
            "◈"
        },
        target: CompletionTarget::Invocation(invocation),
    }
}

impl CompletionSource {
    pub fn suggestions(&self, text: &str, cursor: u32) -> Option<CompletionMenu> {
        let legacy = self
            .catalog
            .suggestions(text, cursor)
            .map(|menu| CompletionMenu {
                query_key: menu.query_key,
                status: menu.status,
                candidates: menu.candidates.into_iter().map(invocation_item).collect(),
                request: None,
            });
        let Some((_, filter)) = path_query(text, cursor) else {
            return legacy;
        };
        let mut candidates = legacy.map_or_else(Vec::new, |menu| menu.candidates);
        let query = crate::text::trim(filter);
        let search = self.paths.as_deref().filter(|search| {
            search.target.draft == self.draft
                && search.target.root == self.root
                && search.target.query == query
        });
        if let Some(search) = search
            && let Some(Ok(entries)) = &search.result
        {
            candidates.extend(
                entries
                    .entries
                    .iter()
                    .filter(|entry| agent_protocol::file_tree::valid_tree_path(&entry.path))
                    .map(|entry| CompletionItem {
                        key: format!("path:{:?}:{}", entry.kind, entry.path),
                        // Workspace labels use Pierre's slash-only basename;
                        // prompt links separately support Windows separators.
                        label: entry.path.rsplit('/').next().unwrap_or(&entry.path).into(),
                        description: entry
                            .path
                            .rsplit_once('/')
                            .map_or("", |(parent, _)| parent)
                            .into(),
                        icon: if entry.kind == EntryKind::Directory {
                            "▸"
                        } else {
                            "▤"
                        },
                        target: CompletionTarget::Path(PathSelection {
                            search: (*search.target).clone(),
                            path: entry.path.clone(),
                        }),
                    }),
            );
        }
        let can_search = path_search_available(self.connected, &self.root, query);
        let request =
            (search.is_none() && can_search).then(|| crate::state::Intent::QueryComposerPaths {
                thread_id: self.draft.clone(),
                text: text.into(),
                cursor,
            });
        let status = if !candidates.is_empty() {
            None
        } else if can_search && search.is_none_or(|search| search.result.is_none()) {
            Some("Searching workspace files...".into())
        } else {
            Some("No matching files or folders.".into())
        };
        Some(CompletionMenu {
            query_key: format!("path:{}", query.to_lowercase()),
            status,
            candidates,
            request,
        })
    }

    pub fn insert(
        &self,
        text: &str,
        cursor: u32,
        target: &CompletionTarget,
    ) -> Option<ComposerInsertion> {
        match target {
            CompletionTarget::Invocation(invocation) => super::insert_invocation(
                text.into(),
                cursor,
                invocation.kind,
                invocation.name.clone(),
            ),
            CompletionTarget::Path(selection) => {
                insert_path(text, cursor, &self.draft, &self.root, selection)
            }
        }
    }
}

// Source's isPathSuggestionCurrent checks the search context, not membership
// in a mutable result list. Both files and folders insert the same link form.
// Core application additionally checks the actual draft before replacing it.
pub(crate) fn insert_path(
    text: &str,
    cursor: u32,
    draft: &crate::state::DraftKey,
    root: &str,
    selection: &PathSelection,
) -> Option<ComposerInsertion> {
    let (range, query) = path_query(text, cursor)?;
    let PathSelection { search, path } = selection;
    if draft != &search.draft
        || search.root != root
        || search.query != crate::text::trim(query)
        || !agent_protocol::file_tree::valid_tree_path(path)
    {
        return None;
    }
    let link = crate::presentation::files::file_mention(path);
    Some(super::insert_completion(text.into(), range, &link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::file_tree::TreeEntry;

    fn source() -> CompletionSource {
        CompletionSource {
            connected: true,
            catalog: CompletionCatalog {
                candidates: Vec::new(),
                loading: false,
                error: None,
                language: crate::localization::UiLanguage::English,
            },
            draft: "draft".into(),
            root: "/repo".into(),
            paths: Some(Arc::new(PathSearch {
                target: Arc::new(PathTarget {
                    draft: "draft".into(),
                    root: "/repo".into(),
                    query: "src".into(),
                }),
                result: Some(Ok(TreeEntries {
                    entries: vec![TreeEntry {
                        path: "src/a [b](#).rs".into(),
                        kind: EntryKind::File,
                        ignored: false,
                    }],
                    truncated: false,
                })),
            })),
        }
    }

    // Source detectComposerTrigger splits on ASCII SP/LF/CR/TAB and accepts
    // every character after @, including slash, unlike plugin invocations.
    #[rstest::rstest]
    #[case("@", 1, Some((0..1, "")))]
    #[case("前😀 @src/main", 17, Some((8..17, "src/main")))]
    #[case("x\n@src:foo", 10, Some((2..10, "src:foo")))]
    #[case("@\u{feff}src", 7, Some((0..7, "\u{feff}src")))]
    #[case("x\u{a0}@src", 7, None)]
    #[case("mail@src", 8, None)]
    #[case("＠src", 6, None)]
    #[case("😀@src", 2, None)]
    fn path_trigger_uses_source_ranges(
        #[case] text: &str,
        #[case] cursor: u32,
        #[case] expected: Option<(std::ops::Range<usize>, &str)>,
    ) {
        assert_eq!(path_query(text, cursor), expected);
    }

    #[rstest::rstest]
    #[case(254, true)]
    #[case(255, false)]
    #[case(256, false)]
    fn rpc_limit_is_utf16_and_never_splits_a_surrogate_pair(
        #[case] ascii: usize,
        #[case] keeps_emoji: bool,
    ) {
        let query = format!("{}😀tail", "x".repeat(ascii));
        let target = PathTarget {
            draft: "draft".into(),
            root: "/repo".into(),
            query: query.clone(),
        };
        let request = path_request(&target.root, &target.query);
        let expected = format!(
            "{}{}",
            "x".repeat(ascii),
            if keeps_emoji { "😀" } else { "" }
        );
        assert_eq!(request.query, expected);
        assert_eq!(request.limit, 80);
        assert!(!request.files_only);
        assert!(request.validate().is_ok());
        assert_eq!(
            target.query, query,
            "selection checks the full search target, not its RPC prefix"
        );
    }

    #[test]
    fn path_selection_preserves_unicode_prefix_suffix_and_exactly_one_separator() {
        let source = source();
        let target = source
            .suggestions("@src", 4)
            .unwrap()
            .candidates
            .remove(0)
            .target;
        for (suffix, expected_suffix) in [
            (" tail", "tail"),
            ("  tail", " tail"),
            ("\ttail", "\ttail"),
            ("\u{a0}tail", "\u{a0}tail"),
            ("tail", "tail"),
        ] {
            let input = format!("前😀 @src{suffix}");
            let insertion = source.insert(&input, 12, &target).unwrap();
            let prefix = "前😀 [a \\[b\\](#).rs](src/a%20%5Bb%5D%28%23%29.rs) ";
            assert_eq!(insertion.text, format!("{prefix}{expected_suffix}"));
            assert_eq!(insertion.cursor as usize, prefix.len());
        }
    }

    #[test]
    fn stale_path_items_cannot_replace_a_changed_query_root_or_draft() {
        let source = source();
        let target = source
            .suggestions("@src", 4)
            .unwrap()
            .candidates
            .remove(0)
            .target;
        assert!(source.insert("@other", 6, &target).is_none());
        assert!(source.insert("@Src", 4, &target).is_none());
        let mut changed = source.clone();
        changed.draft = "other".into();
        assert!(changed.insert("@src", 4, &target).is_none());
        let mut changed = source.clone();
        changed.root = "/other".into();
        assert!(changed.insert("@src", 4, &target).is_none());
    }

    #[test]
    fn a_selected_path_keeps_its_context_when_search_results_refresh() {
        let mut source = source();
        let target = source
            .suggestions("@src", 4)
            .unwrap()
            .candidates
            .remove(0)
            .target;
        // Source validates searchedTarget, not the newest entries array. A
        // refresh must not invalidate an already chosen file or folder link.
        for result in [None, Some(Ok(TreeEntries::default()))] {
            Arc::make_mut(source.paths.as_mut().unwrap()).result = result;
            assert_eq!(
                source.insert("@src", 4, &target).unwrap().text,
                "[a \\[b\\](#).rs](src/a%20%5Bb%5D%28%23%29.rs) "
            );
        }
        source.paths = None;
        assert!(source.insert("@src", 4, &target).is_some());
    }

    #[test]
    fn a_changed_query_requests_its_own_search_and_a_settled_empty_result_is_not_loading() {
        let mut source = source();
        let menu = source.suggestions("@other", 6).unwrap();
        assert!(
            matches!(menu.request, Some(crate::state::Intent::QueryComposerPaths { text, .. }) if text == "@other")
        );
        assert_eq!(menu.status.as_deref(), Some("Searching workspace files..."));
        Arc::make_mut(source.paths.as_mut().unwrap()).result = Some(Ok(TreeEntries::default()));
        assert_eq!(
            source.suggestions("@src", 4).unwrap().status.as_deref(),
            Some("No matching files or folders.")
        );
    }

    #[test]
    fn only_a_missing_current_search_requests_read_only_intake() {
        let mut source = source();
        assert!(source.suggestions("@src", 4).unwrap().request.is_none());
        Arc::make_mut(source.paths.as_mut().unwrap()).result = None;
        assert!(
            source.suggestions("@src", 4).unwrap().request.is_none(),
            "pending searches must not restart on publication"
        );
        source.paths = None;
        assert!(matches!(source.suggestions("@src", 4).unwrap().request,
            Some(crate::state::Intent::QueryComposerPaths { thread_id, text, cursor: 4 })
            if thread_id == source.draft && text == "@src"));
        assert!(
            source.suggestions("/", 1).unwrap().request.is_none(),
            "legacy skill clients do not acquire file resources"
        );
        source.root = "relative".into();
        assert!(source.suggestions("@src", 4).unwrap().request.is_none());
    }

    #[test]
    fn path_results_cannot_be_reused_by_another_queued_or_conversation_draft() {
        let mut source = source();
        source.draft = "another-editor".into();
        let menu = source.suggestions("@src", 4).unwrap();
        assert!(menu.candidates.is_empty());
        assert!(
            matches!(menu.request, Some(crate::state::Intent::QueryComposerPaths { thread_id, .. })
            if thread_id == source.draft)
        );
    }

    #[test]
    fn an_existing_threads_directory_is_not_redirected_by_saved_creation_preferences() {
        use agent_protocol::workspace::WorkspaceStrategy;
        let existing = WorkspaceStrategy::ExistingWorktree {
            path: "/chosen".into(),
        };
        assert_eq!(path_root("/thread", true, Some(&existing)), "/thread");
        assert_eq!(path_root("/project", false, Some(&existing)), "/chosen");
        let future = WorkspaceStrategy::Worktree {
            base_ref: "origin/main".into(),
            start_from_origin: true,
        };
        assert_eq!(
            path_root("/project", false, Some(&future)),
            "/project",
            "a new worktree is allocated by Host only when the conversation is created"
        );
    }

    #[test]
    fn invalid_relative_entries_are_neither_offered_nor_inserted() {
        let mut source = source();
        for path in [
            "",
            "../outside",
            "/outside",
            "src/../outside",
            ".git/objects",
        ] {
            let state = Arc::make_mut(source.paths.as_mut().unwrap());
            state.result = Some(Ok(TreeEntries {
                entries: vec![TreeEntry {
                    path: path.into(),
                    kind: EntryKind::File,
                    ignored: false,
                }],
                truncated: false,
            }));
            let target = CompletionTarget::Path(PathSelection {
                search: (*state.target).clone(),
                path: path.into(),
            });
            assert!(source.suggestions("@src", 4).unwrap().candidates.is_empty());
            assert!(source.insert("@src", 4, &target).is_none());
        }
    }

    #[test]
    fn workspace_labels_and_prompt_links_follow_their_distinct_source_basename_contracts() {
        let mut source = source();
        Arc::make_mut(source.paths.as_mut().unwrap()).result = Some(Ok(TreeEntries {
            entries: vec![TreeEntry {
                path: "日本語\\a.rs".into(),
                kind: EntryKind::File,
                ignored: false,
            }],
            truncated: false,
        }));
        let item = source.suggestions("@src", 4).unwrap().candidates.remove(0);
        assert_eq!(item.label, "日本語\\a.rs");
        assert_eq!(item.description, "");
        assert_eq!(
            source.insert("@src", 4, &item.target).unwrap().text,
            "[a.rs](%E6%97%A5%E6%9C%AC%E8%AA%9E%5Ca.rs) "
        );
    }
}
