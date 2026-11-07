//! The visible list combines wire summaries with locally observed activity.
use crate::{
    models::{Project, ProjectRoot, task_active, task_title},
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
pub struct ThreadSummary {
    pub host_id: String,
    pub id: crate::session::SessionRef,
    pub title: String,
    pub project_id: Option<String>,
    pub active: bool,
    pub waiting: bool,
    pub unread: bool,
    pub worktree_status: Option<crate::models::WorktreeStatus>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadList {
    pub loading_projects: bool,
    pub loading_chats: bool,
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<ProjectSummary>,
    pub hidden_projects: Vec<ProjectSummary>,
    pub has_more: bool,
    pub has_more_projects: bool,
}

/// Only the data consumed by list presentation. Conversation bodies and
/// operation generations are deliberately absent from its equality contract.
pub(crate) struct ListSource<'a> {
    lists: &'a crate::state::Lists,
    conversations: &'a std::sync::Arc<
        BTreeMap<crate::session::SessionRef, std::sync::Arc<crate::models::Thread>>,
    >,
    operations: &'a crate::state::Operations,
    storage_scope: &'a str,
    connected: bool,
}
impl<'a> ListSource<'a> {
    pub(crate) fn new(
        lists: &'a crate::state::Lists,
        conversations: &'a std::sync::Arc<
            BTreeMap<crate::session::SessionRef, std::sync::Arc<crate::models::Thread>>,
        >,
        operations: &'a crate::state::Operations,
        storage_scope: &'a str,
        connected: bool,
    ) -> Self {
        Self {
            lists,
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
        self.lists.threads.same(&other.lists.threads)
            && self
                .lists
                .project_threads
                .same(&other.lists.project_threads)
            && self
                .lists
                .expanded_projects
                .same(&other.lists.expanded_projects)
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
            && self.storage_scope == other.storage_scope
            && self.connected == other.connected
            && (self.lists.task_activity.same(&other.lists.task_activity)
                || self.waiting_sessions() == other.waiting_sessions())
            && self.same_phases(other)
            && self.same_shortcuts(other)
    }
    fn same_phases(&self, other: &Self) -> bool {
        use crate::models::ListPart;
        use crate::state::operations::OperationKey as K;
        if crate::state::Same::same(self.operations, other.operations) {
            return true;
        }
        [
            K::SessionList,
            K::SessionPage {
                part: ListPart::Projects,
            },
            K::SessionPage {
                part: ListPart::Chats,
            },
        ]
        .into_iter()
        .chain(
            self.lists
                .threads
                .iter()
                .flat_map(|page| &page.projects)
                .map(|project| K::ProjectList {
                    project_id: project.id.clone(),
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
        thread.worktree_status,
        thread.list_stale,
    )
}

impl Snapshot {
    pub(crate) fn list_source(&self) -> ListSource<'_> {
        ListSource::new(
            &self.lists,
            &self.sessions.conversations,
            &self.operations,
            &self.connection.storage_scope,
            self.connection.connected,
        )
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// The selected folder is separate from the provider's execution directory.
    pub fn selected_directory(&self) -> String {
        let thread = self
            .pane
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.thread_metadata(id));
        if thread.is_some_and(|thread| {
            thread.project_id == (crate::models::ProjectMembership::Unassigned {})
        }) {
            String::new()
        } else {
            self.pane.navigation.cwd.clone()
        }
    }

    pub fn thread_list(&self) -> Option<ThreadList> {
        self.list_source().thread_list()
    }
    pub fn recent_thread_list(&self) -> Option<ThreadList> {
        self.list_source().recent_thread_list()
    }
}

impl ListSource<'_> {
    fn waiting_sessions(&self) -> &[crate::session::SessionRef] {
        self.lists
            .task_activity
            .as_ref()
            .map_or(&[], |state| state.waiting.as_slice())
    }
    fn list_projection(
        &self,
        list: &crate::models::ThreadList,
        include_standalone: bool,
        additional_queries: usize,
    ) -> (ThreadList, ChatPlacements<'_>) {
        let waiting = self.waiting_sessions();
        let search = self.lists.list_query.search_term.to_lowercase();
        let project_ids: HashSet<_> = list
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect();
        let preferences = self.lists.project_preferences.get(self.storage_scope);
        let mut projects = Vec::new();
        let mut hidden_projects = Vec::new();
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
                    self.lists.expanded_projects.contains_key(&project.id),
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
        let (queries, shortcut_queries) = projects
            .iter()
            .filter(|(_, _, _, expanded)| *expanded)
            .fold(
                (
                    (if include_standalone {
                        list.data.len()
                    } else {
                        0
                    })
                    .saturating_add(additional_queries),
                    0usize,
                ),
                |(queries, shortcuts), (_, prefs, page, _)| {
                    (
                        queries.saturating_add(page.map_or(0, |page| page.data.len())),
                        shortcuts.saturating_add(prefs.map_or(0, |prefs| prefs.placed_chats.len())),
                    )
                },
            );
        let metadata = if shortcut_queries >= 128
            && self.conversations.len().saturating_mul(2) < shortcut_queries
            && self
                .lists
                .project_threads
                .values()
                .fold(list.data.len(), |count, page| {
                    count.saturating_add(page.data.len())
                })
                >= 128
        {
            Some(crate::state::index_missing_metadata(
                self.conversations,
                self.listed_threads(),
                projects
                    .iter()
                    .filter(|(_, _, _, expanded)| *expanded)
                    .flat_map(|(_, prefs, ..)| prefs.iter().flat_map(|prefs| &prefs.placed_chats))
                    .map(|chat| &chat.id),
            ))
        } else {
            None
        };
        let placements = ChatPlacements::new(preferences, queries);
        let summaries = if include_standalone {
            thread_summaries(
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
                waiting,
                // The filter has already resolved these rows as standalone.
                &HashSet::new(),
                None,
            )
        } else {
            Vec::new()
        };
        (
            ThreadList {
                threads: summaries,
                loading_projects: self.operation_running(
                    crate::state::operations::OperationKey::SessionPage {
                        part: crate::models::ListPart::Projects,
                    },
                ),
                loading_chats: self.operation_running(
                    crate::state::operations::OperationKey::SessionPage {
                        part: crate::models::ListPart::Chats,
                    },
                ),
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
                                message: "接続するとタスクを読み込めます".into(),
                                details: None,
                            });
                        }
                        if let Some(operation) = self.operations.get(
                            &crate::state::operations::OperationKey::ProjectList {
                                project_id: project.id.clone(),
                            },
                        ) {
                            match &operation.phase {
                                crate::state::operations::OperationPhase::Running => {
                                    summary.loading = true
                                }
                                crate::state::operations::OperationPhase::Failed { message } => {
                                    summary.error = Some(super::error::error_text(message))
                                }
                            }
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
                                    data,
                                    &self.lists.activity.active,
                                    &self.lists.activity.unread,
                                    waiting,
                                    &project_ids,
                                    None,
                                )
                            } else {
                                thread_summaries(
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
                                    waiting,
                                    &project_ids,
                                    None,
                                )
                            };
                            summary.has_more = page.has_more;
                        }
                        let placed_ids = prefs
                            .filter(|prefs| {
                                prefs.archived_before.is_some() && prefs.placed_chats.len() >= 64
                            })
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
                                metadata
                                    .as_ref()
                                    .and_then(|metadata| metadata.get(&chat.id))
                                    .copied()
                                    .unwrap_or_else(|| self.thread_metadata(&chat.id))
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
                                            && thread.id.as_ref().is_some_and(
                                                |id| match &placed_ids {
                                                    Some(ids) => ids.contains(id),
                                                    None => prefs.is_some_and(|prefs| {
                                                        prefs
                                                            .placed_chats
                                                            .iter()
                                                            .any(|chat| &chat.id == id)
                                                    }),
                                                },
                                            ),
                                    )
                            })
                            .filter(|thread| {
                                search.is_empty()
                                    || task_title(thread.name.as_deref(), thread.preview.as_deref())
                                        .to_lowercase()
                                        .contains(&search)
                            })
                            .collect::<Vec<_>>();
                        summary.threads.splice(
                            0..0,
                            thread_summaries(
                                placed.iter().map(AsRef::as_ref),
                                &self.lists.activity.active,
                                &self.lists.activity.unread,
                                waiting,
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
        let additional_queries = self
            .lists
            .project_threads
            .values()
            .fold(source.data.len(), |queries, page| {
                queries.saturating_add(page.data.len())
            });
        let (mut list, placements) = self.list_projection(source, false, additional_queries);
        let waiting = self.waiting_sessions();
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
            if rows
                .get(id)
                .is_none_or(|old| old.list_revision <= thread.list_revision)
            {
                rows.insert(id, thread);
            }
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
            rows.into_iter(),
            &self.lists.activity.active,
            &self.lists.activity.unread,
            waiting,
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
        for project in &source.projects {
            let key = crate::state::operations::OperationKey::ProjectList {
                project_id: project.id.clone(),
            };
            list.loading_chats |= self.operation_running(key);
        }
        Some(list)
    }

    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.lists.threads.as_deref()?;
        Some(self.list_projection(list, true, 0).0)
    }
}

fn thread_summaries<'a>(
    data: impl Iterator<Item = &'a crate::models::Thread>,
    active: &BTreeMap<crate::session::SessionRef, bool>,
    unread: &BTreeSet<crate::session::SessionRef>,
    waiting: &[crate::session::SessionRef],
    project_ids: &HashSet<&str>,
    project_override: Option<&str>,
) -> Vec<ThreadSummary> {
    let mut indexed = None;
    data.filter(|thread| thread.parent_id.is_none())
        .filter_map(|thread| Some((thread, thread.id.as_ref()?)))
        .enumerate()
        .map(|(index, (thread, id))| {
            let active = task_active(active.get(id).copied(), thread.status);
            let unread = unread.contains(id);
            let waiting = if waiting.len() >= 64 && index >= 64 {
                indexed
                    .get_or_insert_with(|| waiting.iter().collect::<HashSet<_>>())
                    .contains(id)
            } else {
                waiting.contains(id)
            };
            ThreadSummary {
                host_id: String::new(),
                id: id.clone(),
                title: task_title(thread.name.as_deref(), thread.preview.as_deref()).to_owned()
                    + if thread.list_stale == Some(true) {
                        "（保存済み・未確認）"
                    } else {
                        ""
                    },
                project_id: project_override
                    .or_else(|| thread.project_id.as_deref())
                    .filter(|id| project_ids.contains(*id))
                    .map(str::to_owned),
                active,
                waiting,
                unread,
                worktree_status: thread.worktree_status,
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
    use crate::state::operations::ListSessions;
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
                        "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                    .unwrap(),
                )),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn waiting_updates_all_list_scopes_without_opening_conversations() {
        use crate::state::{Event, Intent, reduce};
        use agent_protocol::{
            live_activity::{TaskActivityState, TaskActivitySummary},
            session::{ProviderKind, SessionRef},
        };
        let id = |provider, name: &str| SessionRef::new(provider, name.into()).unwrap();
        let root = id(ProviderKind::Codex, "same");
        let other = id(ProviderKind::Claude, "same");
        let project = id(ProviderKind::Codex, "project");
        let placed = id(ProviderKind::Claude, "placed");
        let affected = vec![root.clone(), project.clone(), placed.clone()];
        let mut state = snapshot_list(json!([
            {"id":root}, {"id":other}, {"id":placed}
        ]));
        let page = snapshot_list(json!([{"id":project,"projectId":"project"}]))
            .lists
            .threads
            .unwrap();
        Arc::make_mut(&mut state.lists.project_threads).insert("project".into(), page);
        state = reduce(
            &state,
            Event::Intent(Intent::MoveChatToProject {
                thread_id: placed,
                project_id: "project".into(),
            }),
        )
        .0;
        let update = |revision, waiting: bool| TaskActivityState {
            revision,
            display: TaskActivitySummary {
                waiting: if waiting { 3 } else { 0 },
                running: if waiting { 0 } else { 3 },
                ..Default::default()
            }
            .display(),
            statuses: affected
                .iter()
                .cloned()
                .map(|id| (id, models::SessionStatus::Running))
                .collect(),
            waiting: if waiting {
                affected.clone()
            } else {
                Vec::new()
            },
            unread: Vec::new(),
        };
        for (revision, requested, expected) in
            [(1, true, true), (2, false, false), (1, true, false)]
        {
            // Exercise the wire contract as well as revision ordering.
            let received =
                serde_json::from_value(serde_json::to_value(update(revision, requested)).unwrap())
                    .unwrap();
            let _ = state.accept_task_activity(received);
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
    fn recent_chats_merge_closed_projects_in_update_order_and_page_globally() {
        let mut snapshot = snapshot_list(json!([
            {"id":{"provider":"codex","id":"standalone"},"updatedAt":10},
            {"id":{"provider":"codex","id":"duplicate"},"updatedAt":5}
        ]));
        let page = serde_json::from_value(json!({
            "data":[
                {"id":{"provider":"codex","id":"project-chat"},"projectId":"project","updatedAt":30},
                {"id":{"provider":"codex","id":"duplicate"},"projectId":"project","updatedAt":20,"listRevision":2},
                {"id":{"provider":"claude","id":"duplicate"},"projectId":"project","updatedAt":15},
                {"id":{"provider":"codex","id":"agent"},"parentId":{"provider":"codex","id":"project-chat"},"projectId":"project","updatedAt":40}
            ],"projects":[],"limit":5,"hasMore":false,"hasMoreProjects":false,"projectPages":{}
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
        assert_eq!(rows, [(Claude, "child"), (Codex, "fork")]);
    }

    #[test]
    fn list_keeps_only_standalone_chats_at_root_and_exposes_known_membership() {
        let mut snapshot = Snapshot::default();
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data": [
                    {"id":{"provider":"codex","id":"assigned"}, "projectId":"known"},
                    {"id":{"provider":"codex","id":"missing"}, "projectId":"absent"},
                    {"id":{"provider":"codex","id":"chat"}, "projectId":null},
                    {"id":{"provider":"codex","id":"unknown"}}
                ],
                "projects":[{"id":"known", "name":"Project", "roots":[]}],
                 "hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
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
        assert!(list.has_more);
        assert!(!list.has_more_projects);
    }

    #[test]
    fn returned_project_header_replaces_branding_while_preserving_loaded_tasks() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.lists.expanded_projects).insert("project".into(), 5);
        ListSessions::new(Default::default()).apply(
            &mut snapshot,
            serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"task"},"projectId":"project"}],
                "projects":[{"id":"project","name":"Old project","roots":[],"icon":{"lightPng":"BAUG","darkPng":"BAUG"}}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{"project":{"limit":5,"hasMore":false}},"limit":5
            }))
            .unwrap(),
        );
        for icon in [Some("AQID"), None] {
            ListSessions::new(Default::default()).apply(&mut snapshot, serde_json::from_value(json!({
                "data":[],
                "projects":[{"id":"project","name":"Renamed project","roots":[],"icon":icon.map(|png| json!({"lightPng":png,"darkPng":"BAUG"}))}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            })).unwrap());
            crate::state::operations::ReadListDecorations {
                scope: agent_protocol::operations::ListDecorationScope::Root { part: None },
                projects: vec![],
                threads: vec![],
            }
            .apply(
                &mut snapshot,
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
    fn list_preserves_worktree_status_alongside_activity_after_serialization_and_refresh(
        #[values(false, true)] active: bool,
        #[values(false, true)] unread: bool,
        #[values(
            None,
            Some(models::WorktreeStatus::Unmerged),
            Some(models::WorktreeStatus::Merged)
        )]
        status: Option<models::WorktreeStatus>,
    ) {
        let mut snapshot = Snapshot::default();
        std::sync::Arc::make_mut(&mut snapshot.lists.activity)
            .active
            .insert(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "task".into(),
                },
                active,
            );
        if unread {
            std::sync::Arc::make_mut(&mut snapshot.lists.activity)
                .unread
                .insert(agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "task".into(),
                });
        }
        let mut thread = json!({"id":{"provider":"codex","id":"task"},"name":"Worktree task",
            "status":if active {"running"} else {"idle"}});
        if let Some(status) = status {
            thread["worktreeStatus"] = json!(status);
        }
        let page: models::ThreadList = serde_json::from_value(json!({
            "data":[thread], "projects":[],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
        .unwrap();
        ListSessions::new(Default::default()).apply(&mut snapshot, page);
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let rows = restored.thread_list().unwrap();
        assert_eq!(rows.threads[0].worktree_status, status);
        assert_eq!(rows.threads[0].active, active);
        assert_eq!(rows.threads[0].unread, unread);
    }
}
