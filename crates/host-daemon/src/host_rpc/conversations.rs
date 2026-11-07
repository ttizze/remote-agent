//! A conversation keeps its public address while native bindings change.
//! Provider bindings and historical source identities belong to the Host. Text
//! lives in Host transcripts. Admitted input is retained until delivery;
//! provider switches retain only the exact delivered handoff.
use crate::providers::Failure;
use agent_protocol::{
    handoff::Context, ids::ClientInputId, models::*, operations::SubmissionReceipt, session::*,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Binding {
    pub session: SessionRef,
    /// Number of Host transcript turns already available to this runtime.
    pub covered: Option<usize>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Segment {
    /// Native import ranges. Once imported, turns and handoff coverage belong
    /// to the Host transcript; switches do not recalculate native offsets.
    pub session: SessionRef,
    pub from: usize,
    pub end: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(super) enum Delivery {
    Ready,
    Pending,
    Injected,
    Inline,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Handoff {
    pub session: SessionRef,
    pub input_id: ClientInputId,
    pub context: Context,
    pub delivery: Delivery,
    pub receipt: Option<SubmissionReceipt>,
    pub submission: SubmissionDelivery,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Conversation {
    pub id: SessionRef,
    pub active: SessionRef,
    pub cwd: String,
    pub name: Option<String>,
    pub preview: Option<String>,
    pub bindings: Vec<Binding>,
    pub segments: Vec<Segment>,
    pub handoffs: Vec<Handoff>,
    pub startup: Option<Startup>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Startup {
    pub preparation: SessionPreparation,
    pub started_at: f64,
}

impl Startup {
    pub(in crate::host_rpc) fn describe(&self, thread: &mut Thread) {
        thread.preparation = Some(self.preparation.clone());
        thread.status = match self.preparation.status {
            PreparationStatus::Preparing | PreparationStatus::Sending => SessionStatus::Preparing,
            PreparationStatus::Unknown => SessionStatus::Unknown,
            PreparationStatus::Failed { .. } | PreparationStatus::Interrupted => {
                SessionStatus::Idle
            }
        };
        thread.updated_at = Some(self.started_at);
    }
}

/// Resource results are committed against the latest row, so an interrupt or
/// rename received during filesystem/provider IO cannot be overwritten.
pub(super) enum StartupChange {
    Worktree,
    Workspace { cwd: String },
    Binding { native: SessionRef },
    Sending,
    Failed { status: PreparationStatus },
    Retry { started_at: f64 },
    Interrupt { input_id: String },
    Finished,
}

pub(super) fn change_startup(
    row: &Conversation,
    change: StartupChange,
) -> Result<Option<Conversation>, Failure> {
    let mut row = row.clone();
    if row.startup.is_none()
        && matches!(
            change,
            StartupChange::Finished | StartupChange::Failed { .. }
        )
    {
        return Ok(None);
    }
    let startup = row
        .startup
        .as_mut()
        .ok_or_else(|| Failure::new("preparation_finished", "準備は完了しています。"))?;
    match change {
        StartupChange::Worktree => {
            if startup.preparation.status != PreparationStatus::Preparing {
                return Err(Failure::new(
                    "preparation_interrupted",
                    "準備を停止しました。",
                ));
            }
            startup.preparation.stage = PreparationStage::Worktree;
        }
        StartupChange::Workspace { cwd } => {
            row.cwd = cwd;
            startup.preparation.stage = PreparationStage::Provider;
        }
        StartupChange::Binding { native } => {
            row.bindings.push(Binding {
                session: native.clone(),
                covered: None,
            });
            row.segments.push(Segment {
                session: native.clone(),
                from: 0,
                end: None,
            });
            row.active = native;
        }
        StartupChange::Sending => {
            if startup.preparation.status != PreparationStatus::Preparing {
                return Err(Failure::new(
                    "preparation_interrupted",
                    "準備を停止しました。",
                ));
            }
            startup.preparation.status = PreparationStatus::Sending;
        }
        StartupChange::Failed { status } => {
            if !startup.preparation.status.is_active() {
                return Ok(None);
            }
            startup.preparation.status = status;
        }
        StartupChange::Retry { started_at } => match startup.preparation.status {
            PreparationStatus::Failed { .. } | PreparationStatus::Interrupted => {
                startup.preparation.status = PreparationStatus::Preparing;
                startup.started_at = started_at;
            }
            PreparationStatus::Preparing | PreparationStatus::Sending => return Ok(None),
            PreparationStatus::Unknown => {
                return Err(Failure::unknown(
                    "submission_delivery_unknown",
                    "送信結果が不明なため、自動で再送できません。",
                ));
            }
        },
        StartupChange::Interrupt { input_id } => {
            if startup
                .preparation
                .submission
                .client_user_message_id
                .as_str()
                != input_id
                || startup.preparation.status != PreparationStatus::Preparing
            {
                return Err(Failure::new(
                    "invalid_turn",
                    "準備の停止を受け付けられません。",
                ));
            }
            startup.preparation.status = PreparationStatus::Interrupted;
        }
        StartupChange::Finished => row.startup = None,
    }
    Ok(Some(row))
}

impl Conversation {
    /// Preserve the first native binding's IDs, independently of the public
    /// conversation address. Other provider sessions,
    /// including replacement sessions of the same AI, have separate namespaces.
    pub fn identity(&self, session: &SessionRef, id: &str) -> String {
        if self
            .bindings
            .first()
            .is_some_and(|binding| &binding.session == session)
        {
            id.into()
        } else {
            qualified(session, id)
        }
    }

    pub fn source(&self, id: &str) -> Result<(SessionRef, String), Failure> {
        if id.starts_with("bex:") {
            source(id)
        } else {
            Ok((
                self.bindings
                    .first()
                    .map_or_else(|| self.id.clone(), |binding| binding.session.clone()),
                id.into(),
            ))
        }
    }
    pub fn new(thread: &Thread) -> Result<Self, Failure> {
        let id = thread
            .id
            .clone()
            .ok_or_else(|| Failure::new("invalid_thread", "session ID is missing"))?;
        Ok(Self {
            active: id.clone(),
            cwd: thread
                .cwd
                .clone()
                .ok_or_else(|| Failure::new("invalid_thread", "working directory is missing"))?,
            name: thread.name.clone(),
            preview: thread
                .preview
                .as_deref()
                .map(agent_protocol::models::compact_title),
            bindings: vec![Binding {
                session: id.clone(),
                covered: None,
            }],
            segments: vec![Segment {
                session: id.clone(),
                from: 0,
                end: None,
            }],
            id,
            handoffs: Vec::new(),
            startup: None,
        })
    }

    pub fn binding(&self, provider: ProviderKind) -> Option<&Binding> {
        self.bindings
            .iter()
            .rev()
            .find(|binding| binding.session.provider == provider)
    }

    pub fn sources(&self) -> impl Iterator<Item = &SessionRef> {
        self.segments
            .iter()
            .map(|segment| &segment.session)
            .chain(self.bindings.iter().map(|binding| &binding.session))
    }

    pub fn cover(&mut self, count: usize) {
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| binding.session == self.active)
        {
            binding.covered = Some(count);
        }
    }

    pub fn activate(&mut self, session: SessionRef, count: usize) {
        self.active = session;
        self.cover(count);
    }

    pub fn describe(&self, thread: &mut Thread) {
        thread.id = Some(self.id.clone());
        thread.active_provider = Some(self.active.provider);
        thread.cwd = Some(self.cwd.clone());
        if let Some(name) = &self.name {
            thread.name = Some(name.clone());
        }
        if let Some(preview) = &self.preview {
            thread.preview = Some(preview.clone());
        }
        if let Some(startup) = &self.startup {
            startup.describe(thread);
        }
    }

    pub fn strip_context(&self, session: &SessionRef, item: &mut Item) {
        let body = match &mut item.body {
            ItemContent::Inline { body } => body.as_mut(),
            ItemContent::Deferred { summary } => summary.as_mut(),
        };
        let ItemBody::UserMessage { text, content } = body else {
            return;
        };
        for handoff in self.handoffs.iter().rev().filter(|handoff| {
            &handoff.session == session
                && handoff.delivery != Delivery::Injected
                && (item.client_input_id.as_ref() == Some(&handoff.input_id)
                    || item.id.as_str() == handoff.input_id.as_str())
        }) {
            // A refused submission may have been retried with the same input
            // identity and newer context. Strip only the exact delivered packet.
            let prefix = inline_context(&handoff.context);
            if let Some(rest) = text
                .as_deref()
                .and_then(|value| value.strip_prefix(&prefix))
            {
                *text = (!rest.is_empty()).then(|| rest.to_owned());
            }
            if let Some(MessagePart::Text { text }) = content.first_mut() {
                if let Some(rest) = text.strip_prefix(&prefix) {
                    *text = rest.into();
                }
                if text.is_empty() {
                    content.remove(0);
                }
            }
        }
    }

    pub fn turn(&self, session: &SessionRef, mut turn: Turn) -> Turn {
        turn.provider = Some(session.provider);
        turn.id = self.identity(session, &turn.id).into();
        if let Some(items) = &mut turn.items {
            for item in items {
                self.item(session, Arc::make_mut(item));
            }
        }
        turn
    }

    pub fn item(&self, session: &SessionRef, item: &mut Item) {
        self.strip_context(session, item);
        item.id = self.identity(session, &item.id).into();
        let body = match &mut item.body {
            ItemContent::Inline { body } => body.as_mut(),
            ItemContent::Deferred { summary } => summary.as_mut(),
        };
        if let ItemBody::Subagent {
            sender: Some(sender),
            ..
        } = body
            && self
                .bindings
                .iter()
                .any(|binding| binding.session == *sender)
        {
            *sender = self.id.clone();
        }
    }
}

pub(super) fn inline_context(context: &Context) -> String {
    format!("{}\n\nUser message:\n", context.render())
}

pub(super) fn qualified(session: &SessionRef, id: &str) -> String {
    format!(
        "bex:{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(session, id)).expect("identity serializes"))
    )
}

pub(super) fn source(id: &str) -> Result<(SessionRef, String), Failure> {
    let value = id
        .strip_prefix("bex:")
        .and_then(|id| URL_SAFE_NO_PAD.decode(id).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    value.ok_or_else(|| {
        Failure::new(
            "invalid_history_reference",
            "The item does not belong to a provider conversation.",
        )
    })
}

pub(super) fn validate(rows: &[Conversation]) -> Result<(), String> {
    let mut identities = std::collections::HashSet::new();
    let public: std::collections::HashSet<_> = rows.iter().map(|row| &row.id).collect();
    if public.len() != rows.len() {
        return Err("Duplicate public conversation".into());
    }
    for row in rows {
        row.id.validate().map_err(str::to_owned)?;
        let unbound = row.bindings.is_empty()
            && row.segments.is_empty()
            && row.active == row.id
            && row.startup.is_some();
        if row.startup.as_ref().is_some_and(|startup| {
            let input = &startup.preparation.submission;
            input.thread_id != row.id
                || input.client_user_message_id.is_empty()
                || input.client_user_message_id.len() > 256
                || (startup.preparation.status == PreparationStatus::Sending
                    && row.bindings.is_empty())
        }) {
            return Err("Invalid admitted task input or native binding".into());
        }
        if (!unbound
            && (!row
                .bindings
                .iter()
                .any(|binding| binding.session == row.active)))
            || row.cwd.trim().is_empty()
        {
            return Err("Invalid conversation identity or workspace".into());
        }
        for binding in &row.bindings {
            binding.session.validate().map_err(str::to_owned)?;
            if !identities.insert(binding.session.clone())
                || (binding.session != row.id && public.contains(&binding.session))
            {
                return Err("Duplicate native owner or invalid history coverage".into());
            }
        }
        for segment in &row.segments {
            segment.session.validate().map_err(str::to_owned)?;
            if segment.end.is_some_and(|end| end < segment.from) {
                return Err("Invalid native import range".into());
            }
        }
    }
    Ok(())
}

pub(super) fn namespace_change(
    row: &Conversation,
    session: &SessionRef,
    change: &mut SessionChange,
    supports_fork: bool,
) {
    match change {
        SessionChange::Turn { turn, .. } => {
            *turn = row.turn(session, turn.clone());
            turn.supports_fork = Some(supports_fork);
        }
        SessionChange::Item { turn_id, item } => {
            *turn_id = row.identity(session, turn_id).into();
            row.item(session, Arc::make_mut(item));
        }
        SessionChange::TurnItems { turn_id, items } => {
            *turn_id = row.identity(session, turn_id).into();
            for item in items {
                row.item(session, Arc::make_mut(item));
            }
        }
        SessionChange::Text {
            turn_id, item_id, ..
        }
        | SessionChange::RemoveItem { turn_id, item_id }
        | SessionChange::ReasoningPart {
            turn_id, item_id, ..
        } => {
            *turn_id = row.identity(session, turn_id).into();
            *item_id = row.identity(session, item_id).into();
        }
        SessionChange::Error { turn_id, .. } => *turn_id = row.identity(session, turn_id).into(),
        SessionChange::Request { request } => namespace_request(row, session, request),
        SessionChange::Submission {
            delivery:
                SubmissionDelivery::Accepted {
                    turn_id: Some(turn_id),
                },
            ..
        } => *turn_id = row.identity(session, turn_id).into(),
        _ => {}
    }
}

pub(super) fn namespace_request(
    row: &Conversation,
    session: &SessionRef,
    request: &mut agent_protocol::requests::Request,
) {
    if let agent_protocol::requests::RequestTarget::Turn { turn_id, item_id } = &mut request.target
    {
        *turn_id = row.identity(session, turn_id).into();
        if let Some(item_id) = item_id {
            *item_id = row.identity(session, item_id).into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_rpc::storage::Database;
    use crate::providers::AgentChange;

    fn conversation() -> Conversation {
        Conversation::new(&Thread {
            id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "original".into(),
            }),
            cwd: Some("/workspace".into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn restart_retains_admitted_input_and_distinguishes_preparation_from_uncertain_delivery() {
        for stage in [
            PreparationStage::Workspace,
            PreparationStage::Worktree,
            PreparationStage::Provider,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("conversations.sqlite");
            let store = Database::open(&path);
            for status in [PreparationStatus::Preparing, PreparationStatus::Sending] {
                let mut row = conversation();
                let submission = agent_protocol::operations::Submission {
                    thread_id: row.id.clone(),
                    client_user_message_id: "input".into(),
                    input: vec![agent_protocol::operations::Input::Text {
                        text: "retained initial input".into(),
                    }],
                    model: None,
                    effort: Some("high".into()),
                    service_tier: Some("fast".into()),
                };
                row.startup = Some(Startup {
                    preparation: SessionPreparation {
                        submission: submission.clone(),
                        status: status.clone(),
                        stage,
                    },
                    started_at: 1.,
                });
                store.save(row.clone()).unwrap();
                let restored = Database::open(&path).get(&row.id).unwrap().unwrap();
                let startup = restored.startup.unwrap();
                assert_eq!(startup.preparation.submission, submission);
                assert_eq!(startup.preparation.stage, stage);
                assert_eq!(restored.active, row.active);
                match status {
                    PreparationStatus::Preparing => assert!(matches!(
                        startup.preparation.status,
                        PreparationStatus::Failed { .. }
                    )),
                    PreparationStatus::Sending => {
                        assert_eq!(startup.preparation.status, PreparationStatus::Unknown)
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    #[test]
    fn persistence_recovers_preparation_and_does_not_commit_failed_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversations.sqlite");
        let store = Database::open(&path);
        let mut row = conversation();
        row.cover(3);
        store.save(row.clone()).unwrap();
        let recovered = Database::open(&path).get(&row.id).unwrap().unwrap();
        assert_eq!(recovered.segments.len(), 1);
        assert_eq!(recovered.bindings[0].covered, Some(3));
        store.with(|connection| connection.execute_batch("CREATE TRIGGER reject_title BEFORE UPDATE ON threads WHEN NEW.host_title = 'uncommitted' BEGIN SELECT RAISE(ABORT, 'test failure'); END;").map_err(crate::host_rpc::storage::failure)).unwrap();
        row.name = Some("uncommitted".into());
        assert!(store.save(row.clone()).is_err());
        assert_eq!(store.get(&row.id).unwrap().unwrap().name, None);
        drop(store);
        std::fs::write(&path, "broken").unwrap();
        assert!(Database::open(&path).get(&row.id).is_err());
    }

    #[test]
    fn released_provider_callbacks_cannot_change_the_active_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let store = Database::open(&directory.path().join("conversations.sqlite"));
        let mut row = conversation();
        let target = SessionRef {
            provider: ProviderKind::Claude,
            id: "target".into(),
        };
        row.cover(1);
        row.bindings.push(Binding {
            session: target.clone(),
            covered: None,
        });
        row.activate(target.clone(), 0);
        store.save(row.clone()).unwrap();
        let change = || SessionChange::Turn {
            turn: Turn {
                id: "same-id".into(),
                ..Default::default()
            },
            completed: true,
        };
        assert!(
            store
                .route(
                    AgentChange::Session {
                        session: row.id.clone(),
                        change: change()
                    },
                    true
                )
                .unwrap()
                .is_none()
        );
        let Some(AgentChange::Session {
            session,
            change: SessionChange::Turn { turn, .. },
        }) = store
            .route(
                AgentChange::Session {
                    session: target.clone(),
                    change: change(),
                },
                false,
            )
            .unwrap()
        else {
            panic!("active event was not routed")
        };
        assert_eq!(session, row.id);
        assert_eq!(row.source(&turn.id).unwrap(), (target, "same-id".into()));
        assert_eq!(turn.supports_fork, Some(false));
    }

    #[test]
    fn inline_cleanup_matches_the_exact_retry_packet_and_preserves_user_whitespace() {
        let mut row = conversation();
        let packet = |coverage: &str| Context {
            messages: vec![],
            coverage: coverage.into(),
            omitted: 0,
        };
        for context in [packet("old history"), packet("new history")] {
            row.handoffs.push(Handoff {
                session: row.id.clone(),
                input_id: "same-input".into(),
                context,
                delivery: Delivery::Inline,
                receipt: None,
                submission: SubmissionDelivery::Rejected,
            });
        }
        let mut item = Item::new(
            "user".into(),
            agent_protocol::execution::ItemStatus::Completed,
            ItemBody::UserMessage {
                text: Some(format!(
                    "{}\n\nkeep spacing",
                    inline_context(&row.handoffs[0].context)
                )),
                content: vec![],
            },
        );
        item.client_input_id = Some("same-input".into());
        row.strip_context(&row.id, &mut item);
        assert!(
            matches!(item.body(), ItemBody::UserMessage { text: Some(text), .. } if text == "\n\nkeep spacing")
        );
        let prefix = inline_context(&row.handoffs[0].context);
        let mut separate = Item::new(
            "same-input".into(),
            agent_protocol::execution::ItemStatus::Completed,
            ItemBody::UserMessage {
                text: Some(prefix.clone()),
                content: vec![
                    MessagePart::Text { text: prefix },
                    MessagePart::Text {
                        text: "\n\nkeep spacing".into(),
                    },
                ],
            },
        );
        row.strip_context(&row.id, &mut separate);
        assert!(
            matches!(separate.body(), ItemBody::UserMessage { text: None, content }
            if content == &[MessagePart::Text { text: "\n\nkeep spacing".into() }])
        );
    }
}
