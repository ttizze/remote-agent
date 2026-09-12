//! Immutable conversation projection shared by desktop, Swift and Kotlin.
use super::{
    ItemMetadata, Role, body, item_presentation, project_items, remaining_submissions, source_order,
};
use crate::{
    client::ServerRequest,
    models,
    state::{PendingSubmission, Snapshot},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedConversation {
    pub source: Arc<models::Thread>,
    pending: PendingItems,
    requests: Arc<BTreeMap<String, Arc<ServerRequest>>>,
    pub turns: Vec<Arc<RenderedTurn>>,
    pub queued: Vec<Arc<RenderedItem>>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedTurn {
    pub source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<ServerRequest>>,
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
    OlderItems {
        turn_id: String,
    },
    User {
        item: Arc<RenderedItem>,
    },
    ActivityHeader {
        activity: ActivityPresentation,
    },
    Activity {
        item: Arc<RenderedItem>,
        turn_id: String,
    },
    PendingRequest {
        request: Box<Request>,
    },
    Error {
        error: TurnErrorPresentation,
    },
    Response {
        item: Arc<RenderedItem>,
        fork_turn_id: Option<String>,
    },
    InProgress {
        turn_id: String,
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedTurn {
    pub fn conversation_rows(&self) -> Vec<ConversationRow> {
        self.rows.clone()
    }

    pub fn progress_label(&self, include_action: bool, now_seconds: f64) -> String {
        let action = self.rows.iter().rev().find_map(|row| match &row.content {
            ConversationRowContent::Activity { item, .. } if include_action => {
                Some(item.data.title.as_str())
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
            ItemSource::Pending(..) => self.data.body.clone(),
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
    fn native(item: &Arc<models::Item>, deferred: bool, previous: Option<&Arc<Self>>) -> Arc<Self> {
        if let Some(previous) = previous
            && previous.data.deferred == deferred
        {
            return previous.clone();
        }
        let presentation = item_presentation(item);
        let body = body::item_body(item, &presentation);
        Arc::new(Self {
            source: ItemSource::Native(item.clone()),
            data: ItemPresentation {
                id: item.client_id.as_ref().unwrap_or(&item.id).clone(),
                native_id: Some(item.id.clone()),
                kind: presentation.kind.into(),
                title: presentation.title,
                collapsible: presentation.collapsible,
                visible: presentation.visible,
                body: body.text,
                image_sources: body.images,
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
                title: "You".into(),
                collapsible: false,
                visible: true,
                body: body.text,
                image_sources: body.images,
                deferred: false,
            },
        })
    }
}

/// Pass the previous projection to retain native render identities across deltas.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn project_conversation(
    snapshot: &Snapshot,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let pending = snapshot
        .pending_submissions
        .iter()
        .filter(|(_, pending)| source.id.as_deref() == Some(pending.draft_key.as_str()));
    let requests = &snapshot.requests;
    if let Some(previous) = previous
        && Arc::ptr_eq(&source, &previous.source)
        && pending
            .clone()
            .map(|(id, pending)| (id, Arc::as_ptr(pending)))
            .eq(previous
                .pending
                .iter()
                .map(|(id, pending)| (id, Arc::as_ptr(pending))))
        && Arc::ptr_eq(requests, &previous.requests)
    {
        return previous.clone();
    }
    let pending: PendingItems = pending
        .map(|(id, pending)| (id.clone(), pending.clone()))
        .collect();
    let previous = previous.as_ref().filter(|old| source.id == old.source.id);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.turns)
        .map(|turn| (turn.source.id.as_str(), turn))
        .collect();
    let native = source.turns.as_deref().unwrap_or_default();
    let last_turn = native.last().map(|turn| turn.id.as_str());
    let turns = native
        .iter()
        .map(|turn| {
            let pending = pending
                .iter()
                .filter(|(_, p)| p.turn_id.as_deref() == Some(&turn.id));
            let requests =
                requests.values().filter(|r| {
                    source.id.as_deref().is_some_and(|id| {
                        r.params.get("threadId").and_then(Value::as_str) == Some(id)
                    }) && r.params.get("turnId").and_then(Value::as_str).or(last_turn)
                        == Some(&turn.id)
                });
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
        .filter(|(_, p)| {
            p.turn_id
                .as_ref()
                .is_none_or(|id| !native.iter().any(|turn| &turn.id == id))
        })
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
    Arc::new(RenderedConversation {
        source,
        pending,
        requests: requests.clone(),
        turns,
        queued,
    })
}

fn render_turn(
    source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<ServerRequest>>,
    previous: Option<&Arc<RenderedTurn>>,
) -> Arc<RenderedTurn> {
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|turn| turn.items())
        .map(|item| (item.key(), item))
        .collect();
    let deferred: HashSet<_> = source
        .deferred_item_ids
        .iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let native = source.items.as_deref().unwrap_or_default();
    let ids: Vec<_> = pending.iter().map(|(id, _)| id.as_str()).collect();
    let retained = remaining_submissions(
        &ids,
        native.iter().filter_map(|item| item.client_id.as_deref()),
    );
    let anchors: Vec<_> = retained
        .iter()
        .map(|&index| pending[index].1.after_item_id.as_deref())
        .collect();
    let order = source_order(native.len(), |index| native[index].id.as_str(), &anchors);
    let metadata = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            ItemMetadata::from(item.as_ref())
        } else {
            let id = pending[retained[index - native.len()]].0.as_str();
            ItemMetadata {
                id,
                client_id: Some(id),
                kind: "userMessage",
                ..Default::default()
            }
        }
    };
    let render_native = |item: &Arc<models::Item>| {
        RenderedItem::native(
            item,
            deferred.contains(item.id.as_str()),
            cached.get(&(None, Arc::as_ptr(item).cast())).copied(),
        )
    };
    let render = |index: usize| {
        let index = order[index];
        if let Some(item) = native.get(index) {
            render_native(item)
        } else {
            let (id, submission) = &pending[retained[index - native.len()]];
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
            PendingRequest { request } => format!("history-request:{}", request.key),
            OlderItems { turn_id } => format!("history-gap:{turn_id}"),
            Error { .. } => format!("history-error:{}", source.id),
            InProgress { turn_id } => format!("in-progress:{turn_id}"),
        };
        let occurrence = occurrences.entry(id.clone()).or_insert(0usize);
        let id = format!("{id}:occurrence:{occurrence}");
        *occurrence += 1;
        rows.push(ConversationRow { id, content });
    };
    if let Some(item) = source
        .opening_user_message
        .as_ref()
        .filter(|item| !native.iter().any(|native| native.id == item.id))
    {
        push(User {
            item: render_native(item),
        });
    }
    if source.items_has_more.unwrap_or(false) {
        push(OlderItems {
            turn_id: source.id.clone(),
        });
    }
    for segment in project_items(&source, order.len(), metadata) {
        let segment = &segment;
        let group = |role| {
            (segment.start..segment.end)
                .filter(move |&index| segment.role(index, metadata(index)) == role)
                .map(&render)
        };
        let in_progress = segment.last && source.status.as_deref() == Some("inProgress");
        for item in group(Role::User) {
            push(User { item });
        }
        if let Some(summary) = &segment.label {
            push(ActivityHeader {
                activity: ActivityPresentation {
                    id: segment.id.clone(),
                    status: source.status.clone().unwrap_or_default(),
                    activity_summary: summary.clone(),
                    activity_initially_expanded: segment.initially_expanded,
                    activity_can_collapse: segment.collapsible,
                    is_in_progress: in_progress,
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
                    request: Box::new(request(&pending.id.to_string(), pending)),
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
            let can_fork = !in_progress && segment.last && responses.peek().is_none();
            push(Response {
                item,
                fork_turn_id: can_fork.then(|| source.id.clone()),
            });
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

pub fn request(key: &str, source: &ServerRequest) -> Request {
    let (kind, title) = match source.method.as_str() {
        "item/commandExecution/requestApproval" => {
            (RequestKind::CommandApproval, "コマンドの承認待ち")
        }
        "item/fileChange/requestApproval" => (RequestKind::FileApproval, "ファイル変更の承認待ち"),
        "item/permissions/requestApproval" => (RequestKind::Permissions, "権限の承認待ち"),
        "item/tool/requestUserInput" => (RequestKind::Questions, "回答待ち"),
        "mcpServer/elicitation/request" => (RequestKind::Elicitation, "MCPからの入力待ち"),
        "item/tool/call" => (RequestKind::Tool, "ツールの入力待ち"),
        _ => (RequestKind::Other, "Codexからの確認待ち"),
    };
    let decisions = crate::client::approval_decisions(source);
    let decision_labels = decisions
        .iter()
        .map(|value| match value.as_str() {
            Some("accept") => "承認".into(),
            Some("acceptForSession") => "このセッションで承認".into(),
            Some("decline") => "拒否".into(),
            Some("cancel") => "キャンセル".into(),
            _ => serde_json::to_string_pretty(value).expect("Value serializes"),
        })
        .collect();
    let params = &source.params;
    let body = params
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|q| q.first())
        .and_then(|q| q["question"].as_str())
        .or_else(|| params.get("reason").and_then(Value::as_str))
        .or_else(|| params.get("message").and_then(Value::as_str))
        .or_else(|| params.get("prompt").and_then(Value::as_str))
        .unwrap_or("操作を続けるには応答が必要です")
        .into();
    Request {
        id: source.id.clone(),
        key: key.into(),
        method: source.method.clone(),
        kind,
        title: title.into(),
        body,
        decision_labels,
        decisions: decisions.to_vec(),
        params: params.clone(),
    }
}
fn turn_error(value: &Value) -> TurnErrorPresentation {
    let info = &value["codexErrorInfo"];
    let kind = info
        .as_str()
        .or_else(|| {
            info.as_object()
                .and_then(|fields| fields.keys().min().map(String::as_str))
        })
        .unwrap_or_default();
    let retrying = value["willRetry"].as_bool().unwrap_or(false);
    let status = &info[kind]["httpStatusCode"];
    let overloaded = kind == "serverOverloaded"
        || status == 429
        || status == 503
        || status == "429"
        || status == "503";
    let title = if retrying {
        if overloaded {
            "サーバーが混み合っています。再接続しています"
        } else {
            "再接続しています"
        }
    } else {
        match kind {
            "contextWindowExceeded" => "コンテキストの上限に達しました",
            "sessionBudgetExceeded" => "セッションの上限に達しました",
            "usageLimitExceeded" => "利用上限に達しました",
            "serverOverloaded" => "サーバーが混み合っています",
            "cyberPolicy" | "misalignmentPolicyViolation" => "安全ポリシーにより停止しました",
            "internalServerError" => "サーバーエラー",
            "unauthorized" => "認証が必要です",
            "badRequest" => "リクエストを処理できません",
            "threadRollbackFailed" => "タスクを元に戻せませんでした",
            "sandboxError" => "サンドボックスエラー",
            "activeTurnNotSteerable" => "この作業中はメッセージを追加できません",
            "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts" => "接続エラー",
            _ => "エラー",
        }
    };
    TurnErrorPresentation {
        title: title.into(),
        message: value["message"]
            .as_str()
            .or_else(|| value.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(value).expect("Value serializes")),
        details: value["additionalDetails"].as_str().map(str::to_owned),
        is_reconnecting: retrying,
    }
}

pub type PendingItems = Vec<(String, Arc<PendingSubmission>)>;
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ItemPresentation {
    pub id: String,
    pub native_id: Option<String>,
    pub body: String,
    pub image_sources: Vec<String>,
    pub deferred: bool,
    pub kind: String,
    pub title: String,
    pub collapsible: bool,
    pub visible: bool,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Request {
    pub id: Value,
    pub key: String,
    pub method: String,
    pub kind: RequestKind,
    pub title: String,
    pub body: String,
    pub decision_labels: Vec<String>,
    pub decisions: Vec<Value>,
    pub params: serde_json::Map<String, Value>,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RequestKind {
    CommandApproval,
    FileApproval,
    Permissions,
    Questions,
    Elicitation,
    Tool,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Draft, Event, Snapshot, reduce};
    use serde_json::json;

    fn fixture() -> Snapshot {
        let thread = serde_json::from_value(json!({"id":"thread","turns":[
            {"id":"done","status":"completed","items":[{"id":"answer","type":"agentMessage","text":"earlier"}]},
            {"id":"live","status":"inProgress","items":[
                {"id":"user","type":"userMessage","clientId":"accepted","text":"question"},
                {"id":"command","type":"commandExecution","command":"pwd","status":"completed","aggregatedOutput":"/fixture"},
                {"id":"stream","type":"agentMessage","text":"hello"}
            ]}
        ]})).unwrap();
        Snapshot {
            conversations: Arc::new([("thread".into(), Arc::new(thread))].into()),
            ..Default::default()
        }
    }
    fn project_snapshot(
        snapshot: Snapshot,
        previous: Option<&Arc<RenderedConversation>>,
    ) -> Arc<RenderedConversation> {
        project_conversation(
            &snapshot,
            snapshot.conversations["thread"].clone(),
            &previous.cloned(),
        )
    }

    #[test]
    fn flat_rows_preserve_history_order_and_only_offer_fork_on_last_completed_response() {
        let mut snapshot = fixture();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.conversations)
                .get_mut("thread")
                .unwrap(),
        );
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
        turn.items_has_more = Some(true);
        turn.opening_user_message = Some(Arc::new(
            serde_json::from_value(json!({"id":"opening","type":"userMessage","text":"first"}))
                .unwrap(),
        ));
        let rendered = project_snapshot(snapshot.clone(), None);
        let rows = rendered.turns[1].conversation_rows();
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids[0], "history-item:live:opening:occurrence:0");
        assert!(ids[1].starts_with("history-gap:"));
        assert_eq!(ids[2], "history-item:live:accepted:occurrence:0");
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
                .get_mut("thread")
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]).status = Some("completed".into());
        let completed = project_snapshot(snapshot, Some(&rendered));
        let rows = completed.turns[1].conversation_rows();
        assert!(
            matches!(&rows.last().unwrap().content, ConversationRowContent::Response { item, fork_turn_id: Some(id) } if id == "live" && item.data.native_id.as_deref() == Some("stream"))
        );
        assert!(
            rows.iter()
                .any(|row| row.id == "history-item:live:accepted:occurrence:0")
        );
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
                        status: "inProgress".into(),
                        expanded: true
                    })
                ));
            }
        }
    }

    #[test]
    fn flat_row_ids_distinguish_repeated_items_and_turns() {
        let source = Arc::new(
            serde_json::from_value(json!({"id":"thread", "turns":[
                {"id":"first", "status":"completed", "items":[
                    {"id":"same", "type":"agentMessage", "text":"old"},
                    {"id":"same", "type":"agentMessage", "phase":"final_answer", "text":"new"}
                ]},
                {"id":"second", "status":"completed", "items":[
                    {"id":"same", "type":"agentMessage", "text":"another turn"}
                ]}
            ]}))
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
                | ConversationRowContent::Response { item, .. } => Some(item.data.body.as_str()),
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
            Some("interrupted".into());
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
    fn pending_input_remains_visible_until_its_turn_is_loaded() {
        let mut snapshot = fixture();
        Arc::make_mut(&mut snapshot.pending_submissions).insert(
            "pending".into(),
            Arc::new(PendingSubmission {
                draft_key: "thread".into(),
                draft: Arc::new(Draft {
                    text: "waiting for history".into(),
                    ..Default::default()
                }),
                turn_id: Some("unloaded".into()),
                after_item_id: None,
                accepted: true,
                recovery_text: None,
                clear_draft: None,
            }),
        );
        let first = project_snapshot(snapshot.clone(), None);
        assert_eq!(first.queued.len(), 1);
        assert_eq!(first.queued[0].data.body, "waiting for history");
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.conversations)
                .get_mut("thread")
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({
            "id":"unloaded","status":"inProgress","items":[{"id":"echo","type":"userMessage","clientId":"pending","text":"waiting for history"}]
        })).unwrap()));
        let loaded = project_snapshot(snapshot, Some(&first));
        assert!(loaded.queued.is_empty());
        assert_eq!(loaded.turns.last().unwrap().items().count(), 1);
        let echoed = loaded.turns.last().unwrap().items().next().unwrap();
        assert_eq!(echoed.data.id, "pending");
        assert!(matches!(&echoed.source, ItemSource::Native(item) if item.id == "echo"));
    }

    #[test]
    fn delta_reuses_untouched_turns_and_items_but_invalidates_deferred_details() {
        let snapshot = fixture();
        let first = project_snapshot(snapshot.clone(), None);
        let same = project_snapshot(snapshot.clone(), Some(&first));
        assert!(Arc::ptr_eq(&first, &same));
        let (updated, _) = reduce(
            &snapshot,
            Event::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({"threadId":"thread","turnId":"live","itemId":"stream","delta":" world"}),
            },
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
        assert_eq!(first.turns[1].items().nth(2).unwrap().data.body, "hello");
        assert_eq!(
            second.turns[1].items().nth(2).unwrap().data.body,
            "hello world"
        );
        let mut deferred = updated;
        let thread = Arc::make_mut(
            Arc::make_mut(&mut deferred.conversations)
                .get_mut("thread")
                .unwrap(),
        );
        Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]).deferred_item_ids =
            Some(vec!["command".into()]);
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
            json!({"id":1,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"done","reason":"run command"}}),
            json!({"id":"question","method":"item/tool/requestUserInput","params":{"threadId":"thread","questions":[{"question":"which?"}]}}),
            json!({"id":3,"method":"item/fileChange/requestApproval","params":{"threadId":"other"}}),
        ] {
            let request: ServerRequest = serde_json::from_value(value).unwrap();
            Arc::make_mut(&mut snapshot.requests).insert(request.id.to_string(), Arc::new(request));
        }
        let pending = |turn_id| {
            Arc::new(PendingSubmission {
                draft_key: "thread".into(),
                draft: Arc::new(Draft {
                    text: "queued text".into(),
                    ..Default::default()
                }),
                turn_id,
                after_item_id: None,
                accepted: true,
                recovery_text: None,
                clear_draft: None,
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
        assert_eq!(done[0].key, "1");
        assert_eq!(done[0].title, "コマンドの承認待ち");
        assert_eq!(done[0].body, "run command");
        assert_eq!(
            done[0].decision_labels,
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
        assert_eq!(rendered.queued[0].data.body, "queued text");
    }
    #[test]
    fn retry_error_titles_accept_numeric_and_string_http_status() {
        for status in [json!(503), json!("429")] {
            let error = turn_error(
                &json!({"willRetry":true,"message":"retry","codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":status}}}),
            );
            assert_eq!(error.title, "サーバーが混み合っています。再接続しています");
            assert!(error.is_reconnecting);
            assert_eq!(error.message, "retry");
        }
    }
}

fn progress_label(turn: &models::Turn, action: Option<&str>, now_seconds: f64) -> String {
    let started = turn
        .started_at
        .as_ref()
        .and_then(Option::as_ref)
        .and_then(serde_json::Number::as_f64)
        .or_else(|| {
            turn.extra
                .get("startedAtMs")?
                .as_f64()
                .map(|milliseconds| milliseconds / 1000.)
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
