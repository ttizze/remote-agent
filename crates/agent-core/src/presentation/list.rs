//! The visible list combines wire summaries with locally observed activity.
use super::task_title;
use crate::localization::{UiLanguage, localized_ui_format};
use crate::{
    models::{Project, ProjectRoot, task_active},
    state::{
        Snapshot,
        projects::{ChatPlacements, project_chat_visible},
    },
};
use base64::Engine;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashSet},
};
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectIconImages {
    pub light_png: Vec<u8>,
    pub dark_png: Vec<u8>,
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectSummary {
    pub host_id: String,
    pub id: String,
    pub name: String,
    /// The native coordinator lives on Claude, separately from local worker sessions.
    pub coordinator_url: Option<String>,
    pub roots: Vec<ProjectRoot>,
    pub icon: Option<ProjectIconImages>,
    pub monogram: String,
    pub icon_color: u32,
    pub pinned: bool,
    pub section: Option<String>,
    pub chats_archived: bool,
    pub expanded: bool,
    pub loading: bool,
    pub error: Option<super::error::ErrorText>,
    pub threads: Vec<ThreadSummary>,
    pub has_more: bool,
}

/// Native lists share the existing sidebar's section boundaries and pinned group.
pub fn project_headings<'a>(
    projects: impl IntoIterator<Item = (bool, Option<&'a str>)>,
    default_title: &'a str,
    pinned_title: &'a str,
) -> Vec<Option<&'a str>> {
    let mut last_section = Some(default_title);
    let mut pinned_heading = false;
    projects
        .into_iter()
        .map(|(pinned, section)| {
            if pinned {
                last_section = None;
                if pinned_heading {
                    None
                } else {
                    pinned_heading = true;
                    Some(pinned_title)
                }
            } else {
                let title = section.unwrap_or(default_title);
                if last_section == Some(title) {
                    None
                } else {
                    last_section = Some(title);
                    Some(title)
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod heading_tests {
    #[test]
    fn project_headings_preserve_pinned_precedence_and_section_boundaries() {
        let projects = [
            (false, None),
            (true, Some("ignored")),
            (true, None),
            (false, None),
            (false, Some("仕事")),
            (false, Some("仕事")),
            (false, None),
        ];
        assert_eq!(
            super::project_headings(projects, "プロジェクト", "ピン留め"),
            [
                None,
                Some("ピン留め"),
                None,
                Some("プロジェクト"),
                Some("仕事"),
                None,
                Some("プロジェクト")
            ]
        );
        assert_eq!(
            super::project_headings(
                [(false, Some("Projects")), (false, None)],
                "Projects",
                "Pinned"
            ),
            [None, None]
        );
        assert!(super::project_headings([], "Projects", "Pinned").is_empty());
    }
}

fn project_summary(project: &Project, display_name: &str) -> ProjectSummary {
    let name = project.name.trim();
    let mut words = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let monogram = words
        .next()
        .map(|word| {
            let first = word.chars().next().unwrap();
            let second = word
                .chars()
                .skip(1)
                .find(|c| c.is_numeric())
                .or_else(|| words.next_back().and_then(|last| last.chars().next()))
                .unwrap_or_else(|| word.chars().next_back().unwrap());
            format!("{first}{second}")
                .to_uppercase()
                .chars()
                .take(2)
                .collect()
        })
        .unwrap_or_else(|| "PR".into());
    const COLORS: [u32; 8] = [
        0x60a5fa, 0xa78bfa, 0xf472b6, 0xfb923c, 0xfbbf24, 0x4ade80, 0x2dd4bf, 0x38bdf8,
    ];
    let index = name
        .to_lowercase()
        .chars()
        .fold(0, |index, c| (index * 31 + c as usize) % COLORS.len());
    ProjectSummary {
        host_id: String::new(),
        id: project.id.clone(),
        name: display_name.to_owned(),
        coordinator_url: coordinator_url(&project.id),
        roots: project.roots.clone(),
        icon: project.icon.as_ref().and_then(|icon| {
            let base64 = base64::engine::general_purpose::STANDARD;
            Some(ProjectIconImages {
                light_png: base64.decode(&icon.light_png).ok()?,
                dark_png: base64.decode(&icon.dark_png).ok()?,
            })
        }),
        monogram,
        icon_color: COLORS[index],
        pinned: false,
        section: None,
        chats_archived: false,
        expanded: false,
        loading: false,
        error: None,
        threads: Vec::new(),
        has_more: false,
    }
}

fn coordinator_url(project_id: &str) -> Option<String> {
    let id = project_id.strip_prefix("claude-project:chan_")?;
    (!id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
        .then(|| format!("https://claude.ai/code/project/chan_{id}"))
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadCopyItem {
    pub label: String,
    pub value: Option<String>,
}

fn thread_copy_items(
    language: UiLanguage,
    title: &str,
    id: &str,
    cwd: Option<&str>,
    branch: Option<&str>,
) -> Vec<ThreadCopyItem> {
    [
        ("パス", cwd),
        ("ブランチ", branch),
        // BEX has no public thread URL, so a thread copies its undecorated title.
        ("スレッド", Some(title)),
        ("スレッドID", Some(id)),
    ]
    .into_iter()
    .map(|(label, value)| ThreadCopyItem {
        label: language.text(label).into(),
        value: value.filter(|value| !value.is_empty()).map(str::to_owned),
    })
    .collect()
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadSummary {
    pub host_id: String,
    pub id: crate::session::SessionRef,
    pub title: String,
    pub copy_items: Vec<ThreadCopyItem>,
    pub project_id: Option<String>,
    pub active: bool,
    pub waiting: bool,
    /// Waiting on background work that will wake the agent.
    pub background: bool,
    pub unread: bool,
    pub attention: Option<super::thread_status::ThreadAttention>,
    pub has_changes: Option<bool>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadList {
    pub loading_projects: bool,
    pub loading_chats: bool,
    pub notice: Option<String>,
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<ProjectSummary>,
    pub hidden_projects: Vec<ProjectSummary>,
    pub has_more: bool,
    pub has_more_projects: bool,
}

/// Only the data consumed by list presentation. Conversation bodies and
/// operation generations are deliberately absent from its equality contract.
pub(crate) struct ListSource<'a> {
    language: UiLanguage,
    lists: &'a crate::state::Lists,
    accounts: &'a Option<std::sync::Arc<agent_protocol::operations::Accounts>>,
    conversations: &'a std::sync::Arc<
        BTreeMap<crate::session::SessionRef, std::sync::Arc<crate::models::Thread>>,
    >,
    operations: &'a crate::state::Operations,
    storage_scope: &'a str,
    connected: bool,
}
impl<'a> ListSource<'a> {
    pub(crate) fn new(
        language: UiLanguage,
        lists: &'a crate::state::Lists,
        accounts: &'a Option<std::sync::Arc<agent_protocol::operations::Accounts>>,
        conversations: &'a std::sync::Arc<
            BTreeMap<crate::session::SessionRef, std::sync::Arc<crate::models::Thread>>,
        >,
        operations: &'a crate::state::Operations,
        storage_scope: &'a str,
        connected: bool,
    ) -> Self {
        Self {
            language,
            lists,
            accounts,
            conversations,
            operations,
            storage_scope,
            connected,
        }
    }
    fn listed_threads(&self) -> impl Iterator<Item = &crate::models::Thread> {
        self.lists
            .threads
            .iter()
            .chain(self.lists.project_threads.values())
            .flat_map(|page| &page.data)
    }
    fn thread_metadata(&self, id: &crate::session::SessionRef) -> Option<&crate::models::Thread> {
        self.conversations.get(id).map(AsRef::as_ref).or_else(|| {
            self.listed_threads()
                .find(|thread| thread.id.as_ref() == Some(id))
        })
    }
    fn project_preference(
        &self,
        project: &str,
    ) -> Option<&crate::state::projects::ProjectPreferences> {
        self.lists
            .project_preferences
            .get(self.storage_scope)?
            .get(project)
    }
    fn operation_running(&self, key: crate::state::operations::OperationKey) -> bool {
        self.operations
            .get(&key)
            .is_some_and(|state| state.phase == crate::state::operations::OperationPhase::Running)
    }
    pub(crate) fn equivalent(&self, other: &Self) -> bool {
        use crate::state::Same;
        self.language == other.language
            && self.lists.threads.same(&other.lists.threads)
            && self
                .lists
                .project_threads
                .same(&other.lists.project_threads)
            && self
                .lists
                .expanded_projects
                .same(&other.lists.expanded_projects)
            && self
                .lists
                .search_expanded_projects
                .same(&other.lists.search_expanded_projects)
            && self.lists.list_query.same(&other.lists.list_query)
            && self.lists.activity.same(&other.lists.activity)
            && self
                .lists
                .project_preferences
                .same(&other.lists.project_preferences)
            && self
                .lists
                .archived_scopes
                .same(&other.lists.archived_scopes)
            && self.lists.recent_chats == other.lists.recent_chats
            && self.accounts.same(other.accounts)
            && self.storage_scope == other.storage_scope
            && self.connected == other.connected
            && (self.lists.task_activity.same(&other.lists.task_activity)
                || self.task_marks() == other.task_marks())
            && self.same_phases(other)
            && self.same_shortcuts(other)
    }
    fn same_phases(&self, other: &Self) -> bool {
        use crate::state::operations::OperationKey as K;
        if crate::state::Same::same(self.operations, other.operations) {
            return true;
        }
        [K::SessionList, K::ListSearch]
            .into_iter()
            .chain(
                self.lists
                    .threads
                    .iter()
                    .flat_map(|page| &page.projects)
                    .map(|project| K::ListDecorations {
                        scope: agent_protocol::operations::ListDecorationScope::Project {
                            project_id: project.id.clone(),
                        },
                    }),
            )
            .all(|key| {
                self.operations.get(&key).map(|state| &state.phase)
                    == other.operations.get(&key).map(|state| &state.phase)
            })
    }
    fn same_shortcuts(&self, other: &Self) -> bool {
        if std::sync::Arc::ptr_eq(self.conversations, other.conversations) {
            return true;
        }
        self.lists
            .project_preferences
            .get(self.storage_scope)
            .into_iter()
            .flat_map(|preferences| preferences.values())
            .flat_map(|preferences| &preferences.placed_chats)
            .all(|chat| {
                match (
                    self.thread_metadata(&chat.id),
                    other.thread_metadata(&chat.id),
                ) {
                    (Some(a), Some(b)) => list_metadata(a) == list_metadata(b),
                    (None, None) => true,
                    _ => false,
                }
            })
    }
}
fn list_metadata(thread: &crate::models::Thread) -> impl PartialEq + '_ {
    (
        &thread.name,
        &thread.preview,
        thread.status,
        thread.updated_at,
        &thread.parent_id,
        &thread.project_id,
        thread.has_changes,
        thread.list_stale,
    )
}

impl Snapshot {
    pub(crate) fn list_source(&self) -> ListSource<'_> {
        ListSource::new(
            self.catalog.ui_language,
            &self.lists,
            &self.catalog.account.accounts,
            &self.sessions.conversations,
            &self.operations,
            &self.connection.storage_scope,
            self.connection.connected,
        )
    }
}
impl Snapshot {
    /// The listed project whose first folder is `directory`, named and marked
    /// as the sidebar shows it.
    pub(crate) fn directory_project(&self, directory: &str) -> Option<ProjectSummary> {
        let project = self
            .lists
            .threads
            .as_ref()?
            .projects
            .iter()
            .find(|project| {
                project
                    .roots
                    .first()
                    .is_some_and(|root| !directory.is_empty() && root.path == directory)
            })?;
        let name = self
            .lists
            .project_preferences
            .get(&self.connection.storage_scope)
            .and_then(|preferences| preferences.get(&project.id))
            .and_then(|preferences| preferences.name.as_deref())
            .unwrap_or(&project.name);
        Some(project_summary(project, name))
    }

    /// The selected folder is separate from the provider's execution directory.
    pub fn selected_directory(&self, pane: &crate::state::PaneState) -> String {
        let thread = pane
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.thread_metadata(id));
        if thread.is_some_and(|thread| {
            thread.project_id == (crate::models::ProjectMembership::Unassigned {})
        }) {
            String::new()
        } else {
            pane.navigation.cwd.clone()
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn thread_list(&self) -> Option<ThreadList> {
        self.list_source().thread_list()
    }
    pub fn recent_thread_list(&self) -> Option<ThreadList> {
        self.list_source().recent_thread_list()
    }
}

impl ListSource<'_> {
    fn list_error_providers(&self, list: &crate::models::ThreadList) -> Vec<String> {
        list.provider_errors
            .iter()
            .flat_map(|errors| errors.keys())
            .filter(|provider| {
                // An unused login provider is not a partial-list failure.
                // Keep errors visible until account discovery has completed.
                !matches!(provider.as_str(), "codex" | "claude")
                    || self.accounts.as_ref().is_none_or(|accounts| {
                        accounts
                            .accounts
                            .iter()
                            .any(|account| account.provider.key() == provider.as_str())
                    })
            })
            .cloned()
            .collect()
    }

    fn task_marks(&self) -> TaskMarks<'_> {
        self.lists
            .task_activity
            .as_ref()
            .map_or(TaskMarks::default(), |state| TaskMarks {
                waiting: &state.waiting,
                background: &state.background,
                attention: &state.attention,
            })
    }
    fn list_projection(
        &self,
        list: &crate::models::ThreadList,
        include_standalone: bool,
    ) -> (ThreadList, ChatPlacements<'_>) {
        let marks = self.task_marks();
        let search = self.lists.list_query.search_term.to_lowercase();
        let project_ids: HashSet<_> = list
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect();
        let errors = self.list_error_providers(list);
        let notice = (!errors.is_empty()).then(|| localized_ui_format(self.language, "会話一覧は部分結果です（{0}）。取得できない提供元の保存済み表示は最新とは限りません。".into(), vec![errors.join("、")]));
        let preferences = self.lists.project_preferences.get(self.storage_scope);
        let mut projects = Vec::new();
        let mut hidden_projects = Vec::new();
        let searching = !self.lists.list_query.search_term.trim().is_empty();
        let expanded = if !searching {
            &self.lists.expanded_projects
        } else {
            &self.lists.search_expanded_projects
        };
        for project in &list.projects {
            let prefs = preferences.and_then(|prefs| prefs.get(&project.id));
            if prefs.is_some_and(|prefs| prefs.hidden) {
                hidden_projects.push(project_summary(
                    project,
                    prefs
                        .and_then(|prefs| prefs.name.as_deref())
                        .unwrap_or(&project.name),
                ));
            } else {
                projects.push((
                    project,
                    prefs,
                    self.lists.project_threads.get(&project.id),
                    expanded
                        .get(&project.id)
                        .map_or(searching, |limit| *limit > 0),
                ));
            }
        }
        projects.sort_by_key(|(_, prefs, ..)| {
            (
                prefs.is_none_or(|p| !p.pinned),
                prefs.and_then(|p| p.section.as_deref()).unwrap_or_default(),
                prefs.and_then(|p| p.order).unwrap_or(usize::MAX),
            )
        });
        let metadata = crate::state::index_missing_metadata(
            self.conversations,
            self.listed_threads(),
            projects
                .iter()
                .filter(|(_, _, _, expanded)| *expanded)
                .flat_map(|(_, prefs, ..)| prefs.iter().flat_map(|prefs| &prefs.placed_chats))
                .map(|chat| &chat.id),
        );
        let placements = ChatPlacements::new(preferences);
        let summaries = if include_standalone {
            thread_summaries(
                self.language,
                list.data.iter().filter(|thread| {
                    thread
                        .project_id
                        .as_deref()
                        .is_none_or(|project| !project_ids.contains(project))
                        && thread
                            .id
                            .as_ref()
                            .is_none_or(|id| placements.get(id).is_none())
                }),
                &self.lists.activity.active,
                &self.lists.activity.unread,
                marks,
                // The filter has already resolved these rows as standalone.
                &HashSet::new(),
                None,
            )
        } else {
            Vec::new()
        };
        (
            ThreadList {
                notice,
                threads: summaries,
                loading_projects: self
                    .operation_running(crate::state::operations::OperationKey::SessionList),
                loading_chats: self
                    .operation_running(crate::state::operations::OperationKey::ListSearch),
                hidden_projects,
                projects: projects
                    .into_iter()
                    .map(|(project, prefs, page, expanded)| {
                        let mut summary = project_summary(
                            project,
                            prefs
                                .and_then(|prefs| prefs.name.as_deref())
                                .unwrap_or(&project.name),
                        );
                        if let Some(prefs) = prefs {
                            summary.pinned = prefs.pinned;
                            summary.section = prefs.section.clone();
                            summary.chats_archived = prefs.archived_before.is_some();
                        }
                        summary.expanded = expanded;
                        if !expanded {
                            return summary;
                        }
                        let archived_before = prefs.and_then(|prefs| prefs.archived_before);
                        let archived_ids = prefs.map(|prefs| &prefs.archived_ids);
                        let has_archive_filter = archived_before.is_some()
                            || archived_ids.is_some_and(|ids| !ids.is_empty());
                        if !self.connected && page.is_none() {
                            summary.error = Some(super::error::ErrorText {
                                message: self
                                    .language
                                    .text("接続するとタスクを読み込めます")
                                    .into(),
                                details: None,
                            });
                        }
                        if let Some(page) = page {
                            let data = page.data.iter().filter(|thread| {
                                thread
                                    .id
                                    .as_ref()
                                    .is_none_or(|id| placements.get(id).is_none())
                            });
                            summary.threads = if !has_archive_filter {
                                thread_summaries(
                                    self.language,
                                    data,
                                    &self.lists.activity.active,
                                    &self.lists.activity.unread,
                                    marks,
                                    &project_ids,
                                    None,
                                )
                            } else {
                                thread_summaries(
                                    self.language,
                                    data.filter(|thread| {
                                        project_chat_visible(
                                            archived_before,
                                            archived_ids,
                                            thread.id.as_ref(),
                                            task_active(
                                                thread.id.as_ref().and_then(|id| {
                                                    self.lists.activity.active.get(id).copied()
                                                }),
                                                thread.status,
                                            ),
                                            thread.updated_at,
                                            false,
                                        )
                                    }),
                                    &self.lists.activity.active,
                                    &self.lists.activity.unread,
                                    marks,
                                    &project_ids,
                                    None,
                                )
                            };
                            summary.has_more = page.has_more;
                            let errors = self.list_error_providers(page);
                            if !errors.is_empty() {
                                summary.error = Some(super::error::ErrorText {
                                    message: localized_ui_format(
                                        self.language,
                                        "一部を取得できません（{0}）".into(),
                                        vec![errors.join("、")],
                                    ),
                                    details: None,
                                });
                            }
                        }
                        let placed_ids =
                            prefs
                                .filter(|prefs| prefs.archived_before.is_some())
                                .map(|prefs| {
                                    prefs
                                        .placed_chats
                                        .iter()
                                        .map(|chat| &chat.id)
                                        .collect::<HashSet<_>>()
                                });
                        let placed = prefs
                            .into_iter()
                            .flat_map(|p| &p.placed_chats)
                            .map(|chat| {
                                self.conversations
                                    .get(&chat.id)
                                    .map(AsRef::as_ref)
                                    .or_else(|| metadata.get(&chat.id).copied().flatten())
                                    .map(Cow::Borrowed)
                                    .unwrap_or_else(|| {
                                        Cow::Owned(crate::models::Thread {
                                            id: Some(chat.id.clone()),
                                            name: Some(chat.title.clone()),
                                            ..Default::default()
                                        })
                                    })
                            })
                            .filter(|thread| {
                                !has_archive_filter
                                    || project_chat_visible(
                                        archived_before,
                                        archived_ids,
                                        thread.id.as_ref(),
                                        task_active(
                                            thread.id.as_ref().and_then(|id| {
                                                self.lists.activity.active.get(id).copied()
                                            }),
                                            thread.status,
                                        ),
                                        thread.updated_at,
                                        archived_before.is_some()
                                            && thread.id.as_ref().is_some_and(|id| {
                                                placed_ids
                                                    .as_ref()
                                                    .is_some_and(|ids| ids.contains(id))
                                            }),
                                    )
                            })
                            .filter(|thread| {
                                search.is_empty()
                                    || task_title(
                                        self.language,
                                        thread.name.as_deref(),
                                        thread.preview.as_deref(),
                                    )
                                    .to_lowercase()
                                    .contains(&search)
                            })
                            .collect::<Vec<_>>();
                        summary.threads.splice(
                            0..0,
                            thread_summaries(
                                self.language,
                                placed.iter().map(AsRef::as_ref),
                                &self.lists.activity.active,
                                &self.lists.activity.unread,
                                marks,
                                &project_ids,
                                Some(&project.id),
                            ),
                        );
                        summary
                    })
                    .collect(),
                has_more: list.has_more,
                has_more_projects: list.has_more_projects,
            },
            placements,
        )
    }
}

impl ListSource<'_> {
    /// Recent top-level chats across standalone and project pages, regardless of expansion.
    pub fn recent_thread_list(&self) -> Option<ThreadList> {
        let source = self.lists.threads.as_deref()?;
        let (mut list, placements) = self.list_projection(source, false);
        let marks = self.task_marks();
        let project_ids: HashSet<_> = source.projects.iter().map(|p| p.id.as_str()).collect();
        let mut rows: BTreeMap<_, &crate::models::Thread> = BTreeMap::new();
        for thread in source
            .data
            .iter()
            .chain(self.lists.project_threads.values().flat_map(|p| &p.data))
        {
            let Some(id) = &thread.id else { continue };
            let placement = placements.get(id);
            let project = placement.or_else(|| thread.project_id.as_ref().map(String::as_str));
            if let Some(project) = project
                && self.project_preference(project).is_some_and(|prefs| {
                    prefs.hidden
                        || !project_chat_visible(
                            prefs.archived_before,
                            Some(&prefs.archived_ids),
                            Some(id),
                            task_active(self.lists.activity.active.get(id).copied(), thread.status),
                            thread.updated_at,
                            placement.is_some(),
                        )
                })
            {
                continue;
            }
            rows.insert(id, thread);
        }
        let mut rows: Vec<_> = rows
            .into_values()
            .filter(|thread| thread.parent_id.is_none())
            .collect();
        rows.sort_by(|a, b| {
            b.updated_at
                .unwrap_or(0.0)
                .total_cmp(&a.updated_at.unwrap_or(0.0))
                .then_with(|| a.id.cmp(&b.id))
        });
        let limit = self.lists.list_query.limit as usize;
        list.has_more |= rows.len() > limit;
        rows.truncate(limit);
        list.threads = thread_summaries(
            self.language,
            rows.into_iter(),
            &self.lists.activity.active,
            &self.lists.activity.unread,
            marks,
            &project_ids,
            None,
        );
        for thread in &mut list.threads {
            if let Some(project) = placements.get(&thread.id) {
                thread.project_id = project_ids
                    .get(project)
                    .map(|project| (*project).to_owned());
            }
        }
        Some(list)
    }

    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.lists.threads.as_deref()?;
        Some(self.list_projection(list, true).0)
    }
}

/// The Host's marks for conversations beside their activity.
#[derive(Clone, Copy, Default, PartialEq)]
struct TaskMarks<'a> {
    waiting: &'a [crate::session::SessionRef],
    background: &'a [(crate::session::SessionRef, bool)],
    attention: &'a [(
        crate::session::SessionRef,
        agent_protocol::live_activity::Attention,
    )],
}

fn thread_summaries<'a>(
    language: UiLanguage,
    data: impl Iterator<Item = &'a crate::models::Thread>,
    active: &BTreeMap<crate::session::SessionRef, bool>,
    unread: &BTreeSet<crate::session::SessionRef>,
    marks: TaskMarks<'_>,
    project_ids: &HashSet<&str>,
    project_override: Option<&str>,
) -> Vec<ThreadSummary> {
    let waiting: HashSet<_> = marks.waiting.iter().collect();
    data.filter(|thread| thread.parent_id.is_none())
        .filter_map(|thread| Some((thread, thread.id.as_ref()?)))
        .map(|(thread, id)| {
            // Work left running after an answer is not the agent working; as
            // T3 Code, only work that wakes the agent keeps it waiting.
            let background = marks
                .background
                .iter()
                .find_map(|(session, holds)| (session == id).then_some(*holds));
            let active =
                task_active(active.get(id).copied(), thread.status) && background.is_none();
            let unread = unread.contains(id);
            let waiting = waiting.contains(id);
            let attention = marks
                .attention
                .iter()
                .find_map(|(session, attention)| (session == id).then_some((*attention).into()));
            let title = task_title(language, thread.name.as_deref(), thread.preview.as_deref());
            ThreadSummary {
                host_id: String::new(),
                id: id.clone(),
                title: if thread.list_stale == Some(true) {
                    localized_ui_format(language, "list.saved_title".into(), vec![title.into()])
                } else {
                    title.into()
                },
                copy_items: thread_copy_items(
                    language,
                    title,
                    &id.id,
                    thread.cwd.as_deref(),
                    thread.git_branch.as_deref(),
                ),
                project_id: project_override
                    .or_else(|| thread.project_id.as_deref())
                    .filter(|id| project_ids.contains(*id))
                    .map(str::to_owned),
                active,
                waiting,
                attention,
                background: background == Some(true),
                unread,
                has_changes: thread.has_changes,
            }
        })
        .collect()
}

#[cfg(test)]
mod placed_tests;
#[cfg(test)]
mod root_tests;
#[cfg(test)]
mod waiting_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::operations::Operation;
    use agent_protocol::models;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn only_native_claude_projects_link_to_their_coordinator() {
        let summary = project_summary(
            &Project {
                id: "claude-project:chan_01Q7KpeXNrNrvTTahXT6YE2S".into(),
                ..Default::default()
            },
            "",
        );
        assert_eq!(
            summary.coordinator_url.as_deref(),
            Some("https://claude.ai/code/project/chan_01Q7KpeXNrNrvTTahXT6YE2S")
        );
        for id in [
            "local",
            "claude-project:C:\\work",
            "claude-project:chan_",
            "claude-project:chan_bad/../../new",
            "claude-project:chan_bad?thread=other",
            "claude-project:chan_日本語",
        ] {
            assert!(
                coordinator_url(id).is_none(),
                "unexpected coordinator for {id}"
            );
        }
    }

    #[rstest::rstest]
    #[case("remote-agent", "RA")]
    #[case("t3code", "T3")]
    #[case("Bex", "BX")]
    #[case("日本語プロジェクト", "日ト")]
    #[case("", "PR")]
    #[case(" ! ", "PR")]
    fn project_names_receive_shared_monograms(#[case] name: &str, #[case] monogram: &str) {
        let project = Project {
            name: name.into(),
            ..Default::default()
        };
        let summary = project_summary(&project, &project.name);
        assert_eq!(summary.monogram, monogram);
        assert!(summary.icon.is_none());
    }

    #[test]
    fn project_logo_appearances_decode_together_and_reject_broken_images() {
        for (light, dark, expected) in [
            ("AQID", "BAUG", true),
            ("!", "BAUG", false),
            ("AQID", "!", false),
        ] {
            let project = Project {
                icon: Some(crate::models::ProjectIcon {
                    light_png: light.into(),
                    dark_png: dark.into(),
                }),
                ..Default::default()
            };
            let summary = project_summary(&project, "Project");
            assert_eq!(summary.icon.is_some(), expected);
            if let Some(images) = summary.icon {
                assert_eq!(images.light_png, [1, 2, 3]);
                assert_eq!(images.dark_png, [4, 5, 6]);
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn project_identity_is_bounded_and_ignores_surrounding_whitespace(name in ".{0,80}") {
            let project = |name| Project { name, ..Default::default() };
            let plain_project = project(name);
            let padded_project = project(format!(" \t{}\n ", plain_project.name));
            let plain = project_summary(&plain_project, &plain_project.name);
            let padded = project_summary(&padded_project, &padded_project.name);
            proptest::prop_assert_eq!(plain.monogram.chars().count(), 2);
            proptest::prop_assert_eq!(plain.monogram, padded.monogram);
            proptest::prop_assert_eq!(plain.icon_color, padded.icon_color);
        }
    }

    fn snapshot_list(data: serde_json::Value) -> Snapshot {
        Snapshot {
            lists: crate::state::Lists {
                threads: Some(std::sync::Arc::new(
                    serde_json::from_value(json!({
                        "data":data,"projects":[{"id":"project","name":"Project","roots":[]}],
                        "hasMore":false,"hasMoreProjects":false,"limit":5,}))
                    .unwrap(),
                )),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn waiting_updates_all_list_scopes_without_opening_conversations() {
        use crate::state::{Event, Intent, list_sync, reduce};
        use agent_protocol::{
            conversation::records,
            session::{ProviderKind, SessionRef},
        };
        let id = |provider, name: &str| SessionRef::new(provider, name.into()).unwrap();
        let root = id(ProviderKind::Codex, "same");
        let other = id(ProviderKind::Claude, "same");
        let project = id(ProviderKind::Codex, "project");
        let placed = id(ProviderKind::Claude, "placed");
        let affected = [root.clone(), project.clone(), placed.clone()];
        let mut state = snapshot_list(json!([{"id":root},{"id":other},{"id":placed}]));
        let page = snapshot_list(json!([{"id":project,"projectId":"project"}]))
            .lists
            .threads
            .unwrap();
        Arc::make_mut(&mut state.lists.project_threads).insert("project".into(), page);
        state = reduce(
            &state,
            "",
            Event::Intent(Intent::MoveChatToProject {
                thread_id: placed,
                project_id: "project".into(),
            }),
        )
        .0;
        let facts = |waiting| {
            affected
                .iter()
                .cloned()
                .map(|id| {
                    (
                        id,
                        records::Activity {
                            status: agent_protocol::execution::SessionStatus::Running,
                            waiting,
                            ..Default::default()
                        },
                    )
                })
                .collect()
        };
        let first = list_sync::set_activity(&mut state, facts(true));
        for (waiting, stale, expected) in [
            (true, false, true),
            (false, false, false),
            (true, true, false),
        ] {
            if stale {
                state =
                    list_sync::received(&state, state.lists.subscription.unwrap(), first.clone()).0;
            } else {
                list_sync::set_activity(&mut state, facts(waiting));
            }
            let list = state.thread_list().unwrap();
            assert_eq!(list.threads.len(), 2);
            assert_eq!(list.projects[0].threads.len(), 2);
            let recent = state.recent_thread_list().unwrap();
            assert_eq!(recent.threads.len(), 4);
            for row in list
                .threads
                .iter()
                .chain(&list.projects[0].threads)
                .chain(&recent.threads)
            {
                assert_eq!(row.waiting, expected && row.id != other);
                if row.id != other {
                    assert!(row.active);
                }
            }
            assert!(state.sessions.conversations.is_empty());
            assert!(state.sessions.subscriptions.is_empty());
        }
    }

    #[test]
    fn work_left_running_after_an_answer_reads_waiting_only_when_it_wakes_the_agent() {
        use agent_protocol::{
            conversation::records,
            execution::{BackgroundTask, BackgroundTaskKind},
            session::{ProviderKind, SessionRef},
        };
        let id = |name: &str| SessionRef::new(ProviderKind::Claude, name.into()).unwrap();
        let (agents, server, working) = (id("agents"), id("server"), id("working"));
        let mut state = snapshot_list(json!([{"id":agents},{"id":server},{"id":working}]));
        let background = |kind| {
            vec![BackgroundTask {
                id: "task".into(),
                kind,
                description: None,
            }]
        };
        crate::state::list_sync::set_activity(
            &mut state,
            vec![
                (
                    agents.clone(),
                    records::Activity {
                        status: agent_protocol::execution::SessionStatus::Running,
                        background: background(BackgroundTaskKind::Subagent),
                        ..Default::default()
                    },
                ),
                (
                    server.clone(),
                    records::Activity {
                        status: agent_protocol::execution::SessionStatus::Running,
                        background: background(BackgroundTaskKind::Command),
                        ..Default::default()
                    },
                ),
                (
                    working.clone(),
                    records::Activity {
                        status: agent_protocol::execution::SessionStatus::Running,
                        ..Default::default()
                    },
                ),
            ],
        );
        let list = state.thread_list().unwrap();
        let row = |id: &SessionRef| {
            let row = list.threads.iter().find(|row| &row.id == id).unwrap();
            (row.active, row.background)
        };
        assert_eq!(row(&agents), (false, true));
        assert_eq!(
            row(&server),
            (false, false),
            "a dev server leaves the chat ready"
        );
        assert_eq!(row(&working), (true, false));
    }

    #[test]
    fn recent_chats_merge_closed_projects_in_update_order_and_page_globally() {
        let mut snapshot = snapshot_list(json!([
            {"id":{"provider":"codex","id":"standalone"},"updatedAt":10},
            {"id":{"provider":"codex","id":"duplicate"},"updatedAt":5}
        ]));
        let page = serde_json::from_value(json!({
            "data":[
                {"id":{"provider":"codex","id":"project-chat"},"projectId":"project","updatedAt":30},
                {"id":{"provider":"codex","id":"duplicate"},"projectId":"project","updatedAt":20},
                {"id":{"provider":"claude","id":"duplicate"},"projectId":"project","updatedAt":15},
                {"id":{"provider":"codex","id":"agent"},"parentId":{"provider":"codex","id":"project-chat"},"projectId":"project","updatedAt":40}
            ],"projects":[],"limit":5,"hasMore":false,"hasMoreProjects":false
        })).unwrap();
        Arc::make_mut(&mut snapshot.lists.project_threads).insert("project".into(), Arc::new(page));
        Arc::make_mut(&mut snapshot.lists.list_query).limit = 2;
        let list = snapshot.recent_thread_list().unwrap();
        assert!(!list.projects[0].expanded);
        assert_eq!(
            list.threads
                .iter()
                .map(|t| t.id.id.as_str())
                .collect::<Vec<_>>(),
            ["project-chat", "duplicate"]
        );
        assert!(
            list.threads
                .iter()
                .all(|t| t.project_id.as_deref() == Some("project"))
        );
        assert!(list.has_more);
        Arc::make_mut(&mut snapshot.lists.list_query).limit = 10;
        let list = snapshot.recent_thread_list().unwrap();
        assert_eq!(list.threads.len(), 4);
        assert!(!list.has_more);
        assert_eq!(list.threads.last().unwrap().id.id, "standalone");
        assert_eq!(snapshot.thread_list().unwrap().threads.len(), 2);
    }

    #[test]
    fn conversation_list_excludes_subagents_and_project_tasks_at_root() {
        let snapshot = snapshot_list(json!([
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"}},
            {"id":{"provider":"claude","id":"child"}},
            {"id":{"provider":"codex","id":"root"},"projectId":"project"},
            {"id":{"provider":"codex","id":"fork"}}
        ]));
        let list = snapshot.thread_list().unwrap();
        let rows: Vec<_> = list
            .threads
            .iter()
            .map(|thread| (thread.id.provider, thread.id.id.as_str()))
            .collect();
        use crate::session::ProviderKind::{Claude, Codex};
        assert_eq!(rows, [(Some(Claude), "child"), (Some(Codex), "fork")]);
    }

    #[test]
    fn list_keeps_only_standalone_chats_at_root_and_exposes_known_membership() {
        let mut snapshot = Snapshot::default();
        crate::state::list_sync::seed(
            &mut snapshot,
            serde_json::from_value(json!({
                "data": [
                    {"id":{"provider":"codex","id":"assigned"}, "updatedAt":4,"projectId":"known"},
                    {"id":{"provider":"codex","id":"missing"}, "updatedAt":3,"projectId":"absent"},
                    {"id":{"provider":"codex","id":"chat"}, "updatedAt":2,"projectId":null},
                    {"id":{"provider":"codex","id":"unknown"},"updatedAt":1}
                ],
                "projects":[{"id":"known", "name":"Project", "roots":[]}],
                 "hasMore":true,"hasMoreProjects":false,"limit":5, }))
            .unwrap(),
        );
        let list = snapshot.thread_list().unwrap();
        let memberships: Vec<_> = list
            .threads
            .iter()
            .map(|thread| (thread.id.id.as_str(), thread.project_id.as_deref()))
            .collect();
        assert_eq!(
            memberships,
            vec![("missing", None), ("chat", None), ("unknown", None)]
        );
        assert_eq!(list.projects[0].id, "known");
        assert!(
            !list.has_more,
            "the complete catalog contains only three standalone rows"
        );
        assert!(!list.has_more_projects);
    }

    #[test]
    fn returned_project_header_replaces_branding_while_preserving_loaded_tasks() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.lists.expanded_projects).insert("project".into(), 5);
        crate::state::list_sync::seed(&mut snapshot, serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"task"},"projectId":"project"}],
                "projects":[{"id":"project","name":"Old project","roots":[],"icon":{"lightPng":"BAUG","darkPng":"BAUG"}}],
                "hasMore":false,"hasMoreProjects":false,"limit":5
            }))
            .unwrap());
        for icon in [Some("AQID"), None] {
            use agent_protocol::{
                conversation::records,
                sync::{Content, Cursor, Key, Position},
            };
            let project: crate::models::Project = serde_json::from_value(
                json!({"id":"project","name":"Renamed project","roots":[],"icon":null}),
            )
            .unwrap();
            let from = snapshot.lists.replica.as_ref().unwrap().cursor().unwrap();
            let message = records::Message::Changes {
                from,
                through: Cursor {
                    sequence: from.sequence + 1,
                    ..from
                },
                rows: vec![records::Row {
                    key: Key::Project(project.id.clone()),
                    version: from.sequence + 1,
                    position: Position::Project { position: 100 },
                    content: Content::Full(Arc::new(records::Record::Project(project))),
                }],
                removed: Vec::new(),
            };
            snapshot = crate::state::list_sync::received(
                &snapshot,
                snapshot.lists.subscription.unwrap(),
                message,
            )
            .0;
            crate::state::operations::ReadListDecorations {
                scope: agent_protocol::operations::ListDecorationScope::Root,
                projects: vec![],
                threads: vec![],
            }
            .apply(
                &mut snapshot,
                "",
                agent_protocol::operations::ListDecorations {
                    icons: [(
                        "project".into(),
                        icon.map(|png| crate::models::ProjectIcon {
                            light_png: png.into(),
                            dark_png: "BAUG".into(),
                        }),
                    )]
                    .into(),
                    statuses: vec![],
                },
            );
            let list = snapshot.thread_list().unwrap();
            let project = &list.projects[0];
            assert_eq!(project.name, "Renamed project");
            if let Some(images) = &project.icon {
                assert!(icon.is_some());
                assert_eq!(images.light_png, [1, 2, 3]);
                assert_eq!(images.dark_png, [4, 5, 6]);
            } else {
                assert!(icon.is_none());
            }
            assert_eq!(project.threads[0].id.id, "task");
        }
    }

    #[rstest::rstest]
    #[case(&[], &[])]
    #[case(&["codex"], &["codex"])]
    #[case(&["claude"], &["claude"])]
    #[case(&["codex", "claude"], &["claude", "codex"])]
    fn list_notices_follow_logged_in_providers(
        #[case] logged_in: &[&str],
        #[case] expected: &[&str],
        #[values(false, true)] root_errors: bool,
    ) {
        let mut snapshot = snapshot_list(json!([]));
        let errors = json!({"codex":{"message":"offline"},"claude":{"message":"offline"}});
        Arc::make_mut(snapshot.lists.threads.as_mut().unwrap()).provider_errors =
            Some(serde_json::from_value(errors).unwrap());
        Arc::make_mut(&mut snapshot.lists.project_threads)
            .insert("project".into(), snapshot.lists.threads.clone().unwrap());
        if !root_errors {
            Arc::make_mut(snapshot.lists.threads.as_mut().unwrap()).provider_errors = None;
        }
        Arc::make_mut(&mut snapshot.lists.expanded_projects).insert("project".into(), 5);
        // Exercise login and logout with the same already-loaded list.
        for providers in [logged_in, &[], logged_in] {
            let previous = Arc::new(snapshot.clone());
            let previous_list = previous.thread_list();
            crate::state::operations::ListAccounts {}.apply(
                &mut snapshot,
                "",
                serde_json::from_value(json!({
                    "accounts": providers.iter().map(|provider| json!({
                        "provider":provider,"id":"account"
                    })).collect::<Vec<_>>(),
                    "selected":{}
                }))
                .unwrap(),
            );
            let list = snapshot.thread_list().unwrap();
            let recent = snapshot.recent_thread_list().unwrap();
            // Mobile clients cache this projection using the publication flag.
            if previous_list.as_ref() != Some(&list) {
                assert!(!snapshot.list_source().equivalent(&previous.list_source()));
            }
            if providers.is_empty() {
                assert!(list.notice.is_none());
                assert!(recent.notice.is_none());
                assert!(list.projects[0].error.is_none());
            } else {
                assert_eq!(recent.notice, list.notice);
                let project_error = list.projects[0].error.as_ref().unwrap();
                if root_errors {
                    assert!(list.notice.as_ref().unwrap().contains("部分結果"));
                } else {
                    assert!(list.notice.is_none());
                }
                for provider in ["codex", "claude"] {
                    let visible = expected.contains(&provider);
                    assert_eq!(
                        list.notice
                            .as_ref()
                            .is_some_and(|notice| notice.contains(provider)),
                        root_errors && visible
                    );
                    assert_eq!(project_error.message.contains(provider), visible);
                }
            }
        }
    }

    #[test]
    fn non_login_provider_errors_remain_visible_without_accounts() {
        let mut snapshot = snapshot_list(json!([]));
        crate::state::operations::ListAccounts {}.apply(
            &mut snapshot,
            "",
            serde_json::from_value(json!({"accounts":[],"selected":{}})).unwrap(),
        );
        let page = Arc::make_mut(snapshot.lists.threads.as_mut().unwrap());
        page.provider_errors = Some(Default::default());
        assert!(snapshot.thread_list().unwrap().notice.is_none());
        Arc::make_mut(snapshot.lists.threads.as_mut().unwrap()).provider_errors =
            Some(serde_json::from_value(json!({"deepseek":{"message":"offline"}})).unwrap());
        assert!(
            snapshot
                .thread_list()
                .unwrap()
                .notice
                .unwrap()
                .contains("deepseek")
        );
    }

    #[test]
    fn unavailable_provider_keeps_explicitly_stale_cached_summaries() {
        let mut snapshot = Snapshot::default();
        crate::state::list_sync::seed(
            &mut snapshot,
            serde_json::from_value(json!({
                "data":[
                    {"id":{"provider":"codex","id":"native"},"name":"Cached","listStale":true},
                    {"id":{"provider":"claude","id":"uuid"},"name":"Available"}
                ],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5
            }))
            .unwrap(),
        );
        let list = snapshot.thread_list().unwrap();
        assert_eq!(list.threads.len(), 2);
        assert!(list.notice.unwrap().contains("部分結果"));
        assert!(
            list.threads
                .iter()
                .find(|thread| thread.id
                    == agent_protocol::session::SessionRef {
                        provider: Some(agent_protocol::session::ProviderKind::Codex),
                        id: "native".into()
                    })
                .unwrap()
                .title
                .contains("未確認")
        );
        assert!(snapshot.panes.primary().error.is_none());
    }
    #[rstest::rstest]
    fn list_preserves_has_changes_alongside_activity_after_serialization_and_refresh(
        #[values(false, true)] active: bool,
        #[values(false, true)] unread: bool,
        #[values(None, Some(true), Some(false))] status: Option<bool>,
    ) {
        let mut snapshot = Snapshot::default();
        std::sync::Arc::make_mut(&mut snapshot.lists.activity)
            .active
            .insert(
                agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "task".into(),
                },
                active,
            );
        if unread {
            std::sync::Arc::make_mut(&mut snapshot.lists.activity)
                .unread
                .insert(agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "task".into(),
                });
        }
        let mut thread = json!({"id":{"provider":"codex","id":"task"},"name":"Worktree task",
            "status":if active {"running"} else {"idle"}});
        if let Some(status) = status {
            thread["hasChanges"] = json!(status);
        }
        let page: models::ThreadList = serde_json::from_value(json!({
            "data":[thread], "projects":[],
            "hasMore":false,"hasMoreProjects":false,"limit":5, }))
        .unwrap();
        snapshot.lists.threads = Some(Arc::new(page));
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let rows = restored.thread_list().unwrap();
        assert_eq!(rows.threads[0].has_changes, status);
        assert_eq!(rows.threads[0].active, active);
        assert_eq!(rows.threads[0].unread, unread);
    }
}
