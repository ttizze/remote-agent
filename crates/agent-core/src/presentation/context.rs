//! Conversation chrome and actions, with no dependency on a draft's text.
use super::{
    conversation::HistoryControl,
    error::{ErrorNotice, ErrorScope},
};
use crate::state::{
    Navigation, Snapshot,
    operations::{OperationKey, OperationPhase},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, PartialEq)]
struct OperationControl {
    phase: OperationPhase,
    error: Option<super::error::ErrorText>,
}

/// What the pane shows for its selected conversation while it has no history
/// to render: never a new chat's prompt, which would read as a different one.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationOpening {
    /// Its history is on its way from the Host.
    Loading,
    /// Reading its history failed; opening it again retries.
    Failed { error: super::error::ErrorText },
}

impl ConversationOpening {
    fn project(snapshot: &Snapshot, pane: &crate::state::PaneState) -> Option<Self> {
        let session = pane.navigation.thread_id.clone()?;
        if snapshot.sessions.conversations.contains_key(&session) {
            return None;
        }
        Some(
            match snapshot.operation_error(OperationKey::History { session }) {
                Some(error) => Self::Failed { error },
                None => Self::Loading,
            },
        )
    }
}

/// T3 Code's thread error banner over the timeline: the latest turn's failure
/// while it stands, unless it is a usage limit, which the timeline's row
/// already explains.
fn thread_error(
    language: crate::localization::UiLanguage,
    thread: &crate::models::Thread,
) -> Option<String> {
    use agent_protocol::execution::{ErrorCategory, TurnStatus};
    let turn = thread.turns.as_ref()?.last()?;
    let error = turn.error.as_ref().filter(|error| {
        turn.status == TurnStatus::Failed && error.category != ErrorCategory::UsageLimit
    })?;
    Some(language.text(&error.message).into())
}

#[derive(Clone, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConversationContext {
    pub navigation: Navigation,
    pub connected: bool,
    pub epoch: u64,
    opening: Option<ConversationOpening>,
    title: Option<String>,
    pub home: Option<String>,
    directory: String,
    project: Option<super::list::ProjectSummary>,
    thread_error: Option<String>,
    pub history: Option<HistoryControl>,
    pub docked: Vec<super::conversation::Request>,
    notices: Vec<ErrorNotice>,
    operations: BTreeMap<OperationKey, OperationControl>,
    workspace_error: Option<super::error::ErrorText>,
}

impl ConversationContext {
    pub(crate) fn project(
        snapshot: &Snapshot,
        pane: &crate::state::PaneState,
        pane_id: &str,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        let session = pane.navigation.thread_id.as_ref();
        let directory = snapshot.selected_directory(pane);
        let next = Self {
            workspace_error: snapshot.operation_error(OperationKey::WorkspaceReview {
                pane: (pane_id).to_owned(),
            }),
            navigation: pane.navigation.as_ref().clone(),
            opening: ConversationOpening::project(snapshot, pane),
            connected: snapshot.connection.connected,
            epoch: snapshot.connection.epoch,
            docked: session
                .into_iter()
                .flat_map(|id| snapshot.docked_requests(id.clone()))
                .filter(|request| {
                    !matches!(
                        request.request_body,
                        agent_protocol::requests::RequestBody::Question { .. }
                    )
                })
                .collect(),
            title: session.and_then(|id| {
                snapshot
                    .sessions
                    .conversations
                    .get(id)
                    .and_then(|thread| thread.name.clone())
                    .or_else(|| {
                        snapshot
                            .listed_threads()
                            .find(|thread| thread.id.as_ref() == Some(id))
                            .and_then(|thread| thread.name.clone())
                    })
            }),
            home: snapshot.connection.host_home_directory.clone(),
            project: snapshot.directory_project(&directory),
            directory,
            thread_error: session
                .and_then(|id| snapshot.sessions.conversations.get(id))
                .and_then(|thread| thread_error(snapshot.catalog.ui_language, thread)),
            history: session.and_then(|id| snapshot.history_control(id.clone())),
            notices: snapshot.error_notices(
                pane_id,
                ErrorScope::Conversation {
                    draft_key: pane.navigation.draft_key.clone(),
                },
            ),
            operations: snapshot
                .operations
                .iter()
                .filter(|(key, _)| match key {
                    OperationKey::Fork {
                        session: target, ..
                    }
                    | OperationKey::Rewind { session: target }
                    | OperationKey::TurnItems {
                        session: target, ..
                    } => Some(target) == session,
                    OperationKey::Item { item } => Some(&item.thread_id) == session,
                    OperationKey::Request { id } => session
                        .and_then(|id| snapshot.sessions.conversations.get(id))
                        .is_some_and(|thread| thread.requests.contains_key(id)),
                    _ => false,
                })
                .map(|(key, state)| {
                    (
                        key.clone(),
                        OperationControl {
                            phase: state.phase.clone(),
                            error: snapshot.operation_error(key.clone()),
                        },
                    )
                })
                .collect(),
        };
        if let Some(previous) = previous
            && next == **previous
        {
            return previous.clone();
        }
        Arc::new(next)
    }
    pub fn operation_running(&self, key: &OperationKey) -> bool {
        self.operations
            .get(key)
            .is_some_and(|control| control.phase == OperationPhase::Running)
    }
    pub fn operation_error(&self, key: &OperationKey) -> Option<&super::error::ErrorText> {
        self.operations
            .get(key)
            .and_then(|control| control.error.as_ref())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConversationContext {
    pub fn navigation(&self) -> Navigation {
        self.navigation.clone()
    }
    pub fn opening(&self) -> Option<ConversationOpening> {
        self.opening.clone()
    }
    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }
    /// The listed project the conversation works in.
    pub fn workspace_project(&self) -> Option<super::list::ProjectSummary> {
        self.project.clone()
    }
    pub fn thread_error(&self) -> Option<String> {
        self.thread_error.clone()
    }
    pub fn workspace_error(&self) -> Option<super::error::ErrorText> {
        self.workspace_error.clone()
    }
    pub fn home(&self) -> Option<String> {
        self.home.clone()
    }
    pub fn selected_directory(&self) -> String {
        self.directory.clone()
    }
    pub fn history(&self) -> Option<HistoryControl> {
        self.history.clone()
    }
    pub fn notices(&self) -> Vec<ErrorNotice> {
        self.notices.clone()
    }
    pub fn docked_request(&self) -> Option<super::conversation::Request> {
        self.docked.first().cloned()
    }
    pub fn request_phase(&self, id: agent_protocol::ids::RequestId) -> Option<OperationPhase> {
        self.operations
            .get(&OperationKey::Request { id })
            .map(|control| control.phase.clone())
    }
    pub fn request_error(
        &self,
        id: agent_protocol::ids::RequestId,
    ) -> Option<super::error::ErrorText> {
        self.operation_error(&OperationKey::Request { id }).cloned()
    }
    pub fn fork_phase(&self, turn: agent_protocol::ids::TurnId) -> Option<OperationPhase> {
        let session = self.navigation.thread_id.clone()?;
        self.operations
            .get(&OperationKey::Fork { session, turn })
            .map(|control| control.phase.clone())
    }
    pub fn rewinding(&self) -> bool {
        self.navigation.thread_id.clone().is_some_and(|session| {
            self.operations
                .get(&OperationKey::Rewind { session })
                .is_some_and(|control| control.phase == OperationPhase::Running)
        })
    }
    pub fn fork_error(&self, turn: agent_protocol::ids::TurnId) -> Option<super::error::ErrorText> {
        let session = self.navigation.thread_id.clone()?;
        self.operation_error(&OperationKey::Fork { session, turn })
            .cloned()
    }
    /// Why the work a folded turn expands to could not be loaded.
    pub fn turn_items_error(
        &self,
        turn: agent_protocol::ids::TurnId,
    ) -> Option<super::error::ErrorText> {
        let session = self.navigation.thread_id.clone()?;
        self.operation_error(&OperationKey::TurnItems { session, turn })
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::operations::{OperationState, ReadItem};
    use agent_protocol::{
        requests::{Request, RequestBody, RequestTarget},
        session::{ProviderKind, RequestDelivery, SessionRef},
    };

    #[test]
    fn actions_follow_the_selected_conversation_and_reuse_unchanged_controls() {
        let selected = SessionRef::new(ProviderKind::Codex, "selected".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(selected.clone());
        let request = Request {
            id: "request".into(),
            target: RequestTarget::Session,
            delivery: RequestDelivery::Awaiting,
            body: RequestBody::ToolExecution {
                tool: "tool".into(),
                namespace: None,
                arguments: serde_json::json!({}),
            },
        };
        let question = Request {
            id: "question".into(),
            body: RequestBody::Question {
                questions: Vec::new(),
            },
            ..request.clone()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            selected.clone(),
            Arc::new(crate::models::Thread {
                id: Some(selected.clone()),
                requests: [request, question]
                    .into_iter()
                    .map(|request| (request.id.clone(), Arc::new(request)))
                    .collect(),
                ..Default::default()
            }),
        );
        let fork = OperationKey::Fork {
            session: selected.clone(),
            turn: "turn".into(),
        };
        let turn_items = OperationKey::TurnItems {
            session: selected.clone(),
            turn: "turn".into(),
        };
        let item = OperationKey::Item {
            item: ReadItem {
                thread_id: selected,
                turn_id: "turn".into(),
                item_id: "item".into(),
            },
        };
        let request = OperationKey::Request {
            id: "request".into(),
        };
        for key in [&fork, &turn_items, &item, &request] {
            Arc::make_mut(&mut snapshot.operations).insert(
                key.clone(),
                OperationState {
                    generation: 1,
                    phase: OperationPhase::Running,
                },
            );
        }
        let initial = ConversationContext::project(&snapshot, snapshot.panes.primary(), "", None);
        assert_eq!(
            initial.docked.len(),
            1,
            "questions use the composer's field"
        );
        assert_eq!(initial.docked[0].id.as_str(), "request");
        for key in [&fork, &turn_items, &item, &request] {
            assert!(initial.operation_running(key));
            assert!(initial.operation_error(key).is_none());
        }
        assert_eq!(
            initial.fork_phase("turn".into()),
            Some(OperationPhase::Running)
        );

        for key in [
            OperationKey::Fork {
                session: other.clone(),
                turn: "turn".into(),
            },
            OperationKey::TurnItems {
                session: other.clone(),
                turn: "turn".into(),
            },
            OperationKey::Item {
                item: ReadItem {
                    thread_id: other,
                    turn_id: "turn".into(),
                    item_id: "item".into(),
                },
            },
            OperationKey::Request {
                id: "unrelated".into(),
            },
        ] {
            Arc::make_mut(&mut snapshot.operations).insert(
                key.clone(),
                OperationState {
                    generation: 2,
                    phase: OperationPhase::Running,
                },
            );
            let next = ConversationContext::project(
                &snapshot,
                snapshot.panes.primary(),
                "",
                Some(&initial),
            );
            assert!(Arc::ptr_eq(&initial, &next));
            assert!(!next.operation_running(&key));
        }
        for key in [&fork, &turn_items, &item, &request] {
            Arc::make_mut(&mut snapshot.operations)
                .get_mut(key)
                .unwrap()
                .generation += 1;
        }
        assert!(Arc::ptr_eq(
            &initial,
            &ConversationContext::project(&snapshot, snapshot.panes.primary(), "", Some(&initial)),
        ));
        for key in [&fork, &turn_items, &item, &request] {
            Arc::make_mut(&mut snapshot.operations)
                .get_mut(key)
                .unwrap()
                .phase = OperationPhase::Failed {
                message: "failure".into(),
            };
        }
        let failed =
            ConversationContext::project(&snapshot, snapshot.panes.primary(), "", Some(&initial));
        for key in [&fork, &turn_items, &item, &request] {
            assert!(!failed.operation_running(key));
            assert_eq!(
                failed.operation_error(key),
                snapshot.operation_error(key.clone()).as_ref()
            );
        }
        assert!(failed.fork_error("turn".into()).is_some());
        assert!(failed.turn_items_error("turn".into()).is_some());
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id = None;
        let empty =
            ConversationContext::project(&snapshot, snapshot.panes.primary(), "", Some(&failed));
        assert!(empty.fork_phase("turn".into()).is_none());
        assert!(empty.fork_error("turn".into()).is_none());
        assert!(empty.turn_items_error("turn".into()).is_none());
        for key in [&fork, &turn_items, &item, &request] {
            assert!(empty.operation_error(key).is_none());
        }
    }
}

#[cfg(test)]
mod chrome_tests {
    use super::*;
    use crate::{
        models,
        session::{ProviderKind, SessionRef},
    };
    use agent_protocol::execution::{ErrorCategory, ExecutionError, RetryEvidence, TurnStatus};

    fn context(snapshot: &Snapshot) -> Arc<ConversationContext> {
        ConversationContext::project(snapshot, snapshot.panes.primary(), "", None)
    }

    #[test]
    fn the_header_names_the_listed_project_of_the_selected_folder() {
        let mut snapshot = Snapshot::default();
        snapshot.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "limit": 20, "data": [], "hasMore": false, "hasMoreProjects": false, "projectPages": {},
                "projects": [{"id": "p", "name": "Bex app", "roots": [{"path": "/work/bex"}, {"path": "/work/other"}], "icon": null}],
            }))
            .unwrap(),
        ));
        let select = |snapshot: &mut Snapshot, cwd: &str| {
            Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
                cwd.into();
        };
        select(&mut snapshot, "/work/bex");
        let project = context(&snapshot).workspace_project().unwrap();
        assert_eq!(
            (project.name.as_str(), project.monogram.as_str()),
            ("Bex app", "BA")
        );
        // Only a project's first folder is its own; a chat without a folder has none.
        for cwd in ["/work/other", "/work", ""] {
            select(&mut snapshot, cwd);
            assert!(context(&snapshot).workspace_project().is_none(), "{cwd}");
        }
    }

    #[test]
    fn the_banner_shows_the_latest_turn_failure_unless_retrying_or_a_usage_limit() {
        let session = SessionRef::new(ProviderKind::Codex, "failed".into()).unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(session.clone());
        let mut show = |turns: Vec<models::Turn>| {
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                session.clone(),
                Arc::new(models::Thread {
                    turns: Some(turns.into_iter().map(Arc::new).collect()),
                    ..Default::default()
                }),
            );
            context(&snapshot).thread_error()
        };
        let failure = |category, retrying: bool| ExecutionError {
            category,
            message: "context is full".into(),
            retry: retrying.then(|| RetryEvidence::new(None, None)),
            ..Default::default()
        };
        let turn = |status, error| models::Turn {
            status,
            error,
            ..Default::default()
        };
        let failed = turn(
            TurnStatus::Failed,
            Some(failure(ErrorCategory::ContextLimit, false)),
        );
        assert_eq!(show(vec![failed.clone()]), Some("context is full".into()));
        // A later turn that did not fail takes the banner away.
        assert_eq!(show(vec![failed, turn(TurnStatus::Completed, None)]), None);
        assert_eq!(
            show(vec![turn(
                TurnStatus::Running,
                Some(failure(ErrorCategory::Network, true))
            )]),
            None
        );
        assert_eq!(
            show(vec![turn(
                TurnStatus::Failed,
                Some(failure(ErrorCategory::UsageLimit, false))
            )]),
            None
        );
        assert_eq!(show(Vec::new()), None);
    }
}

#[cfg(test)]
mod opening_tests {
    use super::*;
    use crate::{
        models,
        session::{ProviderKind, SessionRef},
        state::operations::OperationState,
    };

    #[test]
    fn a_selected_conversation_without_history_loads_then_offers_a_retry() {
        let session = SessionRef::new(ProviderKind::Codex, "opening".into()).unwrap();
        let mut snapshot = Snapshot::default();
        let opening = |snapshot: &Snapshot| {
            ConversationContext::project(snapshot, snapshot.panes.primary(), "", None).opening()
        };
        assert_eq!(opening(&snapshot), None, "a new chat opens nothing");

        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(session.clone());
        assert_eq!(opening(&snapshot), Some(ConversationOpening::Loading));
        let key = OperationKey::History {
            session: session.clone(),
        };
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert_eq!(opening(&snapshot), Some(ConversationOpening::Loading));
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 2,
                phase: OperationPhase::Failed {
                    message: "history unavailable".into(),
                },
            },
        );
        assert_eq!(
            opening(&snapshot),
            Some(ConversationOpening::Failed {
                error: crate::presentation::error::error_text(
                    crate::localization::UiLanguage::Japanese,
                    "history unavailable"
                )
            })
        );

        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session, Arc::new(models::Thread::default()));
        assert_eq!(opening(&snapshot), None, "its history shows instead");
    }
}
