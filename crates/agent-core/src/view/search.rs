//! Thread search: the query bounds the Host accepts, title and pull request
//! matches ahead of message matches, and the highlighted message excerpt.
use super::thread_summary::ThreadSummary;
use super::time::compact_relative_time_label;
use crate::state::Snapshot;
use agent_protocol::conversation::{SearchMatch, SearchSource};
use std::collections::BTreeSet;

/// Message search bounds in UTF-16 units of the trimmed query; bounded so a
/// search cannot hold the Host's database.
pub const SEARCH_QUERY_MIN_LENGTH: usize = 2;
pub const SEARCH_QUERY_MAX_LENGTH: usize = 200;
pub const SEARCH_RESULT_LIMIT: u32 = 50;
/// How long typing settles before a message search starts.
pub const SEARCH_DEBOUNCE_MS: u64 = 200;

/// The trimmed query when the Host can search messages for it.
pub fn content_search_query(query: &str) -> Option<&str> {
    let query = query.trim();
    (SEARCH_QUERY_MIN_LENGTH..=SEARCH_QUERY_MAX_LENGTH)
        .contains(&query.encode_utf16().count())
        .then_some(query)
}

/// Search terms of the thread's linked pull request.
pub fn pull_request_search_terms(thread: &ThreadSummary) -> Vec<String> {
    thread
        .linked_pull_request
        .as_ref()
        .map(|pr| {
            vec![
                format!("#{}", pr.number),
                format!("{}#{}", pr.repository, pr.number),
                pr.url.clone(),
            ]
        })
        .unwrap_or_default()
}

/// Whether the title or a pull request term contains the lowercased query.
pub fn matches_thread_title(thread: &ThreadSummary, normalized_query: &str) -> bool {
    std::iter::once(thread.title.clone())
        .chain(pull_request_search_terms(thread))
        .any(|term| term.to_lowercase().contains(normalized_query))
}

/// Narrows an ordered list to title matches, then threads whose messages
/// matched (`content_ids`), each group in list order so lifecycle ordering
/// stays stable while the user narrows the list. Empty for a blank query.
pub fn search_threads<T: AsRef<ThreadSummary>, S: std::borrow::Borrow<str> + Ord>(
    threads: Vec<T>,
    query: &str,
    content_ids: &BTreeSet<S>,
) -> Vec<T> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut title_matches = vec![];
    let mut content_matches = vec![];
    for thread in threads {
        let summary = thread.as_ref();
        if matches_thread_title(summary, &query) {
            title_matches.push(thread);
        } else if content_ids.contains(summary.id.as_str()) {
            content_matches.push(thread);
        }
    }
    title_matches.extend(content_matches);
    title_matches
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HighlightPart {
    pub text: String,
    pub highlighted: bool,
}

/// Splits `text` around every occurrence of the trimmed query, folding ASCII
/// case only.
pub fn highlight_parts(text: &str, query: &str) -> Vec<HighlightPart> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return vec![HighlightPart {
            text: text.into(),
            highlighted: false,
        }];
    }
    let folded = text.to_ascii_lowercase();
    let mut parts = vec![];
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(offset) = folded[cursor..].find(&query) else {
            parts.push(HighlightPart {
                text: text[cursor..].into(),
                highlighted: false,
            });
            break;
        };
        let start = cursor + offset;
        if start > cursor {
            parts.push(HighlightPart {
                text: text[cursor..start].into(),
                highlighted: false,
            });
        }
        let end = start + query.len();
        parts.push(HighlightPart {
            text: text[start..end].into(),
            highlighted: true,
        });
        cursor = end;
    }
    parts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SearchExcerptSource {
    User,
    Agent,
}

/// "You: …" or "Agent: …" with the query highlighted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SearchExcerpt {
    pub source: SearchExcerptSource,
    pub source_label: String,
    pub parts: Vec<HighlightPart>,
}

pub fn search_excerpt(found: &SearchMatch, query: &str) -> SearchExcerpt {
    let source = match found.source {
        SearchSource::User => SearchExcerptSource::User,
        SearchSource::Assistant => SearchExcerptSource::Agent,
    };
    SearchExcerpt {
        source,
        source_label: match source {
            SearchExcerptSource::User => "You:",
            SearchExcerptSource::Agent => "Agent:",
        }
        .into(),
        parts: highlight_parts(&found.snippet, query),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SearchResultRow {
    pub thread_id: String,
    pub title: String,
    pub project_id: String,
    pub project_name: Option<String>,
    /// Age of the latest user message, else of the last update: "now", "5m".
    pub time_label: String,
    pub excerpt: Option<SearchExcerpt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SearchView {
    pub query: String,
    /// The query is not blank, so results replace the list.
    pub searching: bool,
    /// Message matches may still arrive.
    pub pending: bool,
    pub results: Vec<SearchResultRow>,
    /// Shown while searching with no results.
    pub empty_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SearchOptions {
    /// Thread ids in the list's order (pinned, active, working, snoozed,
    /// settled); results keep it and search only these.
    pub list_order: Vec<String>,
    /// The device is still settling the query or the Host has not answered
    /// it; the store does not record an unanswered search.
    pub awaiting_matches: bool,
}

/// Results for `Snapshot.search` among the listed threads of the active shell.
pub fn search_view(snapshot: &Snapshot, now_ms: i64, options: &SearchOptions) -> SearchView {
    let query = snapshot.search.clone();
    let searching = !query.trim().is_empty();
    let pending =
        options.awaiting_matches && snapshot.connected && content_search_query(&query).is_some();
    let shell = snapshot.shell_view();
    let rows: Vec<_> = shell
        .as_deref()
        .map(|shell| {
            options
                .list_order
                .iter()
                .filter_map(|id| {
                    shell
                        .threads
                        .iter()
                        .find(|row| row.id.as_str() == id && row.archived_at.is_none())
                })
                .map(ThreadSummary::from_shell)
                .collect()
        })
        .unwrap_or_default();
    let content_ids: BTreeSet<&str> = snapshot
        .search_matches
        .iter()
        .map(|found| found.thread_id.as_str())
        .collect();
    let results: Vec<_> = search_threads(rows, &query, &content_ids)
        .into_iter()
        .map(|thread| {
            let excerpt = snapshot
                .search_matches
                .iter()
                .find(|found| found.thread_id.as_str() == thread.id)
                .map(|found| search_excerpt(found, &query));
            SearchResultRow {
                time_label: compact_relative_time_label(
                    thread.latest_user_message_at.unwrap_or(thread.updated_at),
                    now_ms,
                ),
                project_name: shell.as_deref().and_then(|shell| {
                    shell
                        .projects
                        .iter()
                        .find(|project| project.id == thread.project)
                        .map(|project| project.name.clone())
                }),
                thread_id: thread.id,
                title: thread.title,
                project_id: thread.project,
                excerpt,
            }
        })
        .collect();
    let empty_label = (searching && results.is_empty()).then(|| {
        if pending {
            "Searching thread messages…"
        } else {
            "No threads found"
        }
        .into()
    });
    SearchView {
        query,
        searching,
        pending,
        results,
        empty_label,
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::state::Snapshot;
    use crate::sync::{ShellCache, ShellStatus};
    use agent_domain::{ThreadId, ThreadShell};
    use agent_protocol::conversation::ShellSnapshot;
    use agent_protocol::models::{Project, ProjectRoot};
    use std::sync::Arc;

    pub fn row(id: &str, project: &str, title: &str) -> ThreadShell {
        let state = crate::sync::fixtures::thread_state(title);
        let mut row = agent_domain::shell(&state).unwrap();
        row.id = ThreadId::new(id).unwrap();
        row.project = project.into();
        row
    }

    pub fn project(id: &str, name: &str, root: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            roots: vec![ProjectRoot { path: root.into() }],
            ..Project::default()
        }
    }

    pub fn shell_cache(projects: Vec<Project>, threads: Vec<ThreadShell>) -> ShellCache {
        let mut cache = ShellCache::from_cache(ShellSnapshot {
            snapshot_sequence: 1,
            projects,
            threads,
        });
        cache.status = ShellStatus::Live;
        cache
    }

    pub fn snapshot(projects: Vec<Project>, threads: Vec<ThreadShell>) -> Snapshot {
        Snapshot {
            connected: true,
            shell: Arc::new(shell_cache(projects, threads)),
            ..Snapshot::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::view::thread_summary::fixtures::{ms, summary};
    use agent_domain::{LinkedPullRequest, ThreadId, Timestamp};

    fn titled(id: &str, title: &str) -> ThreadSummary {
        ThreadSummary {
            title: title.into(),
            ..summary(id)
        }
    }

    fn threads() -> Vec<ThreadSummary> {
        // The project names ("Alpha", "Workspace", "Beta") are not searched.
        vec![
            titled("thread-1", "Fix workspace search"),
            titled("thread-2", "Review providers"),
            titled("thread-3", "WORKTREE cleanup"),
        ]
    }

    fn ids(threads: &[ThreadSummary]) -> Vec<&str> {
        threads.iter().map(|thread| thread.id.as_str()).collect()
    }

    fn content(ids: &[&'static str]) -> BTreeSet<&'static str> {
        ids.iter().copied().collect()
    }

    #[test]
    fn matches_thread_titles_case_insensitively_and_preserves_their_order() {
        let found = search_threads(threads(), "work", &BTreeSet::<&str>::new());
        assert_eq!(ids(&found), ["thread-1", "thread-3"]);
    }

    #[test]
    fn does_not_match_project_metadata() {
        let found = search_threads(threads(), "workspace", &BTreeSet::<&str>::new());
        assert_eq!(ids(&found), ["thread-1"]);
    }

    #[test]
    fn returns_no_results_for_an_empty_query() {
        assert!(search_threads(threads(), "   ", &BTreeSet::<&str>::new()).is_empty());
    }

    #[test]
    fn appends_content_only_matches_after_every_title_match() {
        let found = search_threads(threads(), "work", &content(&["thread-2"]));
        assert_eq!(ids(&found), ["thread-1", "thread-3", "thread-2"]);
    }

    #[test]
    fn lists_a_thread_matching_both_title_and_content_once() {
        let found = search_threads(threads(), "work", &content(&["thread-1"]));
        assert_eq!(ids(&found), ["thread-1", "thread-3"]);
    }

    #[test]
    fn ignores_content_matches_for_threads_outside_the_list() {
        let found = search_threads(threads(), "work", &content(&["thread-missing"]));
        assert_eq!(ids(&found), ["thread-1", "thread-3"]);
    }

    #[test]
    fn matches_linked_pull_request_terms() {
        let mut thread = titled("thread-1", "Unrelated");
        thread.linked_pull_request = Some(LinkedPullRequest {
            project: "project-1".into(),
            repository: "owner/repo".into(),
            number: 42,
            url: "https://github.com/owner/repo/pull/42".into(),
        });
        for query in ["#42", "OWNER/REPO#42", "github.com/owner"] {
            assert_eq!(
                ids(&search_threads(
                    vec![thread.clone()],
                    query,
                    &BTreeSet::<&str>::new()
                )),
                ["thread-1"],
                "{query}"
            );
        }
    }

    #[test]
    fn accepts_message_searches_at_the_maximum_query_length() {
        let query = "a".repeat(200);
        assert_eq!(content_search_query(&query), Some(query.as_str()));
        assert_eq!(
            content_search_query(&format!(" {query} ")),
            Some(query.as_str())
        );
    }

    #[test]
    fn ignores_message_searches_outside_the_query_bounds() {
        assert_eq!(content_search_query(&"a".repeat(201)), None);
        assert_eq!(content_search_query(" a "), None);
        assert_eq!(content_search_query("ab"), Some("ab"));
    }

    fn parts(text: &str, query: &str) -> Vec<(String, bool)> {
        highlight_parts(text, query)
            .into_iter()
            .map(|part| (part.text, part.highlighted))
            .collect()
    }

    #[test]
    fn highlights_every_occurrence_folding_ascii_case() {
        assert_eq!(
            parts("Fix the Needle and the needle", " needle "),
            [
                ("Fix the ".into(), false),
                ("Needle".into(), true),
                (" and the ".into(), false),
                ("needle".into(), true),
            ]
        );
        assert_eq!(
            parts("…café notes…", "CAFÉ"),
            [("…café notes…".into(), false)]
        );
        assert_eq!(
            parts("…café notes…", "café"),
            [
                ("…".into(), false),
                ("café".into(), true),
                (" notes…".into(), false),
            ]
        );
        assert_eq!(parts("snippet", "  "), [("snippet".into(), false)]);
    }

    fn found(thread: &str, source: SearchSource, snippet: &str) -> SearchMatch {
        SearchMatch {
            thread_id: ThreadId::new(thread).unwrap(),
            project_id: "project-1".into(),
            source,
            snippet: snippet.into(),
            message_created_at: None,
        }
    }

    #[test]
    fn names_who_wrote_the_matched_message() {
        let user = search_excerpt(&found("t", SearchSource::User, "the notes"), "notes");
        assert_eq!(user.source_label, "You:");
        let agent = search_excerpt(&found("t", SearchSource::Assistant, "the notes"), "notes");
        assert_eq!(agent.source, SearchExcerptSource::Agent);
        assert_eq!(agent.source_label, "Agent:");
        assert_eq!(agent.parts[1].text, "notes");
    }

    #[test]
    fn the_view_lists_title_then_message_matches_in_list_order_with_excerpts() {
        let now = ms("2026-04-10T12:00:00.000Z");
        let mut rows = vec![
            row("thread-1", "project-1", "Fix workspace search"),
            row("thread-2", "project-1", "Review providers"),
            row("thread-3", "project-1", "WORKTREE cleanup"),
            row("thread-4", "project-1", "Archived work"),
        ];
        rows[0].latest_user_message_at = Some(Timestamp::from_millis(now - 5 * 60_000).unwrap());
        rows[3].archived_at = Some(Timestamp::from_millis(now).unwrap());
        let mut snapshot = snapshot(vec![project("project-1", "Alpha", "/alpha")], rows);
        snapshot.search = "work".into();
        snapshot.search_matches = vec![
            found("thread-2", SearchSource::Assistant, "…work done…"),
            found("thread-3", SearchSource::User, "more work"),
        ];
        let options = SearchOptions {
            list_order: ["thread-3", "thread-2", "thread-1", "thread-4", "gone"]
                .map(String::from)
                .to_vec(),
            awaiting_matches: false,
        };
        let view = search_view(&snapshot, now, &options);
        assert!(view.searching);
        assert_eq!(view.empty_label, None);
        let result_ids: Vec<_> = view
            .results
            .iter()
            .map(|row| row.thread_id.as_str())
            .collect();
        assert_eq!(result_ids, ["thread-3", "thread-1", "thread-2"]);
        assert_eq!(view.results[1].time_label, "5m");
        assert_eq!(view.results[1].project_name.as_deref(), Some("Alpha"));
        assert_eq!(view.results[1].excerpt, None);
        assert_eq!(
            view.results[0].excerpt.as_ref().unwrap().source_label,
            "You:"
        );
        assert_eq!(
            view.results[2].excerpt.as_ref().unwrap().parts[1],
            HighlightPart {
                text: "work".into(),
                highlighted: true
            }
        );
    }

    #[test]
    fn an_empty_search_says_whether_message_matches_may_still_arrive() {
        let mut snapshot = snapshot(vec![], vec![row("thread-1", "project-1", "Thread")]);
        let mut options = SearchOptions {
            list_order: vec!["thread-1".into()],
            awaiting_matches: true,
        };
        let view = search_view(&snapshot, 0, &options);
        assert!(!view.searching);
        assert!(view.results.is_empty());
        assert_eq!(view.empty_label, None);
        snapshot.search = "x".into();
        let view = search_view(&snapshot, 0, &options);
        assert!(!view.pending);
        assert_eq!(view.empty_label.as_deref(), Some("No threads found"));
        snapshot.search = "needle".into();
        let view = search_view(&snapshot, 0, &options);
        assert!(view.pending);
        assert_eq!(
            view.empty_label.as_deref(),
            Some("Searching thread messages…")
        );
        options.awaiting_matches = false;
        let view = search_view(&snapshot, 0, &options);
        assert_eq!(view.empty_label.as_deref(), Some("No threads found"));
    }
}
