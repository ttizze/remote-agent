//! Importing projects and their Claude Code and Codex sessions found by a
//! Host scan: the picker, its default selection and the result message.
use super::paths::find_project_by_path;
use crate::models::Project;
use crate::view::collation::locale_compare;
use crate::view::time::compact_relative_time_label;
use agent_domain::Driver;
use agent_protocol::conversation::{ImportCounts, SessionCandidate, SessionScan};
use std::collections::{BTreeMap, BTreeSet};

const RECENT_PROJECT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// One or two threads in a folder is usually a one-off question, not a project.
const DEFAULT_SELECTION_MIN_THREADS: u64 = 3;
pub const SCAN_LIMIT_MESSAGE: &str =
    "Scan limit reached. Some projects or conversations may be missing.";

/// Git repositories active in the last 30 days with enough threads to look
/// like real work. Every candidate stays offered, since registered projects
/// still need their history imported.
pub fn is_default_import_candidate(candidate: &SessionCandidate, now_ms: i64) -> bool {
    candidate.git.is_some()
        && candidate.thread_count >= DEFAULT_SELECTION_MIN_THREADS
        && candidate.last_active_at.as_ref().is_some_and(|at| {
            let at = at.millis();
            at >= now_ms - RECENT_PROJECT_WINDOW_MS && at <= now_ms
        })
}

pub fn default_import_selection(scan: &SessionScan, now_ms: i64) -> BTreeSet<String> {
    scan.candidates
        .iter()
        .filter(|candidate| is_default_import_candidate(candidate, now_ms))
        .map(|candidate| candidate.path.clone())
        .collect()
}

/// The selection with `paths` checked or unchecked.
pub fn set_import_selection(
    selected: &BTreeSet<String>,
    paths: &[String],
    checked: bool,
) -> BTreeSet<String> {
    let mut next = selected.clone();
    for path in paths {
        if checked {
            next.insert(path.clone());
        } else {
            next.remove(path);
        }
    }
    next
}

/// Candidates grouped for the picker: clones of one origin share a group, a
/// repository without an origin is its own group, and folders outside Git
/// are kept apart. Groups are newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportGrouping<'a> {
    pub repositories: Vec<ImportGroup<'a>>,
    pub other: Vec<&'a SessionCandidate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportGroup<'a> {
    pub key: String,
    /// GitHub `owner/name`, or the folder name when the origin is elsewhere.
    pub label: String,
    pub repository: Option<String>,
    pub candidates: Vec<&'a SessionCandidate>,
    pub thread_count: u64,
    pub last_active_ms: Option<i64>,
}

pub fn group_import_candidates(candidates: &[SessionCandidate]) -> ImportGrouping<'_> {
    let mut repositories: Vec<ImportGroup<'_>> = Vec::new();
    let mut other = Vec::new();
    for candidate in candidates {
        let Some(git) = &candidate.git else {
            other.push(candidate);
            continue;
        };
        let key = match &git.remote_key {
            Some(remote) => format!("remote:{remote}"),
            None => format!("path:{}", candidate.path),
        };
        let last_active_ms = candidate.last_active_at.as_ref().map(|at| at.millis());
        match repositories.iter_mut().find(|group| group.key == key) {
            Some(group) => {
                group.candidates.push(candidate);
                group.thread_count += candidate.thread_count;
                group.last_active_ms = group.last_active_ms.max(last_active_ms);
            }
            None => repositories.push(ImportGroup {
                key,
                label: git
                    .repository
                    .clone()
                    .unwrap_or_else(|| candidate.title.clone()),
                repository: git.repository.clone(),
                candidates: vec![candidate],
                thread_count: candidate.thread_count,
                last_active_ms,
            }),
        }
    }
    repositories.sort_by(
        |left, right| match (left.last_active_ms, right.last_active_ms) {
            (Some(left_ms), Some(right_ms)) if left_ms != right_ms => right_ms.cmp(&left_ms),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => locale_compare(&left.label, &right.label),
        },
    );
    ImportGrouping {
        repositories,
        other,
    }
}

/// The project a candidate's history imports into: the Host's match from the
/// scan, then a registered project at the same root, else none (register one).
pub fn resolve_import_project_id(
    projects: &[Project],
    candidate: &SessionCandidate,
) -> Option<String> {
    candidate.project_id.clone().or_else(|| {
        find_project_by_path(projects, &candidate.path).map(|project| project.id.clone())
    })
}

/// The project to open after importing: the first selected one that gained
/// history, else the first that finished.
pub fn resolve_import_landing_project<'a>(
    selection: &[String],
    with_history: &'a BTreeMap<String, String>,
    completed: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    [with_history, completed].into_iter().find_map(|projects| {
        selection
            .iter()
            .find_map(|path| projects.get(path).map(String::as_str))
    })
}

fn threads(count: u64) -> String {
    format!("{count} {}", if count == 1 { "thread" } else { "threads" })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ImportToastKind {
    Success,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportToast {
    pub kind: ImportToastKind,
    pub title: String,
    pub description: Option<String>,
}

/// Imports across runs of the picker. A retry skips projects whose history
/// already imported completely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionImportProgress {
    /// Path to project for imports that skipped no session.
    completed: BTreeMap<String, String>,
    /// Path to project for imports that added at least one thread.
    with_history: BTreeMap<String, String>,
    selection: Vec<String>,
    completed_in_run: usize,
    imported_threads: u64,
    skipped_threads: u64,
    warning: Option<String>,
    rescan: bool,
}

impl SessionImportProgress {
    pub fn begin(&mut self, selection: Vec<String>) {
        self.completed_in_run = selection
            .iter()
            .filter(|path| self.completed.contains_key(*path))
            .count();
        self.selection = selection;
        self.imported_threads = 0;
        self.skipped_threads = 0;
        self.warning = None;
        self.rescan = false;
    }

    pub fn needs_import(&self, path: &str) -> bool {
        !self.completed.contains_key(path)
    }

    pub fn record_import(&mut self, path: &str, project_id: &str, counts: ImportCounts) {
        self.imported_threads += counts.imported;
        self.skipped_threads += counts.skipped;
        if counts.imported > 0 {
            self.with_history.insert(path.into(), project_id.into());
        }
        if counts.skipped == 0 {
            self.completed_in_run += 1;
            self.completed.insert(path.into(), project_id.into());
        }
    }

    /// Registering the project or importing its history failed; the scan is
    /// stale. An interrupted request is not a failure.
    pub fn record_failure(&mut self) {
        self.rescan = true;
    }

    pub fn finish(&mut self) {
        if self.completed_in_run >= self.selection.len() {
            return;
        }
        let (imported, skipped) = (self.imported_threads, self.skipped_threads);
        self.warning = Some(match (imported > 0, skipped > 0) {
            (true, true) => format!(
                "Imported {}. {} could not be imported.",
                threads(imported),
                threads(skipped)
            ),
            (false, true) => format!("{} could not be imported.", threads(skipped)),
            (true, false) => format!(
                "Imported {}. Some thread history could not be imported.",
                threads(imported)
            ),
            (false, false) => "Could not import thread history.".into(),
        });
    }

    /// The last run left the scan stale.
    pub fn rescan_needed(&self) -> bool {
        self.rescan
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn landing_project(&self) -> Option<&str> {
        resolve_import_landing_project(&self.selection, &self.with_history, &self.completed)
    }

    /// Shown once the app has opened the landing project.
    pub fn completion_toast(&self) -> Option<ImportToast> {
        if let Some(warning) = &self.warning {
            return Some(ImportToast {
                kind: ImportToastKind::Warning,
                title: "Some history was not imported".into(),
                description: Some(warning.clone()),
            });
        }
        (self.imported_threads > 0).then(|| ImportToast {
            kind: ImportToastKind::Success,
            title: format!("Imported {}", threads(self.imported_threads)),
            description: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ImportCheck {
    Checked,
    Unchecked,
    Mixed,
}

fn check_of(selected: usize, total: usize) -> ImportCheck {
    if selected == total {
        ImportCheck::Checked
    } else if selected > 0 {
        ImportCheck::Mixed
    } else {
        ImportCheck::Unchecked
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportCandidateRow {
    pub path: String,
    pub label: String,
    pub secondary: Option<String>,
    pub checked: bool,
    /// Empty on rows nested under a group header.
    pub sources: Vec<Driver>,
    pub thread_count: u64,
    /// "now", "5m", "3h", "2d", or empty without activity.
    pub age_label: String,
    pub nested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportRepositoryGroup {
    pub key: String,
    pub label: String,
    pub repository: Option<String>,
    pub check: ImportCheck,
    pub sources: Vec<Driver>,
    pub thread_count: u64,
    pub age_label: String,
    /// A group of one shows its row without a header.
    pub single: bool,
    pub rows: Vec<ImportCandidateRow>,
}

/// Folders outside Git, collapsed by default.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportFolderGroup {
    /// "3 folders".
    pub label: String,
    pub check: ImportCheck,
    pub rows: Vec<ImportCandidateRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SessionScanStatus {
    /// No result yet; the whole step waits.
    Loading {
        message: String,
    },
    /// The scan failed; a previous result may still be listed.
    Failed {
        message: String,
    },
    Empty {
        message: String,
    },
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImportView {
    pub title: String,
    pub description: String,
    pub status: SessionScanStatus,
    pub truncated_notice: Option<String>,
    /// "2 of 5 selected", when there is anything to select.
    pub selection_label: Option<String>,
    pub can_select_all: bool,
    pub can_select_none: bool,
    pub repositories: Vec<ImportRepositoryGroup>,
    pub folders: Option<ImportFolderGroup>,
    /// In import order.
    pub selected_paths: Vec<String>,
    pub import_label: String,
    pub can_import: bool,
    pub skip_label: String,
    pub can_skip: bool,
}

/// Activity age for the fixed-width column; under a minute reads "now".
fn import_age_label(last_active_ms: Option<i64>, now_ms: i64) -> String {
    last_active_ms.map_or_else(String::new, |at| compact_relative_time_label(at, now_ms))
}

/// The import step. `selection` is `None` until the user changes the default.
pub fn session_import_view(
    scan: Option<&SessionScan>,
    scan_pending: bool,
    scan_error: Option<&str>,
    selection: Option<&BTreeSet<String>>,
    importing: bool,
    now_ms: i64,
) -> SessionImportView {
    let candidates = scan.map_or(&[][..], |scan| scan.candidates.as_slice());
    let selected = match (selection, scan) {
        (Some(selection), _) => selection.clone(),
        (None, Some(scan)) => default_import_selection(scan, now_ms),
        (None, None) => BTreeSet::new(),
    };
    let selected_paths: Vec<String> = candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.path))
        .map(|candidate| candidate.path.clone())
        .collect();
    let row = |candidate: &SessionCandidate, label: &str, secondary: Option<String>, nested| {
        ImportCandidateRow {
            path: candidate.path.clone(),
            label: label.into(),
            secondary,
            checked: selected.contains(&candidate.path),
            sources: if nested {
                vec![]
            } else {
                candidate.sources.clone()
            },
            thread_count: candidate.thread_count,
            age_label: import_age_label(
                candidate.last_active_at.as_ref().map(|at| at.millis()),
                now_ms,
            ),
            nested,
        }
    };
    let grouping = group_import_candidates(candidates);
    let repositories = grouping
        .repositories
        .iter()
        .map(|group| {
            let single = group.candidates.len() == 1;
            let rows: Vec<_> = if single {
                let only = group.candidates[0];
                let secondary = group.repository.as_ref().map(|_| only.path.clone());
                vec![row(only, &group.label, secondary, false)]
            } else {
                group
                    .candidates
                    .iter()
                    .map(|candidate| row(candidate, &candidate.path, None, true))
                    .collect()
            };
            let mut sources = Vec::new();
            for source in group.candidates.iter().flat_map(|c| &c.sources) {
                if !sources.contains(source) {
                    sources.push(*source);
                }
            }
            ImportRepositoryGroup {
                key: group.key.clone(),
                label: group.label.clone(),
                repository: group.repository.clone(),
                check: check_of(rows.iter().filter(|row| row.checked).count(), rows.len()),
                sources,
                thread_count: group.thread_count,
                age_label: import_age_label(group.last_active_ms, now_ms),
                single,
                rows,
            }
        })
        .collect();
    let folders = (!grouping.other.is_empty()).then(|| {
        let rows: Vec<_> = grouping
            .other
            .iter()
            .map(|candidate| row(candidate, &candidate.path, None, true))
            .collect();
        ImportFolderGroup {
            label: format!(
                "{} {}",
                rows.len(),
                if rows.len() == 1 { "folder" } else { "folders" }
            ),
            check: check_of(rows.iter().filter(|row| row.checked).count(), rows.len()),
            rows,
        }
    });
    let status = if scan.is_none() && scan_pending {
        SessionScanStatus::Loading {
            message: "Looking for projects from Claude Code and Codex…".into(),
        }
    } else if let Some(error) = scan_error {
        SessionScanStatus::Failed {
            message: format!("Could not check projects. {error}"),
        }
    } else if candidates.is_empty() {
        SessionScanStatus::Empty {
            message: "No existing Claude Code or Codex projects found.".into(),
        }
    } else {
        SessionScanStatus::Ready
    };
    let loading = matches!(status, SessionScanStatus::Loading { .. });
    let count = selected_paths.len();
    SessionImportView {
        title: "Choose your projects".into(),
        description: "Import projects and conversations from your selected computers.".into(),
        status,
        truncated_notice: scan
            .filter(|scan| scan.truncated)
            .map(|_| SCAN_LIMIT_MESSAGE.into()),
        selection_label: (!candidates.is_empty())
            .then(|| format!("{count} of {} selected", candidates.len())),
        can_select_all: !importing && count < candidates.len(),
        can_select_none: !importing && count > 0,
        repositories,
        folders,
        import_label: if importing {
            "Importing…".into()
        } else {
            format!(
                "Import {count} {}",
                if count == 1 { "project" } else { "projects" }
            )
        },
        can_import: !loading && !importing && count > 0,
        skip_label: "Do not import projects".into(),
        can_skip: loading || !importing,
        selected_paths,
    }
}

#[cfg(test)]
mod tests {
    use super::super::paths::fixtures::project;
    use super::*;
    use agent_domain::Timestamp;
    use agent_protocol::conversation::ProjectGit;

    fn ms(iso: &str) -> i64 {
        Timestamp::parse(iso).unwrap().millis()
    }

    fn now() -> i64 {
        ms("2026-08-22T12:00:00.000Z")
    }

    fn candidate(path: &str) -> SessionCandidate {
        SessionCandidate {
            path: path.into(),
            title: path.rsplit('/').next().unwrap_or(path).into(),
            project_id: None,
            sources: vec![Driver::Codex],
            thread_count: 3,
            last_active_at: Some(Timestamp::parse("2026-08-20T12:00:00.000Z").unwrap()),
            already_imported: false,
            git: Some(ProjectGit {
                remote_key: None,
                repository: None,
            }),
        }
    }

    fn active(candidate: SessionCandidate, iso: &str) -> SessionCandidate {
        SessionCandidate {
            last_active_at: Some(Timestamp::parse(iso).unwrap()),
            ..candidate
        }
    }

    fn github(repository: &str) -> Option<ProjectGit> {
        Some(ProjectGit {
            remote_key: Some(format!("github.com/{}", repository.to_lowercase())),
            repository: Some(repository.into()),
        })
    }

    fn scan(candidates: Vec<SessionCandidate>) -> SessionScan {
        SessionScan {
            candidates,
            scanned_at: Timestamp::parse("2026-08-22T12:00:00.000Z").unwrap(),
            truncated: false,
        }
    }

    fn recent(candidates: Vec<SessionCandidate>) -> Vec<String> {
        let scan = scan(candidates);
        scan.candidates
            .iter()
            .filter(|c| is_default_import_candidate(c, now()))
            .map(|c| c.path.clone())
            .collect()
    }

    #[test]
    fn keeps_existing_projects_available_for_thread_history_import() {
        let imported = SessionCandidate {
            already_imported: true,
            ..candidate("/projects/current")
        };
        let available = candidate("/projects/other");
        let view = session_import_view(
            Some(&scan(vec![imported, available])),
            false,
            None,
            None,
            false,
            now(),
        );
        assert_eq!(view.selection_label.as_deref(), Some("2 of 2 selected"));
        assert_eq!(
            view.selected_paths,
            ["/projects/current", "/projects/other"]
        );
    }

    #[test]
    fn keeps_projects_older_than_30_days_out_of_the_default_selection() {
        assert_eq!(
            recent(vec![
                candidate("/projects/recent"),
                active(candidate("/projects/older"), "2026-07-01T12:00:00.000Z"),
            ]),
            ["/projects/recent"]
        );
    }

    #[test]
    fn keeps_future_activity_out_of_the_default_selection() {
        assert_eq!(
            recent(vec![
                candidate("/projects/recent"),
                active(candidate("/projects/future"), "2026-08-23T12:00:00.000Z"),
            ]),
            ["/projects/recent"]
        );
    }

    #[test]
    fn keeps_non_git_folders_and_thin_histories_out_of_the_default_selection() {
        assert_eq!(
            recent(vec![
                candidate("/projects/repo"),
                SessionCandidate {
                    git: None,
                    ..candidate("/projects/folder")
                },
                SessionCandidate {
                    thread_count: 2,
                    ..candidate("/projects/thin")
                },
            ]),
            ["/projects/repo"]
        );
    }

    #[test]
    fn groups_clones_by_origin_keeps_local_repos_separate_and_folds_non_git_folders_away() {
        let candidates = vec![
            SessionCandidate {
                git: github("acme/code"),
                thread_count: 79,
                ..active(candidate("/code/code"), "2026-08-21T12:00:00.000Z")
            },
            SessionCandidate {
                git: github("acme/code"),
                thread_count: 13,
                ..active(candidate("/code/clones/code-2"), "2026-08-10T12:00:00.000Z")
            },
            SessionCandidate {
                git: github("acme/fleet"),
                thread_count: 295,
                ..active(candidate("/code/fleet"), "2026-08-22T00:00:00.000Z")
            },
            candidate("/code/scratch-repo"),
            SessionCandidate {
                git: None,
                ..candidate("/tmp/notes")
            },
        ];
        let grouped = group_import_candidates(&candidates);
        assert_eq!(
            grouped
                .other
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["/tmp/notes"]
        );
        assert_eq!(
            grouped
                .repositories
                .iter()
                .map(|group| (
                    group.label.as_str(),
                    group
                        .candidates
                        .iter()
                        .map(|c| c.path.as_str())
                        .collect::<Vec<_>>(),
                    group.thread_count,
                    group.last_active_ms,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "acme/fleet",
                    vec!["/code/fleet"],
                    295,
                    Some(ms("2026-08-22T00:00:00.000Z"))
                ),
                (
                    "acme/code",
                    vec!["/code/code", "/code/clones/code-2"],
                    92,
                    Some(ms("2026-08-21T12:00:00.000Z"))
                ),
                (
                    "scratch-repo",
                    vec!["/code/scratch-repo"],
                    3,
                    Some(ms("2026-08-20T12:00:00.000Z"))
                ),
            ]
        );
    }

    #[test]
    fn uses_the_scanned_project_id_before_the_project_reaches_the_client() {
        let scanned = SessionCandidate {
            project_id: Some("local-project".into()),
            ..candidate("/projects/repo")
        };
        assert_eq!(
            resolve_import_project_id(&[], &scanned).as_deref(),
            Some("local-project")
        );
    }

    #[test]
    fn uses_the_scanned_project_id_when_the_client_still_has_an_older_project_at_that_root() {
        let scanned = SessionCandidate {
            project_id: Some("local-project".into()),
            ..candidate("/projects/repo")
        };
        assert_eq!(
            resolve_import_project_id(&[project("stale-project", "/projects/repo")], &scanned)
                .as_deref(),
            Some("local-project")
        );
    }

    #[test]
    fn returns_none_to_create_a_project_when_neither_the_scan_nor_the_client_has_a_project_id() {
        assert_eq!(
            resolve_import_project_id(&[], &candidate("/projects/new")),
            None
        );
    }

    #[test]
    fn finds_an_existing_project_by_normalized_root() {
        assert_eq!(
            resolve_import_project_id(
                &[project("local-project", "C:\\Work\\Repo\\")],
                &candidate("c:/work/repo")
            )
            .as_deref(),
            Some("local-project")
        );
    }

    #[test]
    fn finds_an_alias_after_the_scanner_returns_its_persisted_project_root() {
        assert_eq!(
            resolve_import_project_id(
                &[project("local-project", "/real/projects/repo")],
                &candidate("/real/projects/repo")
            )
            .as_deref(),
            Some("local-project")
        );
    }

    #[test]
    fn finds_the_current_root_owner_when_the_scan_has_no_project_id() {
        assert_eq!(
            resolve_import_project_id(
                &[
                    project("local-project", "/projects/other"),
                    project("recreated-project", "/projects/repo"),
                ],
                &candidate("/projects/repo")
            )
            .as_deref(),
            Some("recreated-project")
        );
    }

    #[test]
    fn does_not_reuse_a_moved_project_when_the_scan_has_no_project_id() {
        assert_eq!(
            resolve_import_project_id(
                &[project("local-project", "/projects/moved")],
                &candidate("/projects/repo")
            ),
            None
        );
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(path, project)| (path.to_string(), project.to_string()))
            .collect()
    }

    fn selection(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn skips_a_failed_first_project_for_a_later_project_with_imported_history() {
        let imported = map(&[("/projects/imported", "imported")]);
        assert_eq!(
            resolve_import_landing_project(
                &selection(&["/projects/failed", "/projects/imported"]),
                &imported,
                &imported
            ),
            Some("imported")
        );
    }

    #[test]
    fn prefers_a_partial_first_import_that_added_history() {
        assert_eq!(
            resolve_import_landing_project(
                &selection(&["/projects/partial", "/projects/complete"]),
                &map(&[("/projects/partial", "partial")]),
                &map(&[("/projects/complete", "complete")])
            ),
            Some("partial")
        );
    }

    #[test]
    fn uses_a_completed_zero_history_project_when_no_import_added_history() {
        assert_eq!(
            resolve_import_landing_project(
                &selection(&["/projects/empty", "/projects/failed"]),
                &BTreeMap::new(),
                &map(&[("/projects/empty", "empty")])
            ),
            Some("empty")
        );
    }

    #[test]
    fn keeps_an_earlier_successful_import_available_on_retry() {
        let imported = map(&[("/projects/imported", "imported")]);
        assert_eq!(
            resolve_import_landing_project(
                &selection(&["/projects/imported", "/projects/retry"]),
                &imported,
                &imported
            ),
            Some("imported")
        );
    }

    #[test]
    fn ignores_cached_successes_outside_the_current_retry_selection() {
        assert_eq!(
            resolve_import_landing_project(
                &selection(&["/projects/current"]),
                &map(&[("/projects/previous", "previous")]),
                &map(&[
                    ("/projects/previous", "previous"),
                    ("/projects/current", "current")
                ])
            ),
            Some("current")
        );
    }

    fn import_once(imported: u64, skipped: u64) -> SessionImportProgress {
        let mut progress = SessionImportProgress::default();
        progress.begin(selection(&["/project"]));
        progress.record_import(
            "/project",
            "test-project",
            ImportCounts { imported, skipped },
        );
        progress.finish();
        progress
    }

    #[test]
    fn enters_the_workspace_after_a_partial_import_and_warns() {
        let progress = import_once(28, 1);
        assert_eq!(progress.landing_project(), Some("test-project"));
        assert_eq!(
            progress.completion_toast(),
            Some(ImportToast {
                kind: ImportToastKind::Warning,
                title: "Some history was not imported".into(),
                description: Some("Imported 28 threads. 1 thread could not be imported.".into()),
            })
        );
    }

    #[rstest::rstest]
    #[case(0, 0, None)]
    #[case(29, 0, None)]
    #[case(1, 0, None)]
    #[case(0, 1, Some("1 thread could not be imported."))]
    #[case(0, 2, Some("2 threads could not be imported."))]
    fn finishes_setup_with_imported_and_skipped_threads(
        #[case] imported: u64,
        #[case] skipped: u64,
        #[case] warning: Option<&str>,
    ) {
        let progress = import_once(imported, skipped);
        assert_eq!(
            progress.landing_project(),
            (imported > 0 || skipped == 0).then_some("test-project")
        );
        let toast = progress.completion_toast();
        match warning {
            Some(warning) => assert_eq!(
                toast,
                Some(ImportToast {
                    kind: ImportToastKind::Warning,
                    title: "Some history was not imported".into(),
                    description: Some(warning.into()),
                })
            ),
            None if imported > 0 => assert_eq!(
                toast,
                Some(ImportToast {
                    kind: ImportToastKind::Success,
                    title: format!(
                        "Imported {imported} {}",
                        if imported == 1 { "thread" } else { "threads" }
                    ),
                    description: None,
                })
            ),
            None => assert_eq!(toast, None),
        }
    }

    #[test]
    fn preserves_the_import_warning_when_setup_is_finished_without_importing_again() {
        // Finishing setup failed after the import; skipping the import step
        // later reuses the same result instead of starting a new run.
        let progress = import_once(28, 1);
        assert_eq!(progress.landing_project(), Some("test-project"));
        assert_eq!(
            progress
                .completion_toast()
                .and_then(|toast| toast.description),
            Some("Imported 28 threads. 1 thread could not be imported.".into())
        );
    }

    #[test]
    fn a_retry_skips_projects_that_imported_completely_and_reports_failures() {
        let mut progress = SessionImportProgress::default();
        progress.begin(selection(&["/a", "/b"]));
        progress.record_import(
            "/a",
            "a",
            ImportCounts {
                imported: 2,
                skipped: 0,
            },
        );
        progress.record_failure();
        progress.finish();
        assert!(progress.rescan_needed());
        assert_eq!(
            progress.warning(),
            Some("Imported 2 threads. Some thread history could not be imported.")
        );
        progress.begin(selection(&["/a", "/b"]));
        assert!(!progress.needs_import("/a"));
        assert!(progress.needs_import("/b"));
        progress.record_import("/b", "b", ImportCounts::default());
        progress.finish();
        assert_eq!(progress.warning(), None);
        assert!(!progress.rescan_needed());
        assert_eq!(progress.landing_project(), Some("a"));
        assert_eq!(progress.completion_toast(), None);
    }

    #[test]
    fn nothing_imported_reads_as_a_failure() {
        let mut progress = SessionImportProgress::default();
        progress.begin(selection(&["/a"]));
        progress.record_failure();
        progress.finish();
        assert_eq!(progress.warning(), Some("Could not import thread history."));
        assert_eq!(progress.landing_project(), None);
    }

    #[test]
    fn the_step_lists_groups_folders_and_the_import_button() {
        let candidates = vec![
            SessionCandidate {
                git: github("acme/code"),
                sources: vec![Driver::Claude],
                ..active(candidate("/code/code"), "2026-08-22T11:59:30.000Z")
            },
            SessionCandidate {
                git: github("acme/code"),
                ..active(candidate("/code/code-2"), "2026-08-22T09:00:00.000Z")
            },
            SessionCandidate {
                git: github("acme/fleet"),
                ..active(candidate("/code/fleet"), "2026-08-20T12:00:00.000Z")
            },
            SessionCandidate {
                git: None,
                ..candidate("/tmp/notes")
            },
        ];
        let view = session_import_view(Some(&scan(candidates)), false, None, None, false, now());
        assert_eq!(view.status, SessionScanStatus::Ready);
        assert_eq!(view.selection_label.as_deref(), Some("3 of 4 selected"));
        assert_eq!(view.import_label, "Import 3 projects");
        assert!(view.can_import && view.can_select_all && view.can_select_none);
        let code = &view.repositories[0];
        assert_eq!(code.label, "acme/code");
        assert_eq!(code.check, ImportCheck::Checked);
        assert_eq!(code.sources, [Driver::Claude, Driver::Codex]);
        assert_eq!(code.age_label, "now");
        assert!(!code.single);
        assert_eq!(code.rows[0].label, "/code/code");
        assert!(code.rows[0].nested && code.rows[0].sources.is_empty());
        let fleet = &view.repositories[1];
        assert!(fleet.single);
        assert_eq!(fleet.rows[0].label, "acme/fleet");
        assert_eq!(fleet.rows[0].secondary.as_deref(), Some("/code/fleet"));
        assert_eq!(fleet.rows[0].age_label, "2d");
        let folders = view.folders.unwrap();
        assert_eq!(folders.label, "1 folder");
        assert_eq!(folders.check, ImportCheck::Unchecked);

        let mixed = set_import_selection(
            &view.selected_paths.iter().cloned().collect(),
            &["/code/code-2".into()],
            false,
        );
        let view = session_import_view(
            Some(&scan(vec![
                SessionCandidate {
                    git: github("acme/code"),
                    ..candidate("/code/code")
                },
                SessionCandidate {
                    git: github("acme/code"),
                    ..candidate("/code/code-2")
                },
            ])),
            false,
            None,
            Some(&mixed),
            true,
            now(),
        );
        assert_eq!(view.repositories[0].check, ImportCheck::Mixed);
        assert_eq!(view.import_label, "Importing…");
        assert!(!view.can_import && !view.can_skip && !view.can_select_all);
    }

    #[test]
    fn the_step_reports_loading_failures_and_empty_scans() {
        let loading = session_import_view(None, true, None, None, false, now());
        assert_eq!(
            loading.status,
            SessionScanStatus::Loading {
                message: "Looking for projects from Claude Code and Codex…".into()
            }
        );
        assert!(loading.can_skip && !loading.can_import);
        let failed = session_import_view(None, false, Some("Host offline"), None, false, now());
        assert_eq!(
            failed.status,
            SessionScanStatus::Failed {
                message: "Could not check projects. Host offline".into()
            }
        );
        let empty = session_import_view(
            Some(&SessionScan {
                truncated: true,
                ..scan(vec![])
            }),
            false,
            None,
            None,
            false,
            now(),
        );
        assert_eq!(
            empty.status,
            SessionScanStatus::Empty {
                message: "No existing Claude Code or Codex projects found.".into()
            }
        );
        assert_eq!(empty.truncated_notice.as_deref(), Some(SCAN_LIMIT_MESSAGE));
        assert_eq!(empty.selection_label, None);
        assert_eq!(empty.import_label, "Import 0 projects");
    }
}
