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
    items: Vec<Arc<RenderedItem>>,
    opening: Option<Arc<RenderedItem>>,
    pub rows: Vec<TurnPresentationData>,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TurnPresentationData {
    pub id: String,
    pub turn_id: String,
    pub has_older_items: bool,
    pub opening_user_message: Option<Arc<RenderedItem>>,
    pub status: String,
    pub is_in_progress: bool,
    pub user_messages: Vec<Arc<RenderedItem>>,
    pub activity_summary: Option<String>,
    pub activity_items: Vec<Arc<RenderedItem>>,
    pub responses: Vec<Arc<RenderedItem>>,
    pub activity_initially_expanded: bool,
    pub activity_can_collapse: bool,
    pub error: Option<TurnErrorPresentation>,
    pub pending_requests: Vec<Request>,
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
    fn key(&self) -> (bool, &str) {
        match &self.source {
            ItemSource::Native(item) => (false, &item.id),
            ItemSource::Pending(id, _) => (true, id),
        }
    }
    fn metadata(&self) -> ItemMetadata<'_> {
        match &self.source {
            ItemSource::Native(item) => ItemMetadata::from(item.as_ref()),
            ItemSource::Pending(id, _) => ItemMetadata {
                id,
                client_id: Some(id),
                kind: "userMessage",
                ..Default::default()
            },
        }
    }
    fn native(item: &Arc<models::Item>, deferred: bool, previous: Option<&Arc<Self>>) -> Arc<Self> {
        if let Some(previous) = previous
            && let ItemSource::Native(old) = &previous.source
            && Arc::ptr_eq(item, old)
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
        if let Some(previous) = previous
            && let ItemSource::Pending(old_id, old) = &previous.source
            && old_id == id
            && Arc::ptr_eq(pending, old)
        {
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
        .filter(|(_, p)| p.turn_id.is_none())
        .map(|(id, p)| RenderedItem::pending(id, p, queued.get(&(true, id.as_str())).copied()))
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
        .flat_map(|turn| turn.items.iter().chain(turn.opening.iter()))
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
    let items: Vec<_> = order
        .into_iter()
        .map(|index| {
            if let Some(item) = native.get(index) {
                RenderedItem::native(
                    item,
                    deferred.contains(item.id.as_str()),
                    cached.get(&(false, item.id.as_str())).copied(),
                )
            } else {
                let (id, submission) = &pending[retained[index - native.len()]];
                RenderedItem::pending(id, submission, cached.get(&(true, id.as_str())).copied())
            }
        })
        .collect();
    let opening = source
        .opening_user_message
        .as_ref()
        .filter(|item| !native.iter().any(|native| native.id == item.id))
        .map(|item| {
            RenderedItem::native(
                item,
                deferred.contains(item.id.as_str()),
                cached.get(&(false, item.id.as_str())).copied(),
            )
        });
    let rows = project_items(&source, items.len(), |index| items[index].metadata())
        .map(|segment| {
            let mut users = Vec::new();
            let mut activity = Vec::new();
            let mut responses = Vec::new();
            for (index, item) in items
                .iter()
                .enumerate()
                .take(segment.end)
                .skip(segment.start)
            {
                match segment.role(index, item.metadata()) {
                    Role::Hidden => {}
                    Role::User => users.push(item.clone()),
                    Role::Activity => activity.push(item.clone()),
                    Role::Response => responses.push(item.clone()),
                }
            }
            TurnPresentationData {
                id: segment.id,
                turn_id: source.id.clone(),
                has_older_items: segment.start == 0 && source.items_has_more.unwrap_or(false),
                opening_user_message: if segment.start == 0 {
                    opening.clone()
                } else {
                    None
                },
                status: source.status.clone().unwrap_or_default(),
                is_in_progress: segment.last && source.status.as_deref() == Some("inProgress"),
                user_messages: users,
                activity_summary: segment.label,
                activity_items: activity,
                responses,
                activity_initially_expanded: segment.initially_expanded,
                activity_can_collapse: segment.collapsible,
                error: if segment.last {
                    source.error.as_ref().map(turn_error)
                } else {
                    None
                },
                pending_requests: if segment.last {
                    requests
                        .iter()
                        .map(|r| request(&r.id.to_string(), r))
                        .collect()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    Arc::new(RenderedTurn {
        source,
        pending,
        requests,
        items,
        opening,
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
                &first.turns[1].items[index],
                &second.turns[1].items[index]
            ));
        }
        assert_eq!(first.turns[1].items[2].data.body, "hello");
        assert_eq!(second.turns[1].items[2].data.body, "hello world");
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
            &second.turns[1].items[1],
            &third.turns[1].items[1]
        ));
        assert!(third.turns[1].items[1].data.deferred);
        assert!(Arc::ptr_eq(
            &second.turns[1].items[2],
            &third.turns[1].items[2]
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
        let done = &rendered.turns[0].rows.last().unwrap().pending_requests;
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key, "1");
        assert_eq!(done[0].title, "コマンドの承認待ち");
        assert_eq!(done[0].body, "run command");
        assert_eq!(
            done[0].decision_labels,
            ["承認", "このセッションで承認", "拒否", "キャンセル"]
        );
        let live = &rendered.turns[1].rows.last().unwrap().pending_requests;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].body, "which?");
        assert_eq!(
            rendered.turns[1]
                .items
                .iter()
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
