//! Immutable conversation projection shared by desktop, Swift and Kotlin.
use super::{ItemMetadata, Role, body, item_presentation, project_items, source_order};
use crate::{
    models,
    state::{PendingSubmission, Snapshot},
};
use agent_protocol::requests::Request as WireRequest;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

impl Snapshot {
    /// Display pending input before a new conversation has a server ID.
    pub fn conversation_thread(&self) -> Option<Arc<models::Thread>> {
        if let Some(source) = self
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
        {
            return Some(source.clone());
        }
        self.pending_submissions
            .values()
            .any(|pending| pending.draft_key == self.navigation.draft_key)
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
    pub request_rows: Vec<ConversationRow>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueMessage {
    pub id: agent_protocol::ids::ClientInputId,
    pub text: String,
    pub images: Vec<String>,
    pub delivery: agent_protocol::session::SubmissionDelivery,
    pub editable: bool,
    pub removable: bool,
    pub status: String,
    pub move_up: Option<agent_protocol::queue::QueueAction>,
    pub move_down: Option<agent_protocol::queue::QueueAction>,
}

pub fn queue_messages(entries: &[agent_protocol::queue::QueueEntry]) -> Vec<QueueMessage> {
    use agent_protocol::operations::Input;
    use agent_protocol::{queue::QueueAction, session::SubmissionDelivery};
    let waiting: Vec<_> = entries
        .iter()
        .filter(|entry| entry.delivery == SubmissionDelivery::Queued)
        .map(|entry| &entry.submission.client_user_message_id)
        .collect();
    entries
        .iter()
        .map(|entry| {
            let id = &entry.submission.client_user_message_id;
            let position = waiting.iter().position(|value| *value == id);
            QueueMessage {
                id: id.clone(),
                text: entry
                    .submission
                    .input
                    .iter()
                    .filter_map(|part| match part {
                        Input::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                images: entry
                    .submission
                    .input
                    .iter()
                    .filter_map(|part| match part {
                        Input::LocalImage { path } => Some(path.clone()),
                        _ => None,
                    })
                    .collect(),
                delivery: entry.delivery.clone(),
                editable: entry.delivery == SubmissionDelivery::Queued,
                removable: !matches!(
                    entry.delivery,
                    SubmissionDelivery::Sending | SubmissionDelivery::Accepted { .. }
                ),
                status: match entry.delivery {
                    SubmissionDelivery::Queued => "待機中",
                    SubmissionDelivery::Sending => "送信中",
                    SubmissionDelivery::Accepted { .. } => "送信済み",
                    SubmissionDelivery::Unknown => {
                        "送信結果を確認できません。自動では再送しません。"
                    }
                    SubmissionDelivery::Rejected => "送信失敗",
                }
                .into(),
                move_up: position.filter(|position| *position > 0).map(|position| {
                    QueueAction::Move {
                        id: id.clone(),
                        before: Some(waiting[position - 1].clone()),
                    }
                }),
                move_down: position
                    .filter(|position| position + 1 < waiting.len())
                    .map(|position| QueueAction::Move {
                        id: id.clone(),
                        before: waiting.get(position + 2).map(|id| (*id).clone()),
                    }),
            }
        })
        .collect()
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
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationRowContent {
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
        fork_turn_id: Option<agent_protocol::ids::TurnId>,
    },
    InProgress {
        turn_id: agent_protocol::ids::TurnId,
    },
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityPresentation {
    pub id: String,
    pub status: String,
    pub activity_summary: String,
    pub activity_initially_expanded: bool,
    pub activity_can_collapse: bool,
    pub is_in_progress: bool,
    pub load_items: Option<crate::state::operations::LoadTurnItems>,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityExpansion {
    pub status: String,
    pub expanded: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn activity_is_expanded(
    activity: &ActivityPresentation,
    choice: Option<ActivityExpansion>,
) -> bool {
    choice
        .filter(|choice| choice.status == activity.status)
        .map_or(activity.activity_initially_expanded, |choice| {
            choice.expanded
        })
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

    pub fn progress_label(&self, include_action: bool, now_seconds: f64) -> String {
        let action = self.rows.iter().rev().find_map(|row| match &row.content {
            ConversationRowContent::Activity { item, .. } if include_action => {
                item.data.title.as_deref()
            }
            _ => None,
        });
        progress_label(&self.source, action, now_seconds)
    }
}

impl RenderedTurn {
    fn items(&self) -> impl Iterator<Item = &Arc<RenderedItem>> {
        self.rows.iter().filter_map(|row| match &row.content {
            ConversationRowContent::User { item }
            | ConversationRowContent::Activity { item, .. }
            | ConversationRowContent::Response { item, .. } => Some(item),
            _ => None,
        })
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TurnErrorPresentation {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub is_reconnecting: bool,
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
        previous: Option<&Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = previous
            && previous.data.deferred == deferred
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
                kind: "user".into(),
                title: Some(pending.delivery_label().into()),
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
    snapshot: &Snapshot,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let draft_key = source
        .id
        .as_ref()
        .map(crate::state::DraftKey::from)
        .unwrap_or_else(|| snapshot.navigation.draft_key.clone());
    let mut pending: PendingItems = snapshot
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
        .map(|(id, pending)| (id.clone(), pending.clone()))
        .collect();
    pending.sort_by_key(|(_, pending)| pending.sequence);
    let requests = &source.requests;
    if let Some(previous) = previous
        && Arc::ptr_eq(&source, &previous.source)
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
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.turns)
        .map(|turn| (turn.source.id.as_str(), turn))
        .collect();
    let native = source.turns.as_deref().unwrap_or_default();
    // A submission made before any history belongs before the first turn once
    // it arrives. An acknowledged queue entry still waits for its assigned turn.
    let pending_turn = |pending: &PendingSubmission| match pending.turn_id.as_deref() {
        Some(id) => native.iter().rposition(|turn| turn.id.as_str() == id),
        None if !pending.accepted && !native.is_empty() => Some(0),
        None => None,
    };
    let turns = native
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let pending = pending
                .iter()
                .filter(|(_, p)| pending_turn(p) == Some(index));
            let requests = requests.values().filter(|r| matches!(&r.target, agent_protocol::requests::RequestTarget::Turn { turn_id, .. } if turn_id == &turn.id));
            let old = cached.get(turn.id.as_str()).copied();
            if let Some(old) = old
                && Arc::ptr_eq(turn, &old.source)
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
                source.capabilities.unwrap_or_default().fork,
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
    let queued = pending
        .iter()
        .filter(|(_, p)| pending_turn(p).is_none())
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
    let request_rows = requests
        .values()
        .filter(|request| match &request.target {
            agent_protocol::requests::RequestTarget::Session => true,
            agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => {
                !native.iter().any(|turn| &turn.id == turn_id)
            }
        })
        .map(|source| ConversationRow {
            id: format!("request:{}", source.id),
            content: ConversationRowContent::PendingRequest {
                request: Box::new(request(source)),
            },
        })
        .collect();
    Arc::new(RenderedConversation {
        source,
        pending,
        turns,
        queued,
        request_rows,
    })
}

fn render_turn(
    session: Option<&crate::session::SessionRef>,
    supports_fork: bool,
    source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    previous: Option<&Arc<RenderedTurn>>,
) -> Arc<RenderedTurn> {
    let provider = session.map(|session| session.provider);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|turn| turn.items())
        .map(|item| (item.key(), item))
        .collect();
    let native = source.items.as_deref().unwrap_or_default();
    // Snapshot keys already make pending IDs unique.
    let retained: Vec<_> = pending
        .iter()
        .filter(|(id, _)| {
            !native
                .iter()
                .any(|item| item.client_input_id.as_ref() == Some(id))
        })
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
            User { item } | Activity { item, .. } | Response { item, .. } => {
                format!("history-item:{}:{}", source.id, item.data.id)
            }
            ActivityHeader { activity } => activity.id.clone(),
            PendingRequest { request } => format!("history-request:{}", request.id),
            Error { .. } => format!("history-error:{}", source.id),
            InProgress { turn_id } => format!("in-progress:{turn_id}"),
        };
        let occurrence = occurrences.entry(id.clone()).or_insert(0usize);
        let id = format!("{id}:occurrence:{occurrence}");
        *occurrence += 1;
        rows.push(ConversationRow { id, content });
    };
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
                    activity_initially_expanded: segment.initially_expanded
                        && !source.items_summary,
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
            let can_fork =
                supports_fork && !in_progress && segment.last && responses.peek().is_none();
            if item.data.deferred {
                push(Activity {
                    item,
                    turn_id: source.id.clone(),
                });
            } else {
                push(Response {
                    item,
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
        rows,
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
    }
}

pub type PendingItems = Vec<(agent_protocol::ids::ClientInputId, Arc<PendingSubmission>)>;
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ItemPresentation {
    pub id: String,
    pub native_id: Option<agent_protocol::ids::ItemId>,
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
    #[test]
    fn queue_controls_move_only_waiting_inputs_and_preserve_uncertain_text() {
        use agent_protocol::{
            operations::Submission,
            queue::{QueueAction, QueueEntry},
            session::{ProviderKind, SessionRef, SubmissionDelivery},
        };
        let entries: Vec<_> = [
            SubmissionDelivery::Queued,
            SubmissionDelivery::Unknown,
            SubmissionDelivery::Sending,
            SubmissionDelivery::Queued,
            SubmissionDelivery::Queued,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, delivery)| QueueEntry {
            submission: Submission {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "conversation".into(),
                },
                client_user_message_id: index.to_string().into(),
                input: vec![
                    agent_protocol::operations::Input::Text {
                        text: format!("message {index}"),
                    },
                    agent_protocol::operations::Input::LocalImage {
                        path: format!("/isolated/image-{index}.png"),
                    },
                ],
                model: None,
                effort: None,
                service_tier: None,
            },
            delivery,
        })
        .collect();
        let messages = super::queue_messages(&entries);
        assert_eq!(
            messages[0].move_down,
            Some(QueueAction::Move {
                id: "0".into(),
                before: Some("4".into())
            })
        );
        assert_eq!(
            messages[3].move_down,
            Some(QueueAction::Move {
                id: "3".into(),
                before: None
            })
        );
        assert_eq!(
            messages[4].move_up,
            Some(QueueAction::Move {
                id: "4".into(),
                before: Some("3".into())
            })
        );
        assert!(messages[0].move_up.is_none());
        assert!(messages[4].move_down.is_none());
        assert_eq!(
            messages[3].move_up,
            Some(QueueAction::Move {
                id: "3".into(),
                before: Some("0".into())
            })
        );
        assert_eq!(messages[3].images, ["/isolated/image-3.png"]);
        assert!(!messages[1].editable && messages[1].removable);
        assert!(!messages[2].editable && !messages[2].removable);
        assert_eq!(messages[1].text, "message 1");
        assert!(messages[1].move_up.is_none() && messages[1].move_down.is_none());
    }
    use super::*;
    use crate::state::{Draft, Snapshot};
    use serde_json::json;
    use std::collections::HashSet;

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
        }
    }
    fn project_snapshot(
        snapshot: Snapshot,
        previous: Option<&Arc<RenderedConversation>>,
    ) -> Arc<RenderedConversation> {
        project_conversation(
            &snapshot,
            snapshot.conversations[&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }]
                .clone(),
            &previous.cloned(),
        )
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
        let awaiting =
            project_conversation(&Snapshot::default(), make(RequestDelivery::Awaiting), &None);
        assert!(awaiting.turns.is_empty());
        assert_eq!(awaiting.request_rows.len(), 1);
        let ConversationRowContent::PendingRequest { request } = &awaiting.request_rows[0].content
        else {
            panic!("request row")
        };
        assert!(request.can_respond);
        let sent = project_conversation(
            &Snapshot::default(),
            make(RequestDelivery::Sent),
            &Some(awaiting.clone()),
        );
        let ConversationRowContent::PendingRequest { request } = &sent.request_rows[0].content
        else {
            panic!("request row")
        };
        assert!(!request.can_respond);
        assert_ne!(
            &request.title,
            match &awaiting.request_rows[0].content {
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
            Arc::make_mut(&mut snapshot.conversations)
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
            Arc::make_mut(&mut snapshot.conversations)
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
            matches!(&rows.last().unwrap().content, ConversationRowContent::Response { item, fork_turn_id: Some(id) } if id.as_str() == "live" && item.data.native_id.as_deref() == Some("stream"))
        );
        assert!(rows.iter().any(|row| row.id == accepted_id));
        for row in rows {
            if let ConversationRowContent::ActivityHeader { activity } = row.content {
                assert!(!activity_is_expanded(&activity, None));
                assert!(activity_is_expanded(
                    &activity,
                    Some(ActivityExpansion {
                        status: activity.status.clone(),
                        expanded: true
                    })
                ));
                assert!(!activity_is_expanded(
                    &activity,
                    Some(ActivityExpansion {
                        status: "running".into(),
                        expanded: true
                    })
                ));
            }
        }
    }

    #[test]
    fn flat_row_ids_distinguish_repeated_items_and_turns() {
        let source = Arc::new(
            serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"first","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old","phase":"unknown"}}}}},{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"final"}}}}}]},{"id":"second","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"another turn","phase":"unknown"}}}}}]}]}))
            .unwrap(),
        );
        let rendered = project_conversation(&Snapshot::default(), source, &None);
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
        let updated = project_conversation(&Snapshot::default(), changed, &Some(rendered.clone()));
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
        let rendered = project_conversation(&Snapshot::default(), source, &None);
        let rows = &rendered.turns[0].rows;
        assert!(matches!(
            &rows[0].content,
            ConversationRowContent::User { .. }
        ));
        let ConversationRowContent::ActivityHeader { activity } = &rows[1].content else {
            panic!("summary activity is inaccessible")
        };
        assert!(!activity.activity_initially_expanded);
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
            };
            // IDs deliberately disagree with submission order.
            for id in ["z-first", "a-second", "m-third"] {
                Arc::make_mut(&mut snapshot.drafts).insert(
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
                Arc::make_mut(&mut snapshot.conversations)
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
            Arc::make_mut(&mut snapshot.conversations)
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
            Arc::make_mut(&mut snapshot.pending_submissions).insert(
                id.into(),
                Arc::new(PendingSubmission {
                    sequence: sequence as u64,
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
        Arc::make_mut(Arc::make_mut(&mut snapshot.conversations).get_mut(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }).unwrap())
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
        let activity_ids = rendered
            .turns
            .last()
            .unwrap()
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::ActivityHeader { activity } => Some(activity.id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(activity_ids, ["echo-turn:c"]);
        assert!(rendered.queued.is_empty());
        assert_eq!(snapshot.pending_submissions.len(), 3);
        assert!(Arc::ptr_eq(
            &rendered,
            &project_snapshot(snapshot, Some(&rendered))
        ));
    }

    #[test]
    fn pending_input_remains_visible_until_its_turn_is_loaded() {
        let mut snapshot = fixture();
        Arc::make_mut(&mut snapshot.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                sequence: 0,
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
                accepted: true,
                delivery_unknown: false,
            }),
        );
        let first = project_snapshot(snapshot.clone(), None);
        assert_eq!(first.queued.len(), 1);
        assert_eq!(
            first.queued[0].data.body.as_deref(),
            Some("waiting for history")
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.conversations)
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
            &snapshot.conversations[&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }],
        )
        .unwrap();
        Arc::make_mut(&mut updated.conversations).insert(
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
            Arc::make_mut(&mut deferred.conversations)
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
            json!({"id": "1", "target": {"turn": {"turnId": "done", "itemId": null}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "run command", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": ""}, {"id": "choice-1", "label": "このセッションで承認", "description": ""}, {"id": "choice-2", "label": "拒否", "description": ""}, {"id": "choice-3", "label": "キャンセル", "description": ""}]}}}),
            json!({"id": "question", "target": {"turn":{"turnId":"live","itemId":null}}, "delivery": "awaiting", "body": {"question": {"questions": [{"id": "question-0", "header": "", "prompt": "which?", "secret": false, "allowFreeText": true, "multiple": false, "choices": []}]}}}),
            json!({"id": "3", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "fileChange", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": ""}, {"id": "choice-1", "label": "このセッションで承認", "description": ""}, {"id": "choice-2", "label": "拒否", "description": ""}, {"id": "choice-3", "label": "キャンセル", "description": ""}]}}}),
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
                Arc::make_mut(&mut snapshot.conversations)
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
        Arc::make_mut(&mut snapshot.pending_submissions)
            .insert("accepted".into(), pending(Some("live".into())));
        Arc::make_mut(&mut snapshot.pending_submissions).insert("queued".into(), pending(None));
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
            ["承認", "このセッションで承認", "拒否", "キャンセル"]
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
}

fn progress_label(turn: &models::Turn, action: Option<&str>, now_seconds: f64) -> String {
    let started = turn.started_at.as_ref().copied().or_else(|| {
        turn.started_at_ms
            .map(|milliseconds| milliseconds as f64 / 1000.)
    });
    let elapsed = started
        .map(|started| format!("{}秒 ", (now_seconds - started).max(0.) as u64))
        .unwrap_or_default();
    match action.filter(|action| !action.is_empty()) {
        Some(action) => format!("{elapsed}作業中 · {action}"),
        None => format!("{elapsed}作業中…"),
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
        ] {
            let turn = serde_json::from_value(value).unwrap();
            assert_eq!(
                progress_label(&turn, Some("cargo test"), 108.5),
                "8秒 作業中 · cargo test"
            );
            assert_eq!(progress_label(&turn, None, 110.5), "10秒 作業中…");
            assert_eq!(progress_label(&turn, None, 99.), "0秒 作業中…");
        }
        assert_eq!(
            progress_label(&models::Turn::default(), None, 100.),
            "作業中…"
        );
    }
}

/// Shared read-state wording; native views only render this projection.
pub fn can_retry_history(thread: &models::Thread) -> bool {
    thread.history_read_state.as_ref().is_some_and(|state| {
        matches!(
            state.kind,
            crate::session::HistoryReadKind::Incomplete
                | crate::session::HistoryReadKind::Unavailable
        )
    })
}

pub fn history_notice(thread: &models::Thread) -> Option<String> {
    use crate::session::HistoryReadKind;
    let state = thread.history_read_state.as_ref()?;
    let heading = match state.kind {
        HistoryReadKind::Importing => "既存の履歴を取り込んでいます。",
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
