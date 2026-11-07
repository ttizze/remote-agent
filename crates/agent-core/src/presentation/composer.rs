//! A composer's own inputs and controls, independent of transcript deltas.
use super::{conversation::Request, error::ErrorText, model_settings::ModelQuickControls};
use crate::{
    models::{Model, ModelRef},
    session::ProviderKind,
    state::{
        Draft, Navigation, Same, Snapshot,
        operations::{OperationKey, OperationPhase},
    },
};
use std::sync::{Arc, OnceLock};

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RequestControl {
    pub id: agent_protocol::ids::RequestId,
    pub phase: Option<OperationPhase>,
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerControls {
    pub navigation: Navigation,
    pub connected: bool,
    pub provider: Option<ProviderKind>,
    pub selected_model: Option<ModelRef>,
    pub quick: ModelQuickControls,
    pub sending: bool,
    pub transcribing: bool,
    pub running_turn: Option<agent_protocol::ids::TurnId>,
    pub interrupt_phase: Option<OperationPhase>,
    pub unavailable: Option<String>,
    pub question: Option<crate::questions::QuestionPrompt>,
    pub request: Option<Request>,
    pub requests: Vec<RequestControl>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ComposerPresentation {
    pub draft: Arc<Draft>,
    pub controls: ComposerControls,
    models: Arc<Vec<Model>>,
    catalog: Option<Arc<agent_protocol::composer::ComposerCatalog>>,
}

impl ComposerPresentation {
    pub(crate) fn equivalent(&self, other: &Self) -> bool {
        self.draft.same(&other.draft)
            && self.controls == other.controls
            && self.models.same(&other.models)
            && self.catalog.same(&other.catalog)
    }
    pub(crate) fn project(snapshot: &Snapshot, previous: Option<&Arc<Self>>) -> Arc<Self> {
        static EMPTY: OnceLock<Arc<Draft>> = OnceLock::new();
        let key = &snapshot.pane.navigation.draft_key;
        let draft = snapshot
            .input
            .drafts
            .get(key)
            .cloned()
            .unwrap_or_else(|| EMPTY.get_or_init(Arc::default).clone());
        let session = snapshot.pane.navigation.thread_id.as_ref();
        let thread = session.and_then(|id| snapshot.sessions.conversations.get(id));
        let running_turn = thread.and_then(|thread| thread.active_turn_id());
        let provider = snapshot.model_provider_for_draft(key.clone());
        let requests = session
            .map(|id| snapshot.docked_requests(id.clone()))
            .unwrap_or_default();
        let controls = ComposerControls {
            navigation: snapshot.pane.navigation.as_ref().clone(),
            connected: snapshot.connection.connected,
            provider,
            selected_model: draft.model.clone().or_else(|| {
                provider.and_then(|provider| snapshot.model_for_provider(key.clone(), provider))
            }),
            quick: snapshot.model_quick_controls(key.clone()),
            sending: snapshot.operation_running(OperationKey::Submission {
                draft_key: key.clone(),
            }),
            transcribing: snapshot.operation_running(OperationKey::Dictation {
                draft_key: key.clone(),
            }),
            interrupt_phase: session
                .zip(running_turn.as_ref())
                .and_then(|(session, turn)| {
                    snapshot.operation_phase(OperationKey::Interrupt {
                        session: session.clone(),
                        turn: turn.clone(),
                    })
                }),
            running_turn,
            unavailable: session
                .and_then(|id| snapshot.input_unavailable_reason(id.clone(), provider)),
            question: session.and_then(|id| snapshot.question_prompt(id.clone())),
            request: requests
                .iter()
                .find(|request| {
                    !matches!(
                        request.request_body,
                        agent_protocol::requests::RequestBody::Question { .. }
                    )
                })
                .cloned(),
            requests: requests
                .iter()
                .map(|request| RequestControl {
                    id: request.id.clone(),
                    phase: snapshot.operation_phase(OperationKey::Request {
                        id: request.id.clone(),
                    }),
                })
                .collect(),
        };
        let catalog = snapshot
            .catalog
            .composer_catalogs
            .get(&snapshot.pane.navigation.cwd)
            .cloned();
        let next = Self {
            draft,
            controls,
            models: snapshot.catalog.models.clone(),
            catalog,
        };
        if let Some(previous) = previous
            && next.equivalent(previous)
        {
            return previous.clone();
        }
        Arc::new(next)
    }
}

impl Snapshot {
    pub(crate) fn submission_unavailable_reason(
        &self,
        session: &crate::session::SessionRef,
        selected: Option<ProviderKind>,
    ) -> Option<String> {
        let thread = self.thread_metadata(session)?;
        crate::session::direct_input_unavailable_reason(thread.can_accept_direct_input).or_else(|| {
            let active = thread.active_provider.unwrap_or(session.provider);
            let busy = agent_protocol::models::task_active(None, thread.status)
                || thread.active_turn_id().is_some()
                || !thread.requests.is_empty()
                || thread.preparation.is_some();
            if selected.is_some_and(|provider| provider != active) && busy {
                Some("AIを切り替えて送信するには、現在の応答・確認・送信の完了を待つか、新しいチャットを開いてください。".into())
            } else {
                None
            }
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn input_unavailable_reason(
        &self,
        session: crate::session::SessionRef,
        selected: Option<ProviderKind>,
    ) -> Option<String> {
        self.submission_unavailable_reason(&session, selected)
            .or_else(|| {
                self.thread_metadata(&session)
                    .and_then(crate::session::input_unavailable_reason)
            })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ComposerPresentation {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn from_snapshot(snapshot: Arc<Snapshot>) -> Arc<Self> {
        Self::project(&snapshot, None)
    }
    pub fn draft(&self) -> Draft {
        self.draft.as_ref().clone()
    }
    pub fn controls(&self) -> ComposerControls {
        self.controls.clone()
    }
    pub fn current_model(&self) -> Option<Model> {
        super::model_settings::current_model(&self.models, self.draft.model.as_ref()).cloned()
    }
    pub fn request_phase(&self, id: agent_protocol::ids::RequestId) -> Option<OperationPhase> {
        self.controls
            .requests
            .iter()
            .find(|request| request.id == id)
            .and_then(|request| request.phase.clone())
    }
    pub fn request_error(&self, id: agent_protocol::ids::RequestId) -> Option<ErrorText> {
        match self.request_phase(id) {
            Some(OperationPhase::Failed { message }) => Some(super::error::error_text(&message)),
            _ => None,
        }
    }
    pub fn composer_suggestions(
        &self,
        text: String,
        cursor: u32,
    ) -> Option<crate::composer::ComposerSuggestions> {
        crate::composer::suggestions(
            self.catalog.as_deref(),
            self.controls.provider,
            text,
            cursor,
        )
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        self.equivalent(&other)
    }
}
