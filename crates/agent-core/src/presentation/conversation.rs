//! Immutable conversation projection shared by desktop, Swift and Kotlin.
use super::{ItemMetadata, Role, body, item_presentation, project_items, source_order};
use crate::{
    models,
    state::{PendingSubmission, Snapshot},
};
use agent_protocol::requests::Request as WireRequest;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
mod images;

impl Snapshot {
    /// Display pending input before a new conversation has a server ID.
    pub fn conversation_thread(&self) -> Option<Arc<models::Thread>> {
        if let Some(source) = self
            .pane
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.sessions.conversations.get(id))
        {
            return Some(source.clone());
        }
        self.sessions
            .pending_submissions
            .values()
            .any(|pending| pending.draft_key == self.pane.navigation.draft_key)
            .then(|| {
                Arc::new(models::Thread {
                    ..Default::default()
                })
            })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedConversation {
    pub source: Arc<models::Thread>,
    pending: PendingItems,
    pub turns: Vec<Arc<RenderedTurn>>,
    pub queued: Vec<Arc<RenderedItem>>,
    pub session_rows: Vec<ConversationRow>,
    pub source_paths: Arc<Vec<String>>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedTurn {
    pub source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    pub rows: Vec<ConversationRow>,
}
/// Native clients cache this layout per unchanged turn; expansion only filters activity rows.
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationRow {
    pub id: String,
    pub content: ConversationRowContent,
    pub timestamp_ms: Option<u64>,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationRowContent {
    Preparation {
        preparation: PreparationPresentation,
    },
    ProviderHandoff {
        text: String,
    },
    User {
        item: Arc<RenderedItem>,
    },
    ActivityHeader {
        activity: ActivityPresentation,
    },
    Activity {
        item: Arc<RenderedItem>,
        turn_id: agent_protocol::ids::TurnId,
    },
    PendingRequest {
        request: Box<Request>,
    },
    Error {
        error: TurnErrorPresentation,
    },
    Response {
        item: Arc<RenderedItem>,
        show_actions: bool,
        fork_turn_id: Option<agent_protocol::ids::TurnId>,
    },
    InProgress {
        turn_id: agent_protocol::ids::TurnId,
    },
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreparationPresentation {
    pub text: String,
    /// Rendered like a running turn's working row, as T3 Code shows startup.
    pub in_progress: bool,
    pub retry: Option<agent_protocol::operations::RetryPreparation>,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityPresentation {
    pub id: String,
    pub status: String,
    pub activity_summary: String,
    pub activity_can_collapse: bool,
    pub is_in_progress: bool,
    pub load_items: Option<crate::state::operations::LoadTurnItems>,
}
/// Work starts collapsed. A manual choice belongs to the current status;
/// a transition closes it again, including after deferred-detail hydration.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn activity_is_expanded(status: &str, expanded_status: Option<String>) -> bool {
    expanded_status.as_deref() == Some(status)
}

/// Fill the viewport on opening and continue at the oldest visible boundary.
/// Wait for latest-message positioning before paging a scrollable initial page.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn should_load_history(
    has_more: bool,
    loading: bool,
    oldest_visible: bool,
    latest_visible: bool,
    following_latest: bool,
) -> bool {
    has_more && !loading && oldest_visible && (!following_latest || latest_visible)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedTurn {
    pub fn conversation_rows(&self) -> Vec<ConversationRow> {
        self.rows.clone()
    }
}

#[derive(Clone, PartialEq)]
pub struct WorkProgress {
    started_at: Option<f64>,
    action: Option<String>,
}

impl WorkProgress {
    pub fn label(&self, now_seconds: f64) -> String {
        let elapsed = self
            .started_at
            .map(|started| format!("{}秒 ", (now_seconds - started).max(0.) as u64))
            .unwrap_or_default();
        match self.action.as_deref().filter(|action| !action.is_empty()) {
            Some(action) => format!("{elapsed}作業中 · {action}"),
            None => format!("{elapsed}作業中…"),
        }
    }
}

impl RenderedTurn {
    pub fn work_progress(&self, include_action: bool) -> WorkProgress {
        let action = if include_action {
            self.rows.iter().rev().find_map(|row| match &row.content {
                ConversationRowContent::Activity { item, .. } => item.data.title.as_deref(),
                _ => None,
            })
        } else {
            None
        };
        WorkProgress {
            started_at: self.source.started_at.or_else(|| {
                self.source
                    .started_at_ms
                    .map(|milliseconds| milliseconds as f64 / 1000.)
            }),
            action: action.map(str::to_owned),
        }
    }
    fn items(&self) -> impl Iterator<Item = &Arc<RenderedItem>> {
        self.rows.iter().filter_map(|row| match &row.content {
            ConversationRowContent::User { item }
            | ConversationRowContent::Activity { item, .. }
            | ConversationRowContent::Response { item, .. } => Some(item),
            _ => None,
        })
    }
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TurnErrorPresentation {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub is_reconnecting: bool,
    pub is_usage_limit: bool,
}

pub enum ItemSource {
    Native(Arc<models::Item>),
    Pending(String, Arc<PendingSubmission>),
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedItem {
    pub source: ItemSource,
    pub data: ItemPresentation,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedItem {
    pub fn detail_blocks(&self) -> Vec<super::markdown::MarkdownBlock> {
        match &self.source {
            ItemSource::Native(item) => body::detail_blocks(item),
            ItemSource::Pending(..) => {
                super::markdown::markdown_blocks(self.data.body.clone().unwrap_or_default())
            }
        }
    }
    pub fn expanded_body(&self) -> String {
        match &self.source {
            ItemSource::Native(item) => body::expanded_body(item),
            ItemSource::Pending(..) => self.data.body.clone().expect("pending input has text"),
        }
    }
}
impl RenderedItem {
    fn key(&self) -> (Option<&str>, *const ()) {
        match &self.source {
            ItemSource::Native(item) => (None, Arc::as_ptr(item).cast()),
            ItemSource::Pending(id, pending) => (Some(id), Arc::as_ptr(pending).cast()),
        }
    }
    fn native(
        item: &Arc<models::Item>,
        provider: Option<crate::session::ProviderKind>,
        deferred: bool,
        timestamp_ms: Option<u64>,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = previous
            && previous.data.deferred == deferred
            && previous.data.timestamp_ms == timestamp_ms
        {
            return previous.clone();
        }
        let presentation = item_presentation(item, provider);
        let body = body::item_body(item);
        let image_placeholder = presentation.kind == "imageGeneration"
            && item.status == models::ItemStatus::Running
            && body.images.is_empty();
        Arc::new(Self {
            source: ItemSource::Native(item.clone()),
            data: ItemPresentation {
                id: item
                    .client_input_id
                    .as_deref()
                    .unwrap_or(item.id.as_str())
                    .to_owned(),
                native_id: Some(item.id.clone()),
                timestamp_ms,
                kind: presentation.kind.into(),
                title: presentation.title,
                collapsible: presentation.collapsible,
                body: body.text,
                image_sources: body.images,
                image_placeholder,
                deferred,
            },
        })
    }
    fn pending(
        id: &str,
        pending: &Arc<PendingSubmission>,
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = previous {
            return previous.clone();
        }
        let body = body::draft_body(&pending.draft);
        Arc::new(Self {
            source: ItemSource::Pending(id.into(), pending.clone()),
            data: ItemPresentation {
                id: id.into(),
                native_id: None,
                timestamp_ms: pending.submitted_at_ms,
                kind: "user".into(),
                title: pending
                    .delivery_unknown
                    .then(|| "送信結果不明（自動再送しません）".into()),
                collapsible: false,
                body: body.text,
                image_sources: body.images,
                image_placeholder: false,
                deferred: false,
            },
        })
    }
}

/// Pass the previous projection to retain native render identities across deltas.
pub fn project_conversation(
    draft_key: &crate::state::DraftKey,
    pending_submissions: &std::collections::BTreeMap<
        agent_protocol::ids::ClientInputId,
        Arc<PendingSubmission>,
    >,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let draft_key = source
        .id
        .as_ref()
        .map(crate::state::DraftKey::from)
        .unwrap_or_else(|| draft_key.clone());
    let preparation = source.preparation.as_ref();
    let mut pending: PendingItems = pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
        .filter(|(id, _)| {
            preparation
                .is_none_or(|preparation| &preparation.submission.client_user_message_id != *id)
        })
        .map(|(id, pending)| (id.clone(), pending.clone()))
        .collect();
    pending.sort_by_key(|(_, pending)| pending.sequence);
    let requests = &source.requests;
    if let Some(previous) = previous
        && Arc::ptr_eq(&source, &previous.source)
        && preparation.is_none_or(|preparation| {
            let id = &preparation.submission.client_user_message_id;
            previous
                .queued
                .iter()
                .find(|item| item.data.id == id.as_str())
                .is_none_or(|item| {
                    item.data.timestamp_ms
                        == pending_submissions
                            .get(id)
                            .and_then(|pending| pending.submitted_at_ms)
                })
        })
        && pending
            .iter()
            .map(|(id, pending)| (id, Arc::as_ptr(pending)))
            .eq(previous
                .pending
                .iter()
                .map(|(id, pending)| (id, Arc::as_ptr(pending))))
    {
        return previous.clone();
    }
    let previous = previous.as_ref().filter(|old| source.id == old.source.id);
    let supports_fork = source.capabilities.unwrap_or_default().fork;
    let previous_supports_fork =
        previous.is_some_and(|old| old.source.capabilities.unwrap_or_default().fork);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.turns)
        .map(|turn| (turn.source.id.as_str(), turn))
        .collect();
    let native = source.turns.as_deref().unwrap_or_default();
    let preparation_echoed = preparation.is_some_and(|preparation| {
        native
            .iter()
            .flat_map(|turn| turn.items.iter().flatten())
            .any(|item| {
                item.client_input_id.as_ref()
                    == Some(&preparation.submission.client_user_message_id)
            })
    });
    let positions: HashMap<_, _> = if pending.is_empty() && requests.is_empty() {
        HashMap::new()
    } else {
        native
            .iter()
            .enumerate()
            .map(|(index, turn)| (turn.id.as_str(), index))
            .collect()
    };
    // A submission made before any history belongs before the first turn once
    // it arrives. An acknowledged queue entry still waits for its assigned turn.
    let mut pending_by_turn: HashMap<_, Vec<_>> = HashMap::new();
    for entry @ (_, submission) in &pending {
        let position = match submission.turn_id.as_deref() {
            Some(id) => positions.get(id).copied(),
            None if !submission.accepted && !native.is_empty() => Some(0),
            None => None,
        };
        pending_by_turn.entry(position).or_default().push(entry);
    }
    let mut requests_by_turn: HashMap<_, Vec<_>> = HashMap::new();
    let mut session_requests = Vec::new();
    for request in requests.values() {
        if let agent_protocol::requests::RequestTarget::Turn { turn_id, .. } = &request.target
            && positions.contains_key(turn_id.as_str())
        {
            requests_by_turn
                .entry(turn_id.as_str())
                .or_default()
                .push(request);
        } else {
            session_requests.push(request);
        }
    }
    let turns = native
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let handoff = index
                .checked_sub(1)
                .and_then(|previous| native[previous].provider)
                .zip(turn.provider)
                .filter(|(from, to)| from != to)
                .map(|(from, to)| format!("{from:?} → {to:?} に切り替えました"));
            let pending = pending_by_turn
                .get(&Some(index))
                .into_iter()
                .flatten()
                .copied();
            let requests = requests_by_turn
                .get(turn.id.as_str())
                .into_iter()
                .flatten()
                .copied();
            let old = cached.get(turn.id.as_str()).copied();
            if let Some(old) = old
                && Arc::ptr_eq(turn, &old.source)
                && (turn.supports_fork.is_some() || supports_fork == previous_supports_fork)
                && old.rows.iter().find_map(|row| match &row.content {
                    ConversationRowContent::ProviderHandoff { text } => Some(text),
                    _ => None,
                }) == handoff.as_ref()
                && pending
                    .clone()
                    .map(|(id, p)| (id, Arc::as_ptr(p)))
                    .eq(old.pending.iter().map(|(id, p)| (id, Arc::as_ptr(p))))
                && requests
                    .clone()
                    .map(Arc::as_ptr)
                    .eq(old.requests.iter().map(Arc::as_ptr))
            {
                return old.clone();
            }
            render_turn(
                source.id.as_ref(),
                supports_fork,
                handoff,
                turn.clone(),
                pending.cloned().collect(),
                requests.cloned().collect(),
                old,
            )
        })
        .collect();
    let queued: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.queued)
        .map(|item| (item.key(), item))
        .collect();
    let mut queued: Vec<_> = pending_by_turn
        .get(&None)
        .into_iter()
        .flatten()
        .copied()
        .map(|(id, p)| {
            RenderedItem::pending(
                id,
                p,
                queued
                    .get(&(Some(id.as_str()), Arc::as_ptr(p).cast()))
                    .copied(),
            )
        })
        .collect();
    if let Some(preparation) = preparation {
        let id = &preparation.submission.client_user_message_id;
        if !preparation_echoed {
            let item = Arc::new(models::Item::new(
                id.to_string().into(),
                models::ItemStatus::Completed,
                models::ItemBody::UserMessage {
                    text: None,
                    content: agent_protocol::operations::Input::message_parts(
                        &preparation.submission.input,
                    ),
                },
            ));
            queued.insert(
                0,
                RenderedItem::native(
                    &item,
                    source.active_provider,
                    false,
                    pending_submissions
                        .get(id)
                        .and_then(|pending| pending.submitted_at_ms),
                    previous
                        .into_iter()
                        .flat_map(|old| &old.queued)
                        .find(|old| {
                            matches!(&old.source, ItemSource::Native(_))
                                && old.data.id == id.as_str()
                        }),
                ),
            );
        }
    }
    let mut session_rows: Vec<_> = session_requests
        .into_iter()
        .map(|source| ConversationRow {
            id: format!("request:{}", source.id),
            timestamp_ms: None,
            content: ConversationRowContent::PendingRequest {
                request: Box::new(request(source)),
            },
        })
        .collect();
    if let Some(preparation) = preparation.filter(|preparation| match preparation.status {
        agent_protocol::session::PreparationStatus::Unknown => !preparation_echoed,
        // A provider turn that already runs shows its own working row.
        agent_protocol::session::PreparationStatus::Preparing
        | agent_protocol::session::PreparationStatus::Sending => !native
            .iter()
            .any(|turn| turn.status == models::TurnStatus::Running),
        _ => true,
    }) {
        session_rows.push(ConversationRow {
            id: "preparation".into(),
            timestamp_ms: None,
            content: ConversationRowContent::Preparation {
                preparation: preparation_presentation(preparation),
            },
        });
    }
    let paths: std::collections::BTreeSet<_> = source
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter_map(|item| match item.body() {
            agent_protocol::items::ItemBody::UserMessage { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            agent_protocol::items::MessagePart::Image { source }
                if !source.starts_with("data:") =>
            {
                Some(source.to_owned())
            }
            agent_protocol::items::MessagePart::Attachment { path, .. }
            | agent_protocol::items::MessagePart::Invocation { path, .. } => Some(path.to_owned()),
            _ => None,
        })
        .collect();
    let source_paths: Vec<_> = paths.into_iter().collect();
    let source_paths = previous
        .filter(|old| old.source_paths.as_ref() == &source_paths)
        .map_or_else(|| Arc::new(source_paths), |old| old.source_paths.clone());
    Arc::new(RenderedConversation {
        source,
        pending,
        turns,
        queued,
        session_rows,
        source_paths,
    })
}

fn preparation_presentation(
    preparation: &agent_protocol::session::SessionPreparation,
) -> PreparationPresentation {
    use agent_protocol::{
        operations::RetryPreparation,
        session::{PreparationStage, PreparationStatus},
    };
    // Startup reads as ordinary work; the composer's stop control interrupts it.
    let (text, in_progress, retry) = match &preparation.status {
        PreparationStatus::Preparing | PreparationStatus::Sending => {
            let text = match preparation.stage {
                PreparationStage::Worktree => "ワークツリーを準備中…",
                PreparationStage::Workspace | PreparationStage::Provider => "開始中…",
            };
            (text.into(), true, false)
        }
        PreparationStatus::Failed { error } => (
            format!("準備に失敗しました: {}", error.message),
            false,
            true,
        ),
        PreparationStatus::Interrupted => ("準備を停止しました。".into(), false, true),
        PreparationStatus::Unknown => (
            "送信結果を確認できません。二重実行を避けるため再送していません。".into(),
            false,
            false,
        ),
    };
    PreparationPresentation {
        text,
        in_progress,
        retry: retry.then(|| RetryPreparation {
            thread_id: preparation.submission.thread_id.clone(),
        }),
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn user_file_sources_are_deduplicated_and_reused_when_other_content_changes() {
        use agent_protocol::{
            execution::ItemStatus,
            items::{ItemBody, MessagePart},
        };
        let item = Arc::new(models::Item::new(
            "input".into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: vec![
                    MessagePart::Attachment {
                        name: "a".into(),
                        path: "/project/a".into(),
                    },
                    MessagePart::Image {
                        source: "/project/a".into(),
                    },
                    MessagePart::Image {
                        source: "data:image/png;base64,a".into(),
                    },
                    MessagePart::Invocation {
                        name: "skill".into(),
                        path: "/project/skill".into(),
                    },
                ],
            },
        ));
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                items: Some(vec![item]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let graph = project_conversation(
            &Default::default(),
            &Default::default(),
            source.clone(),
            &None,
        );
        assert_eq!(
            graph.source_paths.as_slice(),
            ["/project/a", "/project/skill"]
        );
        let mut changed = source.as_ref().clone();
        changed.name = Some("A new title".into());
        let next = project_conversation(
            &Default::default(),
            &Default::default(),
            Arc::new(changed),
            &Some(graph.clone()),
        );
        assert!(Arc::ptr_eq(&graph.source_paths, &next.source_paths));
    }
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use agent_protocol::{
        operations::{Input, Submission},
        session::{
            PreparationStage, PreparationStatus, ProviderKind, SessionPreparation, SessionRef,
        },
    };

    #[test]
    fn startup_labels_follow_host_stages_in_both_languages() {
        use crate::localization::{UiLanguage, localized_ui_text};
        let id = SessionRef::new(ProviderKind::Codex, "startup".into()).unwrap();
        let mut preparation = SessionPreparation {
            submission: Submission {
                thread_id: id,
                client_user_message_id: "input".into(),
                input: Vec::new(),
                model: None,
                effort: None,
                service_tier: None,
            },
            status: PreparationStatus::Preparing,
            stage: PreparationStage::Workspace,
        };
        for (stage, japanese, english) in [
            (PreparationStage::Workspace, "開始中…", "Starting…"),
            (
                PreparationStage::Worktree,
                "ワークツリーを準備中…",
                "Setting up worktree…",
            ),
            (PreparationStage::Provider, "開始中…", "Starting…"),
        ] {
            preparation.stage = stage;
            let presentation = preparation_presentation(&preparation);
            assert!(presentation.in_progress);
            assert!(presentation.retry.is_none());
            assert_eq!(
                localized_ui_text(UiLanguage::Japanese, presentation.text.clone()),
                japanese
            );
            assert_eq!(
                localized_ui_text(UiLanguage::English, presentation.text),
                english
            );
        }
        preparation.status = PreparationStatus::Sending;
        assert_eq!(preparation_presentation(&preparation).text, "開始中…");
    }

    #[test]
    fn admitted_input_is_visible_once_through_failure_retry_and_native_echo() {
        let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
        let mut thread = models::Thread {
            id: Some(id.clone()),
            preparation: Some(SessionPreparation {
                submission: Submission {
                    thread_id: id.clone(),
                    client_user_message_id: "input".into(),
                    input: vec![Input::Text {
                        text: "accepted text".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                },
                status: PreparationStatus::Preparing,
                stage: PreparationStage::Workspace,
            }),
            ..Default::default()
        };
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "input".into(),
            Arc::new(PendingSubmission {
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: id.clone().into(),
                draft: Arc::new(crate::state::Draft {
                    text: "accepted text".into(),
                    ..Default::default()
                }),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
            }),
        );
        let prepared = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &None,
        );
        let mut timed = snapshot.clone();
        Arc::make_mut(
            Arc::make_mut(&mut timed.sessions.pending_submissions)
                .get_mut(&agent_protocol::ids::ClientInputId::from("input"))
                .unwrap(),
        )
        .submitted_at_ms = Some(13_000);
        let retimed = project_conversation(
            &timed.pane.navigation.draft_key,
            &timed.sessions.pending_submissions,
            prepared.source.clone(),
            &Some(prepared.clone()),
        );
        assert_eq!(retimed.queued[0].data.timestamp_ms, Some(13_000));
        assert_eq!(prepared.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(prepared.queued.len(), 1);
        assert_eq!(prepared.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(
            prepared.queued[0].data.body.as_deref(),
            Some("accepted text")
        );
        let ConversationRowContent::Preparation { preparation } = &prepared.session_rows[0].content
        else {
            panic!("missing preparation")
        };
        assert!(preparation.in_progress && preparation.retry.is_none());
        thread.preparation.as_mut().unwrap().status = PreparationStatus::Sending;
        thread.turns = Some(vec![Arc::new(models::Turn {
            id: "starting".into(),
            status: models::TurnStatus::Running,
            ..Default::default()
        })]);
        let starting = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &Some(prepared.clone()),
        );
        assert_eq!(starting.queued.len(), 1);
        // The running provider turn's own working row replaces the startup row.
        assert!(starting.session_rows.is_empty());
        assert!(
            starting.turns[0]
                .rows
                .iter()
                .any(|row| matches!(row.content, ConversationRowContent::InProgress { .. }))
        );
        assert!(
            !starting.turns[0]
                .rows
                .iter()
                .any(|row| matches!(row.content, ConversationRowContent::User { .. }))
        );
        thread.turns = None;
        thread.preparation.as_mut().unwrap().status = PreparationStatus::Failed {
            error: Default::default(),
        };
        let failed = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &Some(prepared.clone()),
        );
        assert!(Arc::ptr_eq(&prepared.queued[0], &failed.queued[0]));
        let ConversationRowContent::Preparation { preparation } = &failed.session_rows[0].content
        else {
            panic!("missing failure")
        };
        assert!(preparation.retry.is_some() && !preparation.in_progress);
        let mut user = models::Item::new(
            "native-input".into(),
            models::ItemStatus::Completed,
            models::ItemBody::UserMessage {
                text: Some("accepted text".into()),
                content: Vec::new(),
            },
        );
        user.client_input_id = Some("input".into());
        thread.preparation.as_mut().unwrap().status = PreparationStatus::Unknown;
        thread.turns = Some(vec![Arc::new(models::Turn {
            id: "native-turn".into(),
            items: Some(vec![Arc::new(user)]),
            ..Default::default()
        })]);
        let echoed = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread.clone()),
            &Some(failed),
        );
        assert!(echoed.queued.is_empty());
        assert!(echoed.session_rows.is_empty());
        thread.preparation = None;
        let complete = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread),
            &Some(echoed),
        );
        assert!(complete.session_rows.is_empty());
        assert_eq!(
            complete.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(row.content, ConversationRowContent::User { .. }))
                .count(),
            1
        );
    }
}

fn render_turn(
    session: Option<&crate::session::SessionRef>,
    supports_fork: bool,
    handoff: Option<String>,
    source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    previous: Option<&Arc<RenderedTurn>>,
) -> Arc<RenderedTurn> {
    let session_provider = session.map(|session| session.provider);
    let provider = source.provider.or(session_provider);
    let supports_fork = source.supports_fork.unwrap_or(supports_fork);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .filter(|turn| turn.source.provider.or(session_provider) == provider)
        .flat_map(|turn| turn.items())
        .map(|item| (item.key(), item))
        .collect();
    let native = source.items.as_deref().unwrap_or_default();
    let echoed: HashSet<_> = if pending.is_empty() {
        HashSet::new()
    } else {
        native
            .iter()
            .filter_map(|item| item.client_input_id.as_ref())
            .collect()
    };
    // Snapshot keys already make pending IDs unique.
    let retained: Vec<_> = pending
        .iter()
        .filter(|(id, _)| !echoed.contains(id))
        .collect();
    let order = source_order(
        native.len(),
        |index| native[index].id.as_str(),
        retained
            .iter()
            .map(|(_, pending)| pending.after_item_id.as_deref()),
    );
    let metadata = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            ItemMetadata::from(item.as_ref())
        } else {
            let id = retained[index - native.len()].0.as_str();
            ItemMetadata {
                id,
                kind: super::GroupKind::User,
                ..Default::default()
            }
        }
    };
    let render_native = |item: &Arc<models::Item>| {
        RenderedItem::native(
            item,
            provider,
            item.is_deferred(),
            None,
            cached.get(&(None, Arc::as_ptr(item).cast())).copied(),
        )
    };
    let render = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            render_native(item)
        } else {
            let (id, submission) = retained[index - native.len()];
            RenderedItem::pending(
                id,
                submission,
                cached
                    .get(&(Some(id.as_str()), Arc::as_ptr(submission).cast()))
                    .copied(),
            )
        }
    };
    use ConversationRowContent::*;
    let mut rows = Vec::new();
    let mut occurrences = HashMap::new();
    let mut push = |content: ConversationRowContent| {
        let id = match &content {
            ProviderHandoff { .. } => format!("history-handoff:{}", source.id),
            User { item } | Activity { item, .. } | Response { item, .. } => {
                format!("history-item:{}:{}", source.id, item.data.id)
            }
            ActivityHeader { activity } => activity.id.clone(),
            PendingRequest { request } => format!("history-request:{}", request.id),
            Error { .. } => format!("history-error:{}", source.id),
            InProgress { turn_id } => format!("in-progress:{turn_id}"),
            Preparation { .. } => "preparation".into(),
        };
        let occurrence = occurrences.entry(id.clone()).or_insert(0usize);
        let id = format!("{id}:occurrence:{occurrence}");
        *occurrence += 1;
        let started = source.started_at_ms.or_else(|| {
            source
                .started_at
                .filter(|seconds| {
                    seconds.is_finite() && *seconds >= 0. && *seconds < u64::MAX as f64 / 1000.
                })
                .map(|seconds| (seconds * 1000.) as u64)
        });
        let timestamp_ms = match &content {
            User { item } => item
                .data
                .timestamp_ms
                .or(item.data.native_id.as_ref().and(started)),
            Response {
                show_actions: true, ..
            } => source.completed_at_ms.or_else(|| {
                started
                    .zip(source.duration_ms)
                    .and_then(|(start, duration)| start.checked_add(duration))
            }),
            _ => None,
        };
        rows.push(ConversationRow {
            id,
            content,
            timestamp_ms,
        });
    };
    if let Some(text) = handoff {
        push(ProviderHandoff { text });
    }
    for segment in project_items(&source, order.len(), metadata) {
        let segment = &segment;
        let group = |role| {
            (segment.start..segment.end)
                .filter(move |&index| segment.role(index, metadata(index)) == role)
                .map(&render)
        };
        let in_progress = segment.last && source.status == models::TurnStatus::Running;
        for item in group(Role::User) {
            if item.data.deferred {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            } else {
                push(User { item });
            }
        }
        let summary = if source.items_summary {
            Some(super::work_summary(&source))
        } else {
            segment.label.clone()
        };
        if let Some(summary) = summary {
            push(ActivityHeader {
                activity: ActivityPresentation {
                    id: segment.id.clone(),
                    status: source.status.label().into(),
                    activity_summary: summary,
                    activity_can_collapse: segment.collapsible || source.items_summary,
                    is_in_progress: in_progress,
                    load_items: session.filter(|_| source.items_summary).map(|thread_id| {
                        crate::state::operations::LoadTurnItems {
                            thread_id: thread_id.clone(),
                            turn_id: source.id.clone(),
                        }
                    }),
                },
            });
            for item in group(Role::Activity) {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            }
        }
        if segment.last {
            for pending in &requests {
                push(PendingRequest {
                    request: Box::new(request(pending)),
                });
            }
            if let Some(error) = &source.error {
                push(Error {
                    error: turn_error(error),
                });
            }
        }
        let mut responses = group(Role::Response).peekable();
        while let Some(item) = responses.next() {
            // A provider message can end while the response is still running.
            let show_actions =
                source.status != models::TurnStatus::Running && item.data.kind == "agent";
            let can_fork =
                supports_fork && show_actions && segment.last && responses.peek().is_none();
            if item.data.deferred {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            } else {
                push(Response {
                    item,
                    show_actions,
                    fork_turn_id: can_fork.then(|| source.id.clone()),
                });
            }
        }
        if in_progress {
            push(InProgress {
                turn_id: source.id.clone(),
            });
        }
    }
    Arc::new(RenderedTurn {
        source,
        pending,
        requests,
        rows: images::without_repeated_generated_images(rows),
    })
}

pub fn request(source: &WireRequest) -> Request {
    use agent_protocol::requests::{ApprovalKind, RequestBody};
    let (title, body, details) = match &source.body {
        RequestBody::Approval {
            kind,
            description,
            details,
            ..
        } => {
            let title = match kind {
                ApprovalKind::Command => "コマンドの承認待ち",
                ApprovalKind::FileChange => "ファイル変更の承認待ち",
                ApprovalKind::Tool => "ツールの承認待ち",
            };
            (title, description.clone(), details.clone())
        }
        RequestBody::Permission {
            description,
            details,
            ..
        } => ("権限の承認待ち", description.clone(), details.clone()),
        RequestBody::Question { questions } => (
            "回答待ち",
            questions
                .first()
                .map(|question| question.prompt.clone())
                .unwrap_or_default(),
            String::new(),
        ),
        RequestBody::Elicitation {
            server, message, ..
        } => (
            "MCPからの入力待ち",
            format!("{server}\n{message}"),
            String::new(),
        ),
        RequestBody::ToolExecution {
            tool, arguments, ..
        } => (
            "ツールの入力待ち",
            tool.clone(),
            serde_json::to_string_pretty(arguments).expect("arguments serialize"),
        ),
    };
    Request {
        id: source.id.clone(),
        title: match source.delivery {
            crate::session::RequestDelivery::Awaiting => title,
            crate::session::RequestDelivery::Sending => "回答を送信中",
            crate::session::RequestDelivery::Sent => "回答を送信しました",
            crate::session::RequestDelivery::Unknown => "回答の配送結果が不明です",
        }
        .into(),
        body,
        details,
        can_respond: source.delivery == crate::session::RequestDelivery::Awaiting,
        request_body: source.body.clone(),
    }
}

/// How prominently a request action is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ActionTone {
    Primary,
    Secondary,
    Danger,
}

/// T3 Code leads with a one-time grant and marks denial; broader grants stay
/// secondary.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn choice_tone(effect: agent_protocol::requests::ChoiceEffect) -> ActionTone {
    use agent_protocol::requests::ChoiceEffect;
    match effect {
        ChoiceEffect::Allow => ActionTone::Primary,
        ChoiceEffect::Deny => ActionTone::Danger,
        ChoiceEffect::AllowLasting => ActionTone::Secondary,
    }
}

/// The composer answers one question and one other request at a time, as in
/// T3 Code; later ones follow once these resolve.
pub(crate) fn docked_request(thread: &models::Thread, question: bool) -> Option<&WireRequest> {
    thread.requests.values().map(Arc::as_ref).find(|request| {
        matches!(
            request.body,
            agent_protocol::requests::RequestBody::Question { .. }
        ) == question
    })
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// The requests shown above the composer: the other request, then the question.
    pub fn docked_requests(&self, session: crate::session::SessionRef) -> Vec<Request> {
        let Some(thread) = self.sessions.conversations.get(&session) else {
            return Vec::new();
        };
        [false, true]
            .into_iter()
            .filter_map(|question| docked_request(thread, question))
            .map(request)
            .collect()
    }
}

/// Native editors keep typed choice IDs separate from free text.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn build_question_answer(
    multiple: bool,
    text: String,
    choice_ids: Vec<String>,
) -> agent_protocol::requests::QuestionAnswer {
    use agent_protocol::requests::QuestionAnswer;
    if !text.is_empty() {
        QuestionAnswer::FreeText { text }
    } else if multiple {
        QuestionAnswer::MultipleChoices { choice_ids }
    } else {
        QuestionAnswer::SingleChoice {
            choice_id: choice_ids.into_iter().next().unwrap_or_default(),
        }
    }
}

pub fn answer_from_json(
    body: &agent_protocol::requests::RequestBody,
    text: &str,
) -> Result<agent_protocol::requests::Answer, String> {
    use agent_protocol::requests::{Answer, ElicitationAnswer, RequestBody, ToolContent};
    let answer = match body {
        RequestBody::Elicitation { .. } => Answer::Elicitation {
            action: ElicitationAnswer::Accept {
                values: serde_json::from_str(text).map_err(|error| error.to_string())?,
            },
        },
        RequestBody::ToolExecution { .. } => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ResultInput {
                success: bool,
                content: Vec<ToolContent>,
            }
            let value: ResultInput =
                serde_json::from_str(text).map_err(|error| error.to_string())?;
            Answer::ToolExecution {
                success: value.success,
                content: value.content,
            }
        }
        _ => return Err("this request requires a typed choice or question answer".into()),
    };
    agent_protocol::requests::validate_answer(body, &answer)?;
    Ok(answer)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn request_input_default(body: agent_protocol::requests::RequestBody) -> String {
    use agent_protocol::requests::{ElicitationInput, FormInput, RequestBody};
    let value = match body {
        RequestBody::Elicitation {
            input: ElicitationInput::Form { fields },
            ..
        } => Value::Object(
            fields
                .into_iter()
                .filter_map(|field| {
                    let default = match field.input {
                        FormInput::String { default, .. } | FormInput::Choice { default, .. } => {
                            default.map(Value::from)
                        }
                        FormInput::Number { default, .. } => default.map(Value::from),
                        FormInput::Boolean { default } => default.map(Value::from),
                        FormInput::Multiple { default, .. } if !default.is_empty() => {
                            Some(serde_json::json!(default))
                        }
                        _ => None,
                    };
                    default.map(|value| (field.name, value))
                })
                .collect(),
        ),
        RequestBody::Elicitation {
            input: ElicitationInput::Url { .. },
            ..
        } => Value::Null,
        RequestBody::ToolExecution { .. } => serde_json::json!({"success":true,"content":[]}),
        _ => Value::Null,
    };
    serde_json::to_string_pretty(&value).expect("request defaults serialize")
}
fn turn_error(error: &models::ExecutionError) -> TurnErrorPresentation {
    use models::ErrorCategory::*;
    let retrying = error.retry.as_ref().is_some_and(|retry| retry.retrying);
    let title = if retrying {
        if error.retry.as_ref().is_some_and(|retry| retry.overloaded) {
            "サーバーが混み合っています。再接続しています"
        } else {
            "再接続しています"
        }
    } else {
        match error.category {
            ContextLimit => "コンテキストの上限に達しました",
            SessionLimit => "セッションの上限に達しました",
            UsageLimit => "利用上限に達しました",
            Overloaded => "サーバーが混み合っています",
            RateLimited => "リクエストの上限に達しました",
            Policy => "安全ポリシーにより停止しました",
            Internal => "サーバーエラー",
            Auth => "認証が必要です",
            InvalidInput => "リクエストを処理できません",
            Rollback => "タスクを元に戻せませんでした",
            Sandbox => "サンドボックスエラー",
            InputUnavailable => "この作業中はメッセージを追加できません",
            Network => "接続エラー",
            Other | Provider(_) => "エラー",
        }
    };
    TurnErrorPresentation {
        title: title.into(),
        message: error.message.clone(),
        details: error.details.clone(),
        is_reconnecting: retrying,
        is_usage_limit: !retrying && matches!(error.category, UsageLimit),
    }
}

pub type PendingItems = Vec<(agent_protocol::ids::ClientInputId, Arc<PendingSubmission>)>;
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ItemPresentation {
    pub id: String,
    pub native_id: Option<agent_protocol::ids::ItemId>,
    pub timestamp_ms: Option<u64>,
    pub body: Option<String>,
    pub image_sources: Vec<String>,
    pub image_placeholder: bool,
    pub deferred: bool,
    pub kind: String,
    pub title: Option<String>,
    pub collapsible: bool,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Request {
    pub id: agent_protocol::ids::RequestId,
    pub title: String,
    pub body: String,
    pub can_respond: bool,
    pub details: String,
    pub request_body: agent_protocol::requests::RequestBody,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Snapshot};
    use serde_json::json;
    use std::collections::HashSet;

    #[rstest::rstest]
    #[case::plan(json!({"plan":{"text":"Plan"}}), true)]
    #[case::mcp(json!({"toolCall":{"kind":"mcp","tool":"read","arguments":{},"content":[]}}), true)]
    #[case::dynamic(json!({"toolCall":{"kind":"dynamic","tool":"search","arguments":{},"content":[]}}), true)]
    #[case::subagent(json!({"subagent":{"tool":"spawn_agent","receivers":[],"states":[]}}), true)]
    #[case::web(json!({"webSearch":{"query":"Query"}}), true)]
    #[case::image(json!({"imageGeneration":{"savedPath":"/fixture/image.png"}}), true)]
    #[case::compaction(json!({"compaction":{}}), true)]
    #[case::sleep(json!({"sleep":{}}), false)]
    #[case::reasoning(json!({"reasoning":{"content":["private"],"summary":[]}}), false)]
    #[case::review_entered(json!({"review":{"entering":true,"text":"Review"}}), false)]
    #[case::review_left(json!({"review":{"entering":false,"text":"Review"}}), false)]
    fn item_families_project_visible_content_without_exposing_state_only_events(
        #[case] body: serde_json::Value,
        #[case] visible: bool,
        #[values(models::TurnStatus::Running, models::TurnStatus::Completed)]
        status: models::TurnStatus,
    ) {
        let source = Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                id: "work".into(),
                status,
                items: Some(vec![
                    Arc::new(models::Item::new(
                        "family".into(),
                        Default::default(),
                        serde_json::from_value(body).unwrap(),
                    )),
                    Arc::new(models::Item::new(
                        "answer".into(),
                        Default::default(),
                        models::ItemBody::AssistantText {
                            text: "Final answer".into(),
                            phase: models::AssistantPhase::Final,
                        },
                    )),
                ]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let snapshot = Snapshot::default();
        let rendered = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            source,
            &None,
        );
        let ids = rendered.turns[0]
            .items()
            .map(|item| item.data.native_id.as_deref().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            if visible {
                vec!["family", "answer"]
            } else {
                vec!["answer"]
            }
        );
    }

    #[rstest::rstest]
    #[case::distinct_turns(false)]
    #[case::repeated_turn_ids(true)]
    fn reopening_history_retains_each_answer_and_its_native_row_identity(#[case] repeated: bool) {
        let source = Arc::new(models::Thread {
            turns: Some(
                [("old", "Earlier answer"), ("latest", "Latest answer")]
                    .into_iter()
                    .map(|(id, text)| {
                        Arc::new(models::Turn {
                            id: if repeated { "repeated" } else { id }.into(),
                            status: models::TurnStatus::Completed,
                            items: Some(vec![Arc::new(models::Item::new(
                                id.into(),
                                Default::default(),
                                models::ItemBody::AssistantText {
                                    text: text.into(),
                                    phase: models::AssistantPhase::Final,
                                },
                            ))]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        let snapshot = Snapshot::default();
        let live = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            source.clone(),
            &None,
        );
        let restored: Arc<models::Thread> =
            Arc::new(serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap());
        for previous in [None, Some(live)] {
            let reopened = project_conversation(
                &snapshot.pane.navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                restored.clone(),
                &previous,
            );
            let rows = reopened
                .turns
                .iter()
                .flat_map(|turn| turn.conversation_rows())
                .collect::<Vec<_>>();
            let answers = rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::Response { item, .. } => item.data.body.as_deref(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(answers, ["Earlier answer", "Latest answer"]);
            assert_eq!(
                rows.iter().map(|row| &row.id).collect::<HashSet<_>>().len(),
                rows.len()
            );
        }
    }

    #[test]
    fn work_expansion_requires_an_explicit_choice_for_the_current_status() {
        let statuses = ["running", "completed", "failed", "interrupted", "unknown"];
        for status in statuses {
            assert!(!activity_is_expanded(status, None));
            for previous in statuses {
                assert_eq!(
                    activity_is_expanded(status, Some(previous.into())),
                    status == previous,
                );
            }
        }
    }

    #[rstest::rstest]
    #[case::initial_viewport_needs_more(true, false, true, true, true, true)]
    #[case::wait_for_initial_latest_position(true, false, true, false, true, false)]
    #[case::older_boundary_is_visible(true, false, true, false, false, true)]
    #[case::viewport_is_filled(true, false, false, true, true, false)]
    #[case::reading_the_middle(true, false, false, false, false, false)]
    #[case::request_in_flight(true, true, true, true, true, false)]
    #[case::all_history_loaded(false, false, true, true, true, false)]
    fn history_pages_follow_the_viewport(
        #[case] has_more: bool,
        #[case] loading: bool,
        #[case] oldest_visible: bool,
        #[case] latest_visible: bool,
        #[case] following_latest: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_load_history(
                has_more,
                loading,
                oldest_visible,
                latest_visible,
                following_latest
            ),
            expected
        );
    }

    fn fixture() -> Snapshot {
        let thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"done","status":"completed","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"earlier","phase":"unknown"}}}}}]},{"id":"live","status":"running","items":[{"id":"user","status":"unknown","clientInputId":"accepted","body":{"inline":{"body":{"userMessage":{"text":"question","content":[]}}}}},{"id":"command","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"pwd","cwd":null,"output":"/fixture","exitCode":null}}}}},{"id":"stream","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"hello","phase":"unknown"}}}}}]}]})).unwrap();
        Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        agent_protocol::session::SessionRef {
                            provider: agent_protocol::session::ProviderKind::Codex,
                            id: "thread".into(),
                        },
                        Arc::new(thread),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    #[test]
    fn older_history_reveals_a_provider_boundary_without_stale_cached_rows() {
        let mut snapshot = fixture();
        let session = snapshot
            .sessions
            .conversations
            .keys()
            .next()
            .unwrap()
            .clone();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turns = thread.turns.as_mut().unwrap();
        Arc::make_mut(&mut turns[0]).provider = Some(crate::session::ProviderKind::Codex);
        Arc::make_mut(&mut turns[1]).provider = Some(crate::session::ProviderKind::Claude);
        let old = turns.remove(0);
        let initial = project_snapshot(snapshot.clone(), None);
        assert!(
            !initial.turns[0]
                .conversation_rows()
                .iter()
                .any(|row| matches!(row.content, ConversationRowContent::ProviderHandoff { .. }))
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().insert(0, old);
        let loaded = project_snapshot(snapshot.clone(), Some(&initial));
        assert!(
            matches!(&loaded.turns[1].conversation_rows()[0].content, ConversationRowContent::ProviderHandoff { text } if text == "Codex → Claude に切り替えました")
        );
        assert!(!Arc::ptr_eq(&initial.turns[0], &loaded.turns[1]));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]).duration_ms = Some(500);
        let updated = project_snapshot(snapshot, Some(&loaded));
        assert!(
            Arc::ptr_eq(&loaded.turns[1], &updated.turns[1]),
            "unchanged provider boundaries retain their native render identity"
        );
    }

    #[test]
    fn completed_edits_stay_in_collapsed_activity_when_details_load() {
        let mut snapshot = fixture();
        let session = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "thread".into(),
        };
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
        turn.started_at_ms = Some(1_000);
        turn.duration_ms = Some(5_000);
        turn.status = models::TurnStatus::Completed;
        let edit: models::Item = serde_json::from_value(json!({"id":"edit","status":"completed","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"src/a.rs","kind":"add","diff":"@@ -0,0 +1 @@\n+new","proposal":null}],"output":""}}}}})).unwrap();
        let mut deferred = edit.clone();
        deferred.defer();
        let items = turn.items.as_mut().unwrap();
        let index = items.len() - 1;
        items.insert(index, Arc::new(deferred));
        let projection = project_snapshot(snapshot.clone(), None);
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
        turn.items.as_mut().unwrap()[index] = Arc::new(edit);
        let loaded = project_snapshot(snapshot, Some(&projection));
        for (rendered, deferred) in [(&projection, true), (&loaded, false)] {
            let rows = rendered.turns[1].conversation_rows();
            assert_eq!(rows[0].timestamp_ms, Some(1_000));
            let response = rows.last().unwrap();
            assert_eq!(response.timestamp_ms, Some(6_000));
            assert!(matches!(
                &response.content,
                ConversationRowContent::Response { item, .. }
                    if item.data.native_id.as_deref() == Some("stream")
            ));
            let activity = rows
                .iter()
                .find_map(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity } => Some(activity),
                    _ => None,
                })
                .unwrap();
            assert!(activity.activity_can_collapse);
            assert!(!activity_is_expanded(&activity.status, None));
            let edit = rows
                .iter()
                .find_map(|row| match &row.content {
                    ConversationRowContent::Activity { item, .. }
                        if item.data.native_id.as_deref() == Some("edit") =>
                    {
                        Some(item)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(edit.data.deferred, deferred);
            assert!(edit.data.collapsible);
            if !deferred {
                assert_eq!(edit.expanded_body(), "add: src/a.rs\n@@ -0,0 +1 @@\n+new");
            }
        }
    }
    fn project_snapshot(
        snapshot: Snapshot,
        previous: Option<&Arc<RenderedConversation>>,
    ) -> Arc<RenderedConversation> {
        project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            snapshot.sessions.conversations[&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }]
                .clone(),
            &previous.cloned(),
        )
    }

    #[test]
    fn request_choices_rank_as_t3_ranks_approval_decisions() {
        use agent_protocol::requests::ChoiceEffect;
        for (effect, tone) in [
            (ChoiceEffect::Allow, ActionTone::Primary),
            (ChoiceEffect::AllowLasting, ActionTone::Secondary),
            (ChoiceEffect::Deny, ActionTone::Danger),
        ] {
            assert_eq!(choice_tone(effect), tone);
        }
    }

    #[test]
    fn composer_docks_one_other_request_before_one_question_in_request_order() {
        use agent_protocol::{
            requests::{Question, RequestBody, RequestTarget},
            session::{ProviderKind, RequestDelivery, SessionRef},
        };
        let question = || RequestBody::Question {
            questions: vec![Question {
                id: "q".into(),
                header: String::new(),
                prompt: "which?".into(),
                secret: false,
                allow_free_text: true,
                multiple: false,
                choices: vec![],
            }],
        };
        let tool = || RequestBody::ToolExecution {
            tool: "tool".into(),
            namespace: None,
            arguments: json!({}),
        };
        let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
        let mut snapshot = Snapshot::default();
        assert!(snapshot.docked_requests(session.clone()).is_empty());
        let thread = models::Thread {
            requests: [
                ("1-question", question()),
                ("2-tool", tool()),
                ("3-question", question()),
                ("4-tool", tool()),
            ]
            .into_iter()
            .map(|(id, body)| {
                let request = WireRequest {
                    id: id.into(),
                    target: RequestTarget::Session,
                    delivery: RequestDelivery::Awaiting,
                    body,
                };
                (request.id.clone(), Arc::new(request))
            })
            .collect(),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session.clone(), Arc::new(thread));
        let docked: Vec<_> = snapshot
            .docked_requests(session.clone())
            .into_iter()
            .map(|request| request.id.as_str().to_owned())
            .collect();
        assert_eq!(docked, ["2-tool", "1-question"]);
        assert_eq!(
            snapshot
                .question_prompt(session)
                .unwrap()
                .request_id
                .as_str(),
            "1-question"
        );
    }

    #[test]
    fn repeated_turns_keep_pending_input_on_the_latest_occurrence_and_route_requests_in_order() {
        use agent_protocol::{
            requests::{RequestBody, RequestTarget},
            session::{ProviderKind, RequestDelivery, SessionRef},
        };
        let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
        let mut source = models::Thread {
            id: Some(session.clone()),
            turns: Some(
                ["first", "last"]
                    .into_iter()
                    .map(|text| {
                        Arc::new(models::Turn {
                            id: "repeat".into(),
                            status: models::TurnStatus::Completed,
                            items: Some(vec![Arc::new(models::Item::new(
                                text.into(),
                                Default::default(),
                                models::ItemBody::AssistantText {
                                    text: text.into(),
                                    phase: models::AssistantPhase::Final,
                                },
                            ))]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let mut snapshot = Snapshot::default();
        for (sequence, (id, turn, accepted)) in [
            ("assigned", Some("repeat"), true),
            ("missing", Some("absent"), true),
            ("queued", None, true),
            ("unassigned", None, false),
        ]
        .into_iter()
        .enumerate()
        {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                id.into(),
                Arc::new(PendingSubmission {
                    sequence: sequence as u64,
                    submitted_at_ms: None,
                    draft_key: session.clone().into(),
                    draft: Arc::new(Draft {
                        text: id.into(),
                        ..Default::default()
                    }),
                    turn_id: turn.map(Into::into),
                    after_item_id: None,
                    accepted,
                    delivery_unknown: false,
                }),
            );
        }
        for (id, turn) in [
            ("a-session", None),
            ("b-turn", Some("repeat")),
            ("c-missing", Some("absent")),
            ("d-turn", Some("repeat")),
        ] {
            let request = WireRequest {
                id: id.into(),
                target: turn.map_or(RequestTarget::Session, |turn| RequestTarget::Turn {
                    turn_id: turn.into(),
                    item_id: None,
                }),
                delivery: RequestDelivery::Awaiting,
                body: RequestBody::ToolExecution {
                    tool: id.into(),
                    namespace: None,
                    arguments: Value::Null,
                },
            };
            source
                .requests
                .insert(request.id.clone(), Arc::new(request));
        }
        let projected = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(source),
            &None,
        );
        assert_eq!(
            projected.turns[0]
                .items()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["unassigned", "first"]
        );
        assert_eq!(
            projected.turns[1]
                .items()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["assigned", "last"]
        );
        assert_eq!(
            projected
                .queued
                .iter()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["missing", "queued"]
        );
        let requests = |rows: &[ConversationRow]| {
            rows.iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::PendingRequest { request } => {
                        Some(request.id.to_string())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            requests(&projected.session_rows),
            ["a-session", "c-missing"]
        );
        for turn in &projected.turns {
            assert_eq!(requests(&turn.rows), ["b-turn", "d-turn"]);
        }
    }

    #[test]
    fn session_requests_render_without_history_and_sent_requests_disable_answers() {
        use agent_protocol::{
            requests::{ElicitationInput, RequestBody, RequestTarget},
            session::RequestDelivery,
        };
        let make = |delivery| {
            let request = WireRequest {
                id: "request".into(),
                target: RequestTarget::Session,
                delivery,
                body: RequestBody::Elicitation {
                    server: "mcp".into(),
                    message: "confirm".into(),
                    input: ElicitationInput::Url {
                        url: "https://example.com/".into(),
                    },
                },
            };
            Arc::new(models::Thread {
                requests: [(request.id.clone(), Arc::new(request))].into(),
                ..Default::default()
            })
        };
        let awaiting = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            make(RequestDelivery::Awaiting),
            &None,
        );
        assert!(awaiting.turns.is_empty());
        assert_eq!(awaiting.session_rows.len(), 1);
        let ConversationRowContent::PendingRequest { request } = &awaiting.session_rows[0].content
        else {
            panic!("request row")
        };
        assert!(request.can_respond);
        let sent = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            make(RequestDelivery::Sent),
            &Some(awaiting.clone()),
        );
        let ConversationRowContent::PendingRequest { request } = &sent.session_rows[0].content
        else {
            panic!("request row")
        };
        assert!(!request.can_respond);
        assert_ne!(
            &request.title,
            match &awaiting.session_rows[0].content {
                ConversationRowContent::PendingRequest { request } => &request.title,
                _ => unreachable!(),
            }
        );
    }

    #[test]
    fn generated_image_placeholder_yields_to_result_and_stops_on_failure() {
        for (status, saved_path, result, placeholder, sources, error) in [
            ("running", "", "", true, vec![], None),
            (
                "running",
                "/preview.png",
                "",
                false,
                vec!["/preview.png"],
                None,
            ),
            (
                "completed",
                "/generated.png",
                "",
                false,
                vec!["/generated.png"],
                None,
            ),
            (
                "completed",
                "",
                "png-data",
                false,
                vec!["data:image/png;base64,png-data"],
                None,
            ),
            ("completed", "", "", false, vec![], None),
            (
                "failed",
                "",
                "",
                false,
                vec![],
                Some("画像を生成できませんでした"),
            ),
        ] {
            let item = Arc::new(
                serde_json::from_value(json!({"id":"image","status":status,"clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":saved_path,"data":result,"revisedPrompt":null}}}}}))
                .unwrap(),
            );
            let projected = render_turn(
                Some(&crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "session".into()).unwrap()),
                false,
                None,
                Arc::new(models::Turn {
                    id: "turn".into(),
                    status: models::TurnStatus::Completed,
                    items: Some(vec![item, Arc::new(serde_json::from_value(json!({"id":"answer","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Here is the image","phase":"final"}}}}})).unwrap())]),
                    ..Default::default()
                }),
                vec![],
                vec![],
                None,
            );
            let items: Vec<_> = projected.items().collect();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].data.image_placeholder, placeholder);
            assert_eq!(items[0].data.image_sources, sources);
            assert_eq!(items[0].data.title, None);
            assert_eq!(items[0].data.body.as_deref(), error);
            assert_eq!(items[1].data.body.as_deref(), Some("Here is the image"));
        }
    }

    #[test]
    fn flat_rows_preserve_history_order_and_only_offer_fork_on_last_completed_response() {
        let mut snapshot = fixture();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        );
        thread.capabilities = Some(crate::session::Capabilities {
            fork: true,
            ..Default::default()
        });
        let rendered = project_snapshot(snapshot.clone(), None);
        let rows = rendered.turns[1].conversation_rows();
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert!(
            matches!(&rows[0].content, ConversationRowContent::User { item } if item.data.native_id.as_deref() == Some("user"))
        );
        let accepted_id = ids[0].to_owned();
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
        assert!(rows.iter().any(|row| matches!(&row.content, ConversationRowContent::Activity { item, .. } if item.data.native_id.as_deref() == Some("command"))));
        assert!(rows.iter().all(|row| !matches!(
            &row.content,
            ConversationRowContent::Response {
                fork_turn_id: Some(_),
                ..
            }
        )));
        assert!(matches!(
            rows.last().unwrap().content,
            ConversationRowContent::InProgress { .. }
        ));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]).status =
            models::TurnStatus::Completed;
        let completed = project_snapshot(snapshot, Some(&rendered));
        let rows = completed.turns[1].conversation_rows();
        assert!(
            matches!(&rows.last().unwrap().content, ConversationRowContent::Response { item, fork_turn_id: Some(id), .. } if id.as_str() == "live" && item.data.native_id.as_deref() == Some("stream"))
        );
        assert!(rows.iter().any(|row| row.id == accepted_id));
        for row in rows {
            if let ConversationRowContent::ActivityHeader { activity } = row.content {
                assert!(!activity_is_expanded(&activity.status, None));
                assert!(activity_is_expanded(
                    &activity.status,
                    Some(activity.status.clone())
                ));
                assert!(!activity_is_expanded(
                    &activity.status,
                    Some("running".into())
                ));
            }
        }
    }

    #[rstest::rstest]
    #[case::codex("codex", "commentary", "final")]
    #[case::claude("claude", "commentary", "final")]
    #[case::deepseek("deepseek", "unknown", "unknown")]
    fn response_actions_wait_for_turn_completion_and_only_belong_to_the_answer(
        #[case] provider: &str,
        #[case] progress_phase: &str,
        #[case] answer_phase: &str,
        #[values(false, true)] supports_fork: bool,
    ) {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(serde_json::from_value(json!({
            "id": {"provider": provider, "id": "thread"},
            "capabilities": {"fork": supports_fork},
            "turns": [{"id": "turn", "status": "running", "items": [
                {"id": "progress", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Working", "phase": progress_phase}}}}},
                {"id": "tool", "status": "completed", "body": {"inline": {"body": {"commandExecution": {"command": "pwd", "output": "/work"}}}}},
                {"id": "update", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Waiting", "phase": progress_phase}}}}},
                {"id": "answer", "status": "completed", "body": {"inline": {"body": {"assistantText": {"text": "Done", "phase": answer_phase}}}}}
            ]}]
        })).unwrap());
        let live = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &None,
        );
        let responses: Vec<_> = live.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Response {
                    item,
                    show_actions,
                    fork_turn_id,
                } => {
                    assert!(
                        !show_actions,
                        "live messages must not expose answer actions"
                    );
                    assert!(fork_turn_id.is_none());
                    Some(item)
                }
                _ => None,
            })
            .collect();
        assert_eq!(responses.len(), 3);
        let live_answer = responses.last().unwrap();

        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).status =
            models::TurnStatus::Completed;
        let completed = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread,
            &Some(live.clone()),
        );
        let actions: Vec<_> = completed.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Response {
                    item,
                    show_actions: true,
                    fork_turn_id,
                } => {
                    assert_eq!(fork_turn_id.is_some(), supports_fork);
                    Some(item)
                }
                _ => None,
            })
            .collect();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].data.native_id.as_deref(), Some("answer"));
        assert!(Arc::ptr_eq(live_answer, actions[0]));
        let reopened = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            completed.source.clone(),
            &None,
        );
        assert_eq!(
            reopened.turns[0]
                .rows
                .iter()
                .filter(|row| matches!(
                    row.content,
                    ConversationRowContent::Response {
                        show_actions: true,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn cached_assistant_labels_follow_turn_provider_attribution() {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(
            serde_json::from_value(json!({
                "id": {"provider": "codex", "id": "thread"},
                "turns": [{"id": "turn", "status": "completed", "items": [
                    {"id": "answer", "status": "completed", "body": {"inline": {
                        "body": {"assistantText": {"text": "Done", "phase": "final"}}
                    }}}
                ]}]
            }))
            .unwrap(),
        );
        let initial = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &None,
        );
        let before = initial.turns[0].items().next().unwrap();
        assert_eq!(before.data.title.as_deref(), Some("Codex"));
        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).provider =
            Some(crate::session::ProviderKind::Codex);
        let attributed = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &Some(initial.clone()),
        );
        assert!(Arc::ptr_eq(
            before,
            attributed.turns[0].items().next().unwrap()
        ));
        Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).provider =
            Some(crate::session::ProviderKind::Claude);
        let updated = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread,
            &Some(attributed),
        );
        let after = updated.turns[0].items().next().unwrap();
        assert_eq!(after.data.title.as_deref(), Some("Claude"));
        assert_eq!(after.data.body.as_deref(), Some("Done"));
        assert!(matches!((&before.source, &after.source),
            (ItemSource::Native(before), ItemSource::Native(after)) if Arc::ptr_eq(before, after)));
        let unchanged = project_conversation(
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            updated.source.clone(),
            &Some(updated.clone()),
        );
        assert!(Arc::ptr_eq(&updated.turns[0], &unchanged.turns[0]));
    }

    #[rstest::rstest]
    fn cached_response_actions_follow_capability_changes(
        #[values(None, Some(false), Some(true))] turn_supports_fork: Option<bool>,
    ) {
        let snapshot = Snapshot::default();
        let mut thread: Arc<models::Thread> = Arc::new(
            serde_json::from_value(json!({
                "id": {"provider": "codex", "id": "thread"},
                "turns": [{"id": "turn", "status": "completed", "supportsFork": turn_supports_fork,
                    "items": [{"id": "answer", "status": "completed", "body": {"inline": {
                        "body": {"assistantText": {"text": "Done", "phase": "final"}}
                    }}}]
                }]
            }))
            .unwrap(),
        );
        let mut previous = None;
        for enabled in [false, true, false] {
            Arc::make_mut(&mut thread).capabilities = Some(crate::session::Capabilities {
                fork: enabled,
                ..Default::default()
            });
            let projected = project_conversation(
                &snapshot.pane.navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                thread.clone(),
                &previous,
            );
            let actions: Vec<_> = projected.turns[0]
                .rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::Response { fork_turn_id, .. } => Some(fork_turn_id),
                    _ => None,
                })
                .collect();
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].is_some(), turn_supports_fork.unwrap_or(enabled));
            if let Some(previous) = &previous {
                assert!(Arc::ptr_eq(
                    &previous.turns[0].source,
                    &projected.turns[0].source,
                ));
                assert_eq!(
                    Arc::ptr_eq(&previous.turns[0], &projected.turns[0]),
                    turn_supports_fork.is_some(),
                    "only turns that inherit the changed capability need new rows",
                );
                assert!(Arc::ptr_eq(
                    previous.turns[0].items().next().unwrap(),
                    projected.turns[0].items().next().unwrap(),
                ));
            }
            previous = Some(projected);
        }
    }

    #[test]
    fn flat_row_ids_distinguish_repeated_items_and_turns() {
        let source = Arc::new(
            serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"first","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old","phase":"unknown"}}}}},{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"final"}}}}}]},{"id":"second","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"another turn","phase":"unknown"}}}}}]}]}))
            .unwrap(),
        );
        let rendered = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            source,
            &None,
        );
        let rows: Vec<_> = rendered
            .turns
            .iter()
            .flat_map(|turn| turn.conversation_rows())
            .collect();
        assert_eq!(
            rows.iter().map(|row| &row.id).collect::<HashSet<_>>().len(),
            rows.len()
        );
        let texts: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Activity { item, .. }
                | ConversationRowContent::Response { item, .. } => item.data.body.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["old", "new", "another turn"]);
        let again: Vec<_> = rendered
            .turns
            .iter()
            .flat_map(|turn| turn.conversation_rows())
            .map(|row| row.id)
            .collect();
        assert_eq!(
            again,
            rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>()
        );
        let mut changed = rendered.source.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut changed).turns.as_mut().unwrap()[0]).status =
            models::TurnStatus::Interrupted;
        let updated = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            changed,
            &Some(rendered.clone()),
        );
        assert_eq!(updated.turns[0].items().count(), 2);
        for (before, after) in rendered.turns[0].items().zip(updated.turns[0].items()) {
            assert!(
                Arc::ptr_eq(before, after),
                "unchanged items must retain their cache even when IDs repeat"
            );
        }
    }

    #[test]
    fn summaries_show_input_and_answer_with_activity_loaded_on_expansion() {
        let source = Arc::new(serde_json::from_value(json!({
            "id":{"provider":"codex","id":"thread"},"turns":[{
                "id":"turn","status":"completed","itemsSummary":true,"items":[
                    {"id":"user","body":{"inline":{"body":{"userMessage":{"text":"question","content":[]}}}}},
                    {"id":"answer","body":{"inline":{"body":{"assistantText":{"text":"answer","phase":"final"}}}}}
                ]
            }]
        })).unwrap());
        let rendered = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            source,
            &None,
        );
        let rows = &rendered.turns[0].rows;
        assert!(matches!(
            &rows[0].content,
            ConversationRowContent::User { .. }
        ));
        let ConversationRowContent::ActivityHeader { activity } = &rows[1].content else {
            panic!("summary activity is inaccessible")
        };
        assert!(!activity_is_expanded(&activity.status, None));
        assert!(activity.activity_can_collapse);
        assert_eq!(activity.load_items.as_ref().unwrap().thread_id.id, "thread");
        assert_eq!(
            activity.load_items.as_ref().unwrap().turn_id.as_str(),
            "turn"
        );
        assert!(matches!(
            &rows[2].content,
            ConversationRowContent::Response { .. }
        ));
    }

    #[test]
    fn unknown_submissions_keep_send_order_and_position_after_reopening() {
        use crate::state::{Event, Intent, reduce};
        for status in ["completed", "running"] {
            let mut snapshot = Snapshot {
                sessions: crate::state::Sessions {
                    conversations: Arc::new(
                        [(
                            agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() },
                            Arc::new(
                                serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"capabilities":{"additionalInput":true,"fork":false,"rename":false,"modelChange":false},"turns":[{"id":"before","status":status,"items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"before","phase":"unknown"}}}}}]}]}))
                                .unwrap(),
                            ),
                        )]
                        .into(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            };
            // IDs deliberately disagree with submission order.
            let mut activity_ids = HashSet::new();
            for id in ["z-first", "a-second", "m-third"] {
                Arc::make_mut(&mut snapshot.input.drafts).insert(
                    agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    }
                    .into(),
                    Arc::new(Draft {
                        text: id.into(),
                        ..Default::default()
                    }),
                );
                snapshot = reduce(
                    &snapshot,
                    Event::Intent(Intent::Submit {
                        thread_id: Some(agent_protocol::session::SessionRef {
                            provider: agent_protocol::session::ProviderKind::Codex,
                            id: "thread".into(),
                        }),
                        client_user_message_id: id.into(),
                    }),
                )
                .0;
                snapshot = reduce(&snapshot, Event::SubmissionUnknown(id.into())).0;
                if status == "running" {
                    let rendered = project_snapshot(snapshot.clone(), None);
                    let activity = rendered.turns[0]
                        .rows
                        .iter()
                        .find_map(|row| match &row.content {
                            ConversationRowContent::ActivityHeader { activity } => Some(activity),
                            _ => None,
                        })
                        .unwrap();
                    assert!(
                        activity_ids.insert(activity.id.clone()),
                        "a new input must not inherit another group's expansion"
                    );
                }
            }
            let first = project_snapshot(snapshot.clone(), None);
            let texts = |rendered: &RenderedConversation| {
                rendered
                    .turns
                    .iter()
                    .flat_map(|turn| turn.items())
                    .chain(rendered.queued.iter())
                    .filter_map(|item| item.data.body.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(texts(&first), ["before", "z-first", "a-second", "m-third"]);
            assert!(Arc::ptr_eq(
                &first,
                &project_snapshot(snapshot.clone(), Some(&first))
            ));
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    })
                    .unwrap(),
            );
            thread.turns.as_mut().unwrap().push(Arc::new(
                serde_json::from_value(json!({"id":"later","status":"completed","items":[{"id":"later-user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"later","content":[]}}}}}]}))
                .unwrap(),
            ));
            let reopened: Snapshot =
                serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
            let rendered = project_snapshot(reopened, Some(&first));
            assert_eq!(
                texts(&rendered),
                ["before", "z-first", "a-second", "m-third", "later"]
            );
            assert!(rendered.queued.is_empty());
        }
    }

    #[test]
    fn queued_submissions_keep_send_order_without_history() {
        use crate::state::{Event, Intent, reduce};
        let mut snapshot = Snapshot {
            sessions: crate::state::Sessions {
                conversations: Arc::new(
                    [(
                        agent_protocol::session::SessionRef {
                            provider: agent_protocol::session::ProviderKind::Codex,
                            id: "thread".into(),
                        },
                        Arc::new(models::Thread {
                            id: Some(agent_protocol::session::SessionRef {
                                provider: agent_protocol::session::ProviderKind::Codex,
                                id: "thread".into(),
                            }),
                            ..Default::default()
                        }),
                    )]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        for id in ["z-first", "a-second", "m-third"] {
            snapshot = reduce(
                &snapshot,
                Event::Intent(Intent::Submit {
                    thread_id: Some(agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    }),
                    client_user_message_id: id.into(),
                }),
            )
            .0;
            snapshot = reduce(&snapshot, Event::SubmissionUnknown(id.into())).0;
        }
        let rendered = project_snapshot(snapshot.clone(), None);
        assert_eq!(
            rendered
                .queued
                .iter()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second", "m-third"]
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        )
        .turns = Some(vec![Arc::new(
            serde_json::from_value(json!({"id":"later","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"later","phase":"unknown"}}}}}],"status":"unknown"}))
            .unwrap(),
        )]);
        let updated = project_snapshot(snapshot, Some(&rendered));
        assert!(updated.queued.is_empty());
        assert_eq!(
            updated.turns[0]
                .items()
                .map(|item| item.data.id.as_str())
                .collect::<Vec<_>>(),
            ["z-first", "a-second", "m-third", "answer"]
        );
    }

    #[test]
    fn pending_echoes_render_once_and_keep_remaining_input_order() {
        let mut snapshot = fixture();
        for (sequence, id) in ["a", "b", "a", "c"].into_iter().enumerate() {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                id.into(),
                Arc::new(PendingSubmission {
                    sequence: sequence as u64,
                    submitted_at_ms: None,
                    draft_key: agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    }
                    .into(),
                    draft: Arc::new(Draft {
                        text: format!("pending {id} {sequence}"),
                        ..Default::default()
                    }),
                    turn_id: Some("echo-turn".into()),
                    after_item_id: Some("echo".into()),
                    accepted: false,
                    delivery_unknown: true,
                }),
            );
        }
        Arc::make_mut(Arc::make_mut(&mut snapshot.sessions.conversations).get_mut(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }).unwrap())
            .turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"echo-turn","items":[{"id":"echo","status":"unknown","clientInputId":"b","body":{"inline":{"body":{"userMessage":{"text":"native b","content":[]}}}}}],"status":"unknown"})).unwrap()));
        let rendered = project_snapshot(snapshot.clone(), None);
        let items: Vec<_> = rendered.turns.last().unwrap().items().collect();
        assert_eq!(
            items
                .iter()
                .filter_map(|item| item.data.body.as_deref())
                .collect::<Vec<_>>(),
            ["native b", "pending a 2", "pending c 3"]
        );
        assert!(matches!(&items[0].source, ItemSource::Native(_)));
        assert!(rendered.queued.is_empty());
        assert_eq!(snapshot.sessions.pending_submissions.len(), 3);
        assert!(Arc::ptr_eq(
            &rendered,
            &project_snapshot(snapshot, Some(&rendered))
        ));
    }

    #[rstest::rstest]
    #[case::sending(false, false, None)]
    #[case::accepted(true, false, None)]
    #[case::unknown(false, true, Some("送信結果不明（自動再送しません）"))]
    #[case::accepted_then_unknown(true, true, Some("送信結果不明（自動再送しません）"))]
    fn pending_input_delivery_status_is_shared_in_history_and_queue(
        #[case] accepted: bool,
        #[case] delivery_unknown: bool,
        #[case] title: Option<&str>,
        #[values(false, true)] history_loaded: bool,
    ) {
        let mut snapshot = fixture();
        let session = snapshot
            .sessions
            .conversations
            .keys()
            .next()
            .unwrap()
            .clone();
        if !history_loaded {
            Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&session)
                    .unwrap(),
            )
            .turns = None;
        }
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: session.into(),
                draft: Arc::new(Draft {
                    text: "waiting for echo".into(),
                    ..Default::default()
                }),
                turn_id: Some("live".into()),
                after_item_id: None,
                accepted,
                delivery_unknown,
            }),
        );
        let rendered = project_snapshot(snapshot, None);
        let item = rendered
            .queued
            .iter()
            .chain(rendered.turns.iter().flat_map(|turn| turn.items()))
            .find(|item| item.data.id == "pending")
            .unwrap();
        assert_eq!(item.data.title.as_deref(), title);
        assert_eq!(item.data.body.as_deref(), Some("waiting for echo"));
        assert_eq!(item.data.timestamp_ms, Some(12_000));
        assert!(item.data.native_id.is_none());
        if history_loaded {
            let row = rendered.turns.iter().flat_map(|turn| &turn.rows)
                .find(|row| matches!(&row.content, ConversationRowContent::User { item } if item.data.id == "pending"))
                .unwrap();
            assert_eq!(row.timestamp_ms, Some(12_000));
        }
    }

    #[test]
    fn local_input_keeps_its_footer_while_acceptance_and_history_are_pending() {
        let mut snapshot = fixture();
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                sequence: 0,
                submitted_at_ms: Some(12_000),
                draft_key: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: "waiting for history".into(),
                    ..Default::default()
                }),
                turn_id: Some("unloaded".into()),
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
            }),
        );
        let sending = project_snapshot(snapshot.clone(), None);
        assert!(sending.queued[0].data.title.is_none());
        assert_eq!(sending.queued[0].data.timestamp_ms, Some(12_000));
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.pending_submissions)
                .get_mut("pending")
                .unwrap(),
        )
        .accepted = true;
        let first = project_snapshot(snapshot.clone(), Some(&sending));
        assert_eq!(first.queued.len(), 1);
        assert!(first.queued[0].data.title.is_none());
        assert_eq!(first.queued[0].data.timestamp_ms, Some(12_000));
        assert_eq!(
            first.queued[0].data.body.as_deref(),
            Some("waiting for history")
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"unloaded","status":"running","items":[{"id":"echo","status":"unknown","clientInputId":"pending","body":{"inline":{"body":{"userMessage":{"text":"waiting for history","content":[]}}}}}]})).unwrap()));
        let loaded = project_snapshot(snapshot, Some(&first));
        assert!(loaded.queued.is_empty());
        assert_eq!(loaded.turns.last().unwrap().items().count(), 1);
        let echoed = loaded.turns.last().unwrap().items().next().unwrap();
        assert_eq!(echoed.data.id, "pending");
        assert!(matches!(&echoed.source, ItemSource::Native(item) if item.id == "echo".into()));
    }

    #[test]
    fn delta_reuses_untouched_turns_and_items_but_invalidates_deferred_details() {
        let snapshot = fixture();
        let first = project_snapshot(snapshot.clone(), None);
        let same = project_snapshot(snapshot.clone(), Some(&first));
        assert!(Arc::ptr_eq(&first, &same));
        let mut updated = snapshot.clone();
        let thread = crate::session::SessionChange::Text {
            turn_id: "live".into(),
            item_id: "stream".into(),
            field: crate::session::TextField::AssistantText,
            delta: " world".into(),
        }
        .apply(
            &snapshot.sessions.conversations[&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }],
        )
        .unwrap();
        Arc::make_mut(&mut updated.sessions.conversations).insert(
            agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            },
            Arc::new(thread),
        );
        let second = project_snapshot(updated.clone(), Some(&first));
        assert!(Arc::ptr_eq(&first.turns[0], &second.turns[0]));
        assert!(!Arc::ptr_eq(&first.turns[1], &second.turns[1]));
        for index in [0, 1] {
            assert!(Arc::ptr_eq(
                first.turns[1].items().nth(index).unwrap(),
                second.turns[1].items().nth(index).unwrap()
            ));
        }
        assert_eq!(
            first.turns[1].items().nth(2).unwrap().data.body.as_deref(),
            Some("hello")
        );
        assert_eq!(
            second.turns[1].items().nth(2).unwrap().data.body.as_deref(),
            Some("hello world")
        );
        let mut deferred = updated;
        let thread = Arc::make_mut(
            Arc::make_mut(&mut deferred.sessions.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        );
        Arc::make_mut(
            &mut Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1])
                .items
                .as_mut()
                .unwrap()[1],
        )
        .defer();
        let third = project_snapshot(deferred, Some(&second));
        assert!(!Arc::ptr_eq(
            second.turns[1].items().nth(1).unwrap(),
            third.turns[1].items().nth(1).unwrap()
        ));
        assert!(third.turns[1].items().nth(1).unwrap().data.deferred);
        assert!(Arc::ptr_eq(
            second.turns[1].items().nth(2).unwrap(),
            third.turns[1].items().nth(2).unwrap()
        ));
    }
    #[test]
    fn requests_and_pending_submissions_have_one_shared_native_projection() {
        let mut snapshot = fixture();
        for value in [
            json!({"id": "1", "target": {"turn": {"turnId": "done", "itemId": null}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "run command", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}}),
            json!({"id": "question", "target": {"turn":{"turnId":"live","itemId":null}}, "delivery": "awaiting", "body": {"question": {"questions": [{"id": "question-0", "header": "", "prompt": "which?", "secret": false, "allowFreeText": true, "multiple": false, "choices": []}]}}}),
            json!({"id": "3", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "fileChange", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}}),
        ] {
            let request: WireRequest = serde_json::from_value(value).unwrap();
            let request = Arc::new(request);
            let session = agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: if request.id.as_str() == "3" {
                    "other"
                } else {
                    "thread"
                }
                .into(),
            };
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .entry(session.clone())
                    .or_insert_with(|| {
                        Arc::new(models::Thread {
                            id: Some(session),
                            ..Default::default()
                        })
                    }),
            );
            thread.requests.insert(request.id.clone(), request);
        }
        let pending = |turn_id| {
            Arc::new(PendingSubmission {
                sequence: 0,
                submitted_at_ms: None,
                draft_key: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: "queued text".into(),
                    ..Default::default()
                }),
                turn_id,
                after_item_id: None,
                accepted: true,
                delivery_unknown: false,
            })
        };
        Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .insert("accepted".into(), pending(Some("live".into())));
        Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .insert("queued".into(), pending(None));
        let rendered = project_snapshot(snapshot, None);
        let done: Vec<_> = rendered.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::PendingRequest { request } => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id.as_str(), "1");
        assert_eq!(done[0].title, "コマンドの承認待ち");
        assert_eq!(done[0].body, "run command");
        assert_eq!(
            done[0]
                .request_body
                .choices()
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["承認", "このセッションで承認", "拒否"]
        );
        let live: Vec<_> = rendered.turns[1]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::PendingRequest { request } => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].body, "which?");
        assert_eq!(
            rendered.turns[1]
                .items()
                .filter(|item| item.data.id == "accepted")
                .count(),
            1
        );
        assert_eq!(rendered.queued.len(), 1);
        assert_eq!(rendered.queued[0].data.body.as_deref(), Some("queued text"));
    }
    #[test]
    fn retry_error_titles_use_host_evidence_without_reinterpreting_native_codes() {
        let error = turn_error(&models::ExecutionError {
            category: models::ErrorCategory::Network,
            message: "retry".into(),
            retry: Some(models::RetryEvidence {
                retrying: true,
                overloaded: true,
            }),
            ..Default::default()
        });
        assert_eq!(error.title, "サーバーが混み合っています。再接続しています");
        assert!(error.is_reconnecting);
        assert_eq!(error.message, "retry");
    }

    #[test]
    fn usage_limit_actions_require_provider_evidence_and_a_finished_retry() {
        for category in [
            models::ErrorCategory::UsageLimit,
            models::ErrorCategory::RateLimited,
            models::ErrorCategory::Other,
        ] {
            let error = turn_error(&models::ExecutionError {
                category: category.clone(),
                message: "usage limit".into(),
                ..Default::default()
            });
            assert_eq!(
                error.is_usage_limit,
                category == models::ErrorCategory::UsageLimit
            );
        }
        let error = turn_error(&models::ExecutionError {
            category: models::ErrorCategory::UsageLimit,
            retry: Some(models::RetryEvidence {
                retrying: true,
                overloaded: false,
            }),
            ..Default::default()
        });
        assert!(!error.is_usage_limit);
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn elapsed_work_uses_provider_time_and_describes_the_current_tool() {
        for value in [
            serde_json::json!({"id":"t","startedAt":100.5}),
            serde_json::json!({"id":"t","startedAtMs":100500}),
            serde_json::json!({"id":"t","startedAt":100.5,"startedAtMs":0}),
        ] {
            let source = Arc::new(serde_json::from_value(value).unwrap());
            let turn = RenderedTurn {
                source,
                pending: Default::default(),
                requests: vec![],
                rows: vec![],
            };
            let mut progress = turn.work_progress(false);
            progress.action = Some("cargo test".into());
            assert_eq!(progress.label(108.5), "8秒 作業中 · cargo test");
            assert_eq!(turn.work_progress(false).label(110.5), "10秒 作業中…");
            assert_eq!(turn.work_progress(false).label(99.), "0秒 作業中…");
        }
        assert_eq!(
            WorkProgress {
                started_at: None,
                action: None
            }
            .label(100.),
            "作業中…"
        );
        assert_eq!(
            WorkProgress {
                started_at: None,
                action: Some(String::new())
            }
            .label(100.),
            "作業中…"
        );
    }

    #[test]
    fn progress_retains_only_the_latest_action_and_start_time() {
        let source = Arc::new(serde_json::from_value(serde_json::json!({
            "id":{"provider":"codex", "id":"fixture"},
            "turns":[{"id":"turn", "status":"running", "startedAt":100.5, "items":[
                {"id":"first", "body":{"inline":{"body":{"commandExecution":{"command":"first", "output":"", "exitCode":0}}}}},
                {"id":"second", "status":"running", "body":{"inline":{"body":{"webSearch":{"query":"latest action"}}}}}
            ]}]
        })).unwrap());
        let conversation = project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            source,
            &None,
        );
        let turn = &conversation.turns[0];
        assert_eq!(
            turn.work_progress(true).label(108.5),
            "8秒 作業中 · Webを検索: latest action"
        );
        assert_eq!(turn.work_progress(false).label(108.5), "8秒 作業中…");
        let progress = turn.work_progress(true);
        drop(conversation);
        assert_eq!(
            progress.label(110.5),
            "10秒 作業中 · Webを検索: latest action"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HistoryControl {
    pub has_more: bool,
    pub loading: bool,
    pub load_label: String,
    pub error: Option<super::error::ErrorText>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn history_control(&self, session: crate::session::SessionRef) -> Option<HistoryControl> {
        let has_more = self
            .sessions
            .conversations
            .get(&session)
            .is_some_and(|thread| thread.history_has_more == Some(true));
        let key = crate::state::operations::OperationKey::History { session };
        let error = self.operation_error(key.clone());
        if !has_more && error.is_none() {
            return None;
        }
        let loading = self.operation_running(key);
        Some(HistoryControl {
            has_more,
            loading,
            load_label: if loading {
                "以前のターンを読み込み中…"
            } else {
                "以前のターンを読み込む"
            }
            .into(),
            error,
        })
    }
}

#[cfg(test)]
mod history_control_tests {
    use super::*;
    use crate::{
        session::{ProviderKind, SessionRef},
        state::operations::{OperationKey, OperationPhase, OperationState},
    };

    #[test]
    fn history_control_tracks_its_own_page_request_and_retry() {
        let session = SessionRef::new(ProviderKind::Codex, "history".into()).unwrap();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let mut snapshot = Snapshot::default();
        assert_eq!(snapshot.history_control(session.clone()), None);
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(models::Thread {
                history_has_more: Some(true),
                ..Default::default()
            }),
        );
        let idle = HistoryControl {
            has_more: true,
            loading: false,
            load_label: "以前のターンを読み込む".into(),
            error: None,
        };
        assert_eq!(
            snapshot.history_control(session.clone()),
            Some(idle.clone())
        );
        let key = OperationKey::History {
            session: session.clone(),
        };
        for (phase, expected) in [
            (OperationPhase::Running, HistoryControl {
                loading: true,
                load_label: "以前のターンを読み込み中…".into(),
                ..idle.clone()
            }),
            (OperationPhase::Failed {
                message: r#"remote RPC error: {"code":"provider_failed","message":"Older page unavailable"}"#.into(),
            }, HistoryControl {
                error: Some(crate::presentation::error::ErrorText {
                    message: "ホストで操作に失敗しました。".into(),
                    details: Some("Older page unavailable".into()),
                }),
                ..idle.clone()
            }),
            (OperationPhase::Running, HistoryControl {
                loading: true,
                load_label: "以前のターンを読み込み中…".into(),
                ..idle.clone()
            }),
        ] {
            snapshot.operations.insert(key.clone(), OperationState {
                generation: 1,
                phase,
            });
            assert_eq!(snapshot.history_control(session.clone()), Some(expected));
            assert_eq!(snapshot.history_control(other.clone()), None);
        }
        snapshot.operations = Default::default();
        assert_eq!(snapshot.history_control(session), Some(idle));
    }

    #[rstest::rstest]
    #[case(None)]
    #[case(Some(false))]
    fn no_continuation_hides_the_button_but_preserves_a_read_failure(#[case] more: Option<bool>) {
        let session = SessionRef::new(ProviderKind::Codex, "history".into()).unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(models::Thread {
                history_has_more: more,
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.history_control(session.clone()), None);
        snapshot.operations.insert(
            OperationKey::History {
                session: session.clone(),
            },
            OperationState {
                generation: 1,
                phase: OperationPhase::Failed {
                    message: "History unavailable".into(),
                },
            },
        );
        assert_eq!(
            snapshot.history_control(session),
            Some(HistoryControl {
                has_more: false,
                loading: false,
                load_label: "以前のターンを読み込む".into(),
                error: Some(crate::presentation::error::ErrorText {
                    message: "エラーが発生しました。".into(),
                    details: Some("History unavailable".into()),
                }),
            })
        );
    }
}

/// Shared read-state wording; native views only render this projection.
pub fn history_notice(thread: &models::Thread) -> Option<String> {
    use crate::session::HistoryReadKind;
    let state = thread.history_read_state.as_ref()?;
    let heading = match state.kind {
        // Older pages are loaded on demand. A usable continuation is normal,
        // while missing history or reported read issues still need a notice.
        HistoryReadKind::Partial
            if state.issues.is_empty()
                && thread.history_has_more == Some(true)
                && thread
                    .history_cursor
                    .as_ref()
                    .is_some_and(|cursor| !cursor.is_empty()) =>
        {
            return None;
        }
        HistoryReadKind::Partial => "履歴の一部を表示しています。",
        HistoryReadKind::Incomplete => "履歴の一部を読み取れませんでした。",
        HistoryReadKind::Unavailable => {
            "履歴を取得できません。保存済みの表示は最新とは限りません。"
        }
        HistoryReadKind::Complete => return None,
    };
    let issues = state
        .issues
        .iter()
        .take(8)
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(if issues.is_empty() {
        heading.into()
    } else {
        format!("{heading}\n{issues}")
    })
}

#[cfg(test)]
mod history_notice_tests {
    use super::*;
    use crate::session::{HistoryPage, HistoryReadKind, HistoryReadState};

    #[test]
    fn history_pages_keep_source_errors_visible_and_can_clear_them_after_recovery() {
        let mut thread = models::Thread::default();
        HistoryPage {
            turns: Vec::new(),
            next_cursor: None,
            history_read_state: Some(HistoryReadState::new(
                HistoryReadKind::Incomplete,
                vec!["claude: Claude failed to start".into()],
            )),
        }
        .prepend_to(&mut thread);
        assert_eq!(
            history_notice(&thread).as_deref(),
            Some("履歴の一部を読み取れませんでした。\nclaude: Claude failed to start")
        );
        HistoryPage {
            turns: Vec::new(),
            next_cursor: None,
            history_read_state: Some(HistoryReadState::new(HistoryReadKind::Complete, Vec::new())),
        }
        .prepend_to(&mut thread);
        assert_eq!(history_notice(&thread), None);
    }

    #[test]
    fn normal_pagination_has_no_warning_before_or_after_loading_the_remaining_history() {
        let mut thread = models::Thread::default();
        HistoryPage {
            history_read_state: None,
            turns: Vec::new(),
            next_cursor: Some("older-page".into()),
        }
        .prepend_to(&mut thread);
        assert_eq!(history_notice(&thread), None);
        HistoryPage {
            history_read_state: None,
            turns: Vec::new(),
            next_cursor: None,
        }
        .prepend_to(&mut thread);
        assert_eq!(history_notice(&thread), None);
    }

    #[rstest::rstest]
    #[case(None, None)]
    #[case(None, Some(""))]
    #[case(None, Some("older-page"))]
    #[case(Some(false), None)]
    #[case(Some(false), Some(""))]
    #[case(Some(false), Some("older-page"))]
    #[case(Some(true), None)]
    #[case(Some(true), Some(""))]
    fn a_partial_read_without_a_usable_continuation_keeps_its_notice(
        #[case] more: Option<bool>,
        #[case] cursor: Option<&str>,
    ) {
        let thread = models::Thread {
            history_has_more: more,
            history_cursor: cursor.map(str::to_owned),
            history_read_state: Some(HistoryReadState::new(HistoryReadKind::Partial, Vec::new())),
            ..Default::default()
        };
        assert_eq!(
            history_notice(&thread).as_deref(),
            Some("履歴の一部を表示しています。")
        );
    }

    #[rstest::rstest]
    #[case(HistoryReadKind::Partial, "履歴の一部を表示しています。")]
    #[case(HistoryReadKind::Incomplete, "履歴の一部を読み取れませんでした。")]
    #[case(
        HistoryReadKind::Unavailable,
        "履歴を取得できません。保存済みの表示は最新とは限りません。"
    )]
    fn read_failures_remain_visible_even_when_older_pages_exist(
        #[case] kind: HistoryReadKind,
        #[case] heading: &str,
    ) {
        let thread = models::Thread {
            history_has_more: Some(true),
            history_cursor: Some("older-page".into()),
            history_read_state: Some(HistoryReadState::new(kind, vec!["read issue".into()])),
            ..Default::default()
        };
        assert_eq!(
            history_notice(&thread),
            Some(format!("{heading}\nread issue"))
        );
    }
}
