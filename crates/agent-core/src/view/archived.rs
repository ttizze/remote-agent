//! The archived threads list: rows grouped by project, its search and sort,
//! and its loading, error and empty states.
use super::thread_menu::{ThreadMenuConfirmation, delete_confirmation};
use super::time::{relative_time, relative_time_label};
use crate::state::{Snapshot, ThreadAction};
use crate::sync::ShellStatus;
use agent_domain::ThreadShell;
use agent_protocol::models::Project;
use std::cmp::Ordering;

/// Shown instead of the underlying failure, which may carry secrets.
pub const ARCHIVED_LOAD_ERROR: &str = "Failed to load archived threads.";

/// Desktop settings panel (projects in list order, no search) or the mobile
/// screen (search and archived-date sort).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArchivedLayout {
    #[default]
    Settings,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArchivedSortOrder {
    #[default]
    Newest,
    Oldest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedOptions {
    pub layout: ArchivedLayout,
    /// Screen only.
    pub search_query: String,
    /// Screen only.
    pub sort_order: ArchivedSortOrder,
    /// Settings only; the screen always asks.
    pub confirm_delete: bool,
}
impl Default for ArchivedOptions {
    fn default() -> Self {
        Self {
            layout: ArchivedLayout::Settings,
            search_query: String::new(),
            sort_order: ArchivedSortOrder::Newest,
            confirm_delete: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArchivedActionKind {
    Unarchive,
    Delete,
}

/// A row action: `Intent::Thread { thread_id, action }` after the
/// confirmation, if any.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedAction {
    pub kind: ArchivedActionKind,
    pub label: String,
    pub destructive: bool,
    pub action: ThreadAction,
    pub confirmation: Option<ThreadMenuConfirmation>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedRow {
    pub thread_id: String,
    pub title: String,
    pub branch: Option<String>,
    /// Mobile age of the archive: "<1m", "5m".
    pub age_label: String,
    /// Mobile line under the title: the Host and the branch.
    pub subtitle: Option<String>,
    /// Desktop line under the title: "Archived 5m ago · Created 2d ago".
    pub description: String,
    pub actions: Vec<ArchivedAction>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedGroup {
    pub project_id: String,
    pub title: String,
    pub root: Option<String>,
    pub environment_label: Option<String>,
    pub rows: Vec<ArchivedRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedEmpty {
    pub title: String,
    pub detail: Option<String>,
    pub loading: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArchivedView {
    pub groups: Vec<ArchivedGroup>,
    /// The archive is loading or reloading.
    pub loading: bool,
    /// Loading with rows already shown.
    pub refreshing: bool,
    pub error: Option<String>,
    /// Shown when no group is.
    pub empty: Option<ArchivedEmpty>,
}

fn archive_timestamp(thread: &ThreadShell) -> i64 {
    thread
        .archived_at
        .as_ref()
        .unwrap_or(&thread.updated_at)
        .millis()
}

fn contains(value: Option<&str>, query: &str) -> bool {
    value.is_some_and(|value| value.to_lowercase().contains(query))
}

fn root(project: &Project) -> Option<&str> {
    project.roots.first().map(|root| root.path.as_str())
}

/// Mobile grouping: a project matching the query keeps all its threads,
/// otherwise threads match by title or branch. Rows and groups order by
/// archive time, then title, then id.
pub fn screen_groups<'a>(
    projects: &'a [Project],
    threads: &'a [ThreadShell],
    environment_label: Option<&str>,
    search_query: &str,
    sort_order: ArchivedSortOrder,
) -> Vec<(&'a Project, Vec<&'a ThreadShell>)> {
    let query = search_query.trim().to_lowercase();
    let direction = |ordering: Ordering| match sort_order {
        ArchivedSortOrder::Newest => ordering.reverse(),
        ArchivedSortOrder::Oldest => ordering,
    };
    let mut groups: Vec<_> = projects
        .iter()
        .filter_map(|project| {
            let project_threads = threads
                .iter()
                .filter(|thread| thread.archived_at.is_some() && thread.project == project.id);
            let group_matches = query.is_empty()
                || contains(Some(&project.name), &query)
                || contains(root(project), &query)
                || contains(environment_label, &query);
            let mut matching: Vec<_> = project_threads
                .filter(|thread| {
                    group_matches
                        || contains(Some(&thread.title), &query)
                        || contains(
                            thread.workspace.as_ref().and_then(|w| w.branch.as_deref()),
                            &query,
                        )
                })
                .collect();
            if matching.is_empty() {
                return None;
            }
            matching.sort_by(|left, right| {
                direction(archive_timestamp(left).cmp(&archive_timestamp(right)))
                    .then_with(|| left.title.cmp(&right.title))
                    .then_with(|| left.id.cmp(&right.id))
            });
            Some((project, matching))
        })
        .collect();
    groups.sort_by(|(left, left_rows), (right, right_rows)| {
        direction(archive_timestamp(left_rows[0]).cmp(&archive_timestamp(right_rows[0])))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    groups
}

/// Desktop grouping: projects in list order, each thread newest archive
/// (else creation) first, then by id descending.
pub fn settings_groups<'a>(
    projects: &'a [Project],
    threads: &'a [ThreadShell],
) -> Vec<(&'a Project, Vec<&'a ThreadShell>)> {
    let key = |thread: &ThreadShell| {
        thread
            .archived_at
            .as_ref()
            .unwrap_or(&thread.created_at)
            .millis()
    };
    projects
        .iter()
        .filter_map(|project| {
            let mut rows: Vec<_> = threads
                .iter()
                .filter(|thread| thread.project == project.id)
                .collect();
            rows.sort_by(|left, right| {
                key(right)
                    .cmp(&key(left))
                    .then_with(|| right.id.cmp(&left.id))
            });
            (!rows.is_empty()).then_some((project, rows))
        })
        .collect()
}

fn row(
    thread: &ThreadShell,
    environment_label: Option<&str>,
    now_ms: i64,
    options: &ArchivedOptions,
) -> ArchivedRow {
    let branch = thread
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.branch.clone());
    let subtitle: Vec<&str> = [environment_label, branch.as_deref()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
    let archived_or_created = thread
        .archived_at
        .as_ref()
        .unwrap_or(&thread.created_at)
        .millis();
    let delete_confirmation = match options.layout {
        ArchivedLayout::Settings => options.confirm_delete.then(|| ThreadMenuConfirmation {
            title: None,
            message: delete_confirmation(&thread.title),
            destructive: true,
        }),
        ArchivedLayout::Screen => Some(ThreadMenuConfirmation {
            title: Some("Delete thread?".into()),
            message: format!(
                "\u{201c}{}\u{201d} will be permanently deleted, including its terminal history.",
                thread.title
            ),
            destructive: true,
        }),
    };
    ArchivedRow {
        thread_id: thread.id.to_string(),
        title: thread.title.clone(),
        age_label: relative_time(archive_timestamp(thread), now_ms),
        subtitle: (!subtitle.is_empty()).then(|| subtitle.join(" · ")),
        description: format!(
            "Archived {} · Created {}",
            relative_time_label(archived_or_created, now_ms),
            relative_time_label(thread.created_at.millis(), now_ms)
        ),
        branch,
        actions: vec![
            ArchivedAction {
                kind: ArchivedActionKind::Unarchive,
                label: "Unarchive".into(),
                destructive: false,
                action: ThreadAction::Unarchive,
                confirmation: None,
            },
            ArchivedAction {
                kind: ArchivedActionKind::Delete,
                label: "Delete".into(),
                destructive: true,
                action: ThreadAction::Delete,
                confirmation: delete_confirmation,
            },
        ],
    }
}

fn empty_state(options: &ArchivedOptions, loading: bool, error: Option<&str>) -> ArchivedEmpty {
    let empty = |title: &str, detail: Option<&str>, loading| ArchivedEmpty {
        title: title.into(),
        detail: detail.map(String::from),
        loading,
    };
    match options.layout {
        ArchivedLayout::Settings if loading => empty(
            "Loading archived threads",
            Some("Checking connected environments."),
            true,
        ),
        ArchivedLayout::Settings => match error {
            Some(error) => empty("Could not load archived threads", Some(error), false),
            None => empty(
                "No archived threads",
                Some("Archived threads will appear here."),
                false,
            ),
        },
        ArchivedLayout::Screen if loading && error.is_none() => {
            empty("Loading archive...", None, true)
        }
        ArchivedLayout::Screen if !options.search_query.trim().is_empty() => empty(
            "No matching threads",
            Some("Try another search or environment."),
            false,
        ),
        ArchivedLayout::Screen => empty(
            "No archived threads",
            Some("Threads you archive will appear here."),
            false,
        ),
    }
}

/// The archived list from `Snapshot.archived`, which is open only while the
/// archive is shown; until its first data it reads as loading.
pub fn archived_view(snapshot: &Snapshot, now_ms: i64, options: &ArchivedOptions) -> ArchivedView {
    let cache = snapshot.archived.as_deref();
    let error = cache
        .and_then(|cache| cache.error.as_ref())
        .map(|_| ARCHIVED_LOAD_ERROR.to_string());
    let loading = match cache {
        None => true,
        Some(cache) => {
            cache.status == ShellStatus::Synchronizing
                || (cache.status == ShellStatus::Empty && cache.error.is_none())
        }
    };
    let environment_label = snapshot.host_name.as_deref();
    let groups: Vec<_> = cache
        .and_then(|cache| cache.snapshot.as_ref())
        .map(|shell| {
            let grouped = match options.layout {
                ArchivedLayout::Settings => settings_groups(&shell.projects, &shell.threads),
                ArchivedLayout::Screen => screen_groups(
                    &shell.projects,
                    &shell.threads,
                    environment_label,
                    &options.search_query,
                    options.sort_order,
                ),
            };
            grouped
                .into_iter()
                .map(|(project, threads)| ArchivedGroup {
                    project_id: project.id.clone(),
                    title: project.name.clone(),
                    root: root(project).map(String::from),
                    environment_label: environment_label.map(String::from),
                    rows: threads
                        .into_iter()
                        .map(|thread| row(thread, environment_label, now_ms, options))
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();
    let empty = groups
        .is_empty()
        .then(|| empty_state(options, loading, error.as_deref()));
    ArchivedView {
        refreshing: loading && !groups.is_empty(),
        groups,
        loading,
        error,
        empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::search::fixtures::{project, row as shell_row, shell_cache};
    use crate::view::thread_summary::fixtures::ms;
    use agent_domain::{Timestamp, Workspace};
    use std::sync::Arc;

    fn at(iso: &str) -> Option<Timestamp> {
        Some(Timestamp::parse(iso).unwrap())
    }

    fn archived(id: &str, project: &str, title: &str) -> ThreadShell {
        let mut thread = shell_row(id, project, title);
        thread.archived_at = at("2026-06-02T00:00:00.000Z");
        thread
    }

    fn ids(groups: &[(&Project, Vec<&ThreadShell>)], index: usize) -> Vec<String> {
        groups[index]
            .1
            .iter()
            .map(|thread| thread.id.to_string())
            .collect()
    }

    #[test]
    fn groups_archived_threads_by_project_and_sorts_newest_first() {
        let projects = vec![project("project-1", "Code", "/workspaces/project-1")];
        let older = archived("thread-older", "project-1", "Older");
        let mut newer = archived("thread-newer", "project-1", "Newer");
        newer.archived_at = at("2026-06-03T00:00:00.000Z");
        let threads = vec![older, newer];
        let groups = screen_groups(
            &projects,
            &threads,
            Some("Julius's MacBook Pro"),
            "",
            ArchivedSortOrder::Newest,
        );
        assert_eq!(ids(&groups, 0), ["thread-newer", "thread-older"]);
    }

    #[test]
    fn matches_project_thread_and_branch_text() {
        let projects = vec![
            project("project-1", "Code", "/workspaces/project-1"),
            project("project-2", "Website", "/workspaces/project-2"),
        ];
        let mut first = archived("thread-1", "project-1", "Build settings route");
        first.workspace = Some(Workspace {
            cwd: "/workspaces/project-1".into(),
            worktree_path: None,
            branch: Some("fix/archive-screen".into()),
        });
        let threads = vec![first, archived("thread-2", "project-2", "Unrelated")];
        let groups = screen_groups(
            &projects,
            &threads,
            Some("Local"),
            "archive-screen",
            ArchivedSortOrder::Oldest,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0.id, "project-1");
        assert_eq!(ids(&groups, 0), ["thread-1"]);
        let by_project = screen_groups(
            &projects,
            &threads,
            Some("Local"),
            "WEBSITE",
            ArchivedSortOrder::Oldest,
        );
        assert_eq!(ids(&by_project, 0), ["thread-2"]);
        let by_root = screen_groups(
            &projects,
            &threads,
            Some("Local"),
            "project-2",
            ArchivedSortOrder::Oldest,
        );
        assert_eq!(ids(&by_root, 0), ["thread-2"]);
    }

    #[test]
    fn ignores_non_archived_entries_returned_in_a_snapshot() {
        let projects = vec![project("project-1", "Code", "/workspaces/project-1")];
        let threads = vec![shell_row("thread-active", "project-1", "Active")];
        assert!(screen_groups(&projects, &threads, None, "", ArchivedSortOrder::Newest).is_empty());
    }

    #[test]
    fn screen_groups_order_by_their_newest_row_then_title() {
        let projects = vec![
            project("project-a", "Alpha", "/a"),
            project("project-b", "Beta", "/b"),
            project("project-c", "Gamma", "/c"),
        ];
        let mut a = archived("a", "project-a", "A");
        a.archived_at = at("2026-06-01T00:00:00.000Z");
        let b = archived("b", "project-b", "B");
        let c = archived("c", "project-c", "C");
        let threads = vec![a, c, b];
        let order = |sort| {
            screen_groups(&projects, &threads, None, "", sort)
                .iter()
                .map(|(project, _)| project.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(order(ArchivedSortOrder::Newest), ["Beta", "Gamma", "Alpha"]);
        assert_eq!(order(ArchivedSortOrder::Oldest), ["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn settings_groups_keep_project_order_and_list_newest_archives_first() {
        let projects = vec![
            project("project-b", "Beta", "/b"),
            project("project-a", "Alpha", "/a"),
            project("project-empty", "Empty", "/e"),
        ];
        let mut early = archived("thread-1", "project-a", "Early");
        early.archived_at = at("2026-06-01T00:00:00.000Z");
        let threads = vec![
            early,
            archived("thread-2", "project-a", "Tie"),
            archived("thread-3", "project-a", "Tie"),
            archived("thread-4", "project-b", "Beta"),
        ];
        let groups = settings_groups(&projects, &threads);
        assert_eq!(
            groups
                .iter()
                .map(|(project, _)| project.id.as_str())
                .collect::<Vec<_>>(),
            ["project-b", "project-a"]
        );
        assert_eq!(ids(&groups, 1), ["thread-3", "thread-2", "thread-1"]);
    }

    fn snapshot_with(cache: Option<crate::sync::ShellCache>) -> Snapshot {
        Snapshot {
            host_name: Some("Studio".into()),
            archived: cache.map(Arc::new),
            ..Snapshot::default()
        }
    }

    #[test]
    fn rows_carry_labels_and_their_unarchive_and_delete_actions() {
        let now = ms("2026-06-02T00:05:00.000Z");
        let mut thread = archived("thread-1", "project-1", "Release prep");
        thread.created_at = Timestamp::parse("2026-05-31T00:00:00.000Z").unwrap();
        thread.workspace = Some(Workspace {
            cwd: "/repo".into(),
            worktree_path: None,
            branch: Some("feat/x".into()),
        });
        let cache = shell_cache(vec![project("project-1", "Alpha", "/repo")], vec![thread]);
        let snapshot = snapshot_with(Some(cache));
        let view = archived_view(&snapshot, now, &ArchivedOptions::default());
        assert!(!view.loading);
        assert_eq!(view.empty, None);
        let group = &view.groups[0];
        assert_eq!(group.title, "Alpha");
        assert_eq!(group.environment_label.as_deref(), Some("Studio"));
        let row = &group.rows[0];
        assert_eq!(row.description, "Archived 5m ago · Created 2d ago");
        assert_eq!(row.age_label, "5m");
        assert_eq!(row.subtitle.as_deref(), Some("Studio · feat/x"));
        assert_eq!(
            row.actions
                .iter()
                .map(|action| (
                    action.label.as_str(),
                    action.destructive,
                    action.action.clone()
                ))
                .collect::<Vec<_>>(),
            [
                ("Unarchive", false, ThreadAction::Unarchive),
                ("Delete", true, ThreadAction::Delete),
            ]
        );
        assert_eq!(
            row.actions[1].confirmation.as_ref().unwrap().message,
            "Delete thread \"Release prep\"?\nThis permanently clears conversation history for this thread."
        );
        let screen = archived_view(
            &snapshot,
            now,
            &ArchivedOptions {
                layout: ArchivedLayout::Screen,
                ..ArchivedOptions::default()
            },
        );
        let confirmation = screen.groups[0].rows[0].actions[1]
            .confirmation
            .clone()
            .unwrap();
        assert_eq!(confirmation.title.as_deref(), Some("Delete thread?"));
        assert_eq!(
            confirmation.message,
            "\u{201c}Release prep\u{201d} will be permanently deleted, including its terminal history."
        );
    }

    #[test]
    fn does_not_expose_an_archived_snapshot_failure_message() {
        let mut cache =
            crate::sync::ShellCache::new(agent_protocol::conversation::ShellLocation::Archived);
        cache.stream_error();
        let view = archived_view(&snapshot_with(Some(cache)), 0, &ArchivedOptions::default());
        assert!(view.groups.is_empty());
        assert_eq!(view.error.as_deref(), Some(ARCHIVED_LOAD_ERROR));
        assert!(!view.loading);
        assert_eq!(
            view.empty,
            Some(ArchivedEmpty {
                title: "Could not load archived threads".into(),
                detail: Some(ARCHIVED_LOAD_ERROR.into()),
                loading: false,
            })
        );
    }

    #[test]
    fn empty_states_follow_loading_and_the_query() {
        let loading = archived_view(&snapshot_with(None), 0, &ArchivedOptions::default());
        assert!(loading.loading);
        assert_eq!(loading.empty.unwrap().title, "Loading archived threads");
        let screen = |query: &str, cache| {
            archived_view(
                &snapshot_with(cache),
                0,
                &ArchivedOptions {
                    layout: ArchivedLayout::Screen,
                    search_query: query.into(),
                    ..ArchivedOptions::default()
                },
            )
            .empty
            .unwrap()
        };
        assert_eq!(
            screen("", None),
            ArchivedEmpty {
                title: "Loading archive...".into(),
                detail: None,
                loading: true,
            }
        );
        let live = || Some(shell_cache(vec![], vec![]));
        assert_eq!(screen("", live()).title, "No archived threads");
        assert_eq!(screen("x", live()).title, "No matching threads");
        let settings = archived_view(&snapshot_with(live()), 0, &ArchivedOptions::default());
        assert_eq!(
            settings.empty.unwrap().detail.as_deref(),
            Some("Archived threads will appear here.")
        );
    }

    #[test]
    fn a_reload_over_shown_rows_is_refreshing() {
        let mut cache = shell_cache(
            vec![project("project-1", "Alpha", "/repo")],
            vec![archived("thread-1", "project-1", "T")],
        );
        cache.status = ShellStatus::Synchronizing;
        let view = archived_view(&snapshot_with(Some(cache)), 0, &ArchivedOptions::default());
        assert!(view.loading && view.refreshing);
        assert_eq!(view.empty, None);
    }
}
