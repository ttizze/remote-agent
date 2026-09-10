//! UniFFI handles and value conversion for the shared Rust projection.
use crate::{ItemPresentation, Request, RequestKind, Thread};
use conversation_presentation::presentation as shared;
use std::{collections::HashMap, sync::Arc};

#[derive(uniffi::Object)]
pub struct RenderedConversation {
    source: Arc<shared::RenderedConversation>,
    turns: Vec<Arc<RenderedTurn>>,
    queued: Vec<Arc<RenderedItem>>,
}
#[uniffi::export]
impl RenderedConversation {
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
    }
    pub fn turns(&self) -> Vec<Arc<RenderedTurn>> {
        self.turns.clone()
    }
    pub fn queued(&self) -> Vec<Arc<RenderedItem>> {
        self.queued.clone()
    }
}
#[derive(uniffi::Object)]
pub struct RenderedTurn {
    source: Arc<shared::RenderedTurn>,
    rows: Vec<TurnPresentationData>,
}
#[uniffi::export]
impl RenderedTurn {
    pub fn id(&self) -> String {
        self.source.source.id.clone()
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
    }
    pub fn rows(&self) -> Vec<TurnPresentationData> {
        self.rows.clone()
    }
}
#[derive(uniffi::Object)]
pub struct RenderedItem(Arc<shared::RenderedItem>);
#[uniffi::export]
impl RenderedItem {
    pub fn id(&self) -> String {
        self.0.data.id.clone()
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn presentation(&self) -> ItemPresentation {
        let p = &self.0.data;
        ItemPresentation {
            id: p.id.clone(),
            native_id: p.native_id.clone(),
            kind: p.kind.clone(),
            title: p.title.clone(),
            collapsible: p.collapsible,
            visible: p.visible,
            body: p.body.clone(),
            image_sources: p.image_sources.clone(),
            deferred: p.deferred,
        }
    }
    pub fn expanded_body(&self) -> String {
        self.0.expanded_body()
    }
}
#[derive(Clone, uniffi::Record)]
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
#[derive(Clone, uniffi::Record)]
pub struct TurnErrorPresentation {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub is_reconnecting: bool,
}

/// Retain binding handles when the shared projection retains its objects.
#[uniffi::export]
pub fn project_conversation(
    source: Arc<Thread>,
    previous: Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let projected = shared::project_conversation(
        source.0.clone(),
        &source.1,
        source.2.clone(),
        previous.as_ref().map(|p| &p.source),
    );
    if let Some(previous) = &previous
        && Arc::ptr_eq(&previous.source, &projected)
    {
        return previous.clone();
    }
    let turns: HashMap<_, _> = previous
        .iter()
        .flat_map(|p| &p.turns)
        .map(|t| (Arc::as_ptr(&t.source), t))
        .collect();
    let mut items: HashMap<_, _> = previous
        .iter()
        .flat_map(|p| &p.queued)
        .map(|i| (Arc::as_ptr(&i.0), i.clone()))
        .collect();
    for row in previous.iter().flat_map(|p| &p.turns).flat_map(|t| &t.rows) {
        for item in row
            .user_messages
            .iter()
            .chain(&row.activity_items)
            .chain(&row.responses)
            .chain(&row.opening_user_message)
        {
            items.insert(Arc::as_ptr(&item.0), item.clone());
        }
    }
    let mut item = |source: &Arc<shared::RenderedItem>| {
        items
            .entry(Arc::as_ptr(source))
            .or_insert_with(|| Arc::new(RenderedItem(source.clone())))
            .clone()
    };
    let turns = projected
        .turns
        .iter()
        .map(|source| {
            if let Some(previous) = turns.get(&Arc::as_ptr(source)) {
                return (*previous).clone();
            }
            let rows = source
                .rows
                .iter()
                .map(|row| TurnPresentationData {
                    id: row.id.clone(),
                    turn_id: row.turn_id.clone(),
                    has_older_items: row.has_older_items,
                    opening_user_message: row.opening_user_message.as_ref().map(&mut item),
                    status: row.status.clone(),
                    is_in_progress: row.is_in_progress,
                    user_messages: row.user_messages.iter().map(&mut item).collect(),
                    activity_summary: row.activity_summary.clone(),
                    activity_items: row.activity_items.iter().map(&mut item).collect(),
                    responses: row.responses.iter().map(&mut item).collect(),
                    activity_initially_expanded: row.activity_initially_expanded,
                    activity_can_collapse: row.activity_can_collapse,
                    error: row.error.as_ref().map(|e| TurnErrorPresentation {
                        title: e.title.clone(),
                        message: e.message.clone(),
                        details: e.details.clone(),
                        is_reconnecting: e.is_reconnecting,
                    }),
                    pending_requests: row.pending_requests.iter().map(Request::from).collect(),
                })
                .collect();
            Arc::new(RenderedTurn {
                source: source.clone(),
                rows,
            })
        })
        .collect();
    let queued = projected.queued.iter().map(item).collect();
    Arc::new(RenderedConversation {
        source: projected,
        turns,
        queued,
    })
}
impl From<&shared::Request> for Request {
    fn from(r: &shared::Request) -> Self {
        Self {
            id: (&r.id).into(),
            key: r.key.clone(),
            method: r.method.clone(),
            kind: match r.kind {
                shared::RequestKind::CommandApproval => RequestKind::CommandApproval,
                shared::RequestKind::FileApproval => RequestKind::FileApproval,
                shared::RequestKind::Permissions => RequestKind::Permissions,
                shared::RequestKind::Questions => RequestKind::Questions,
                shared::RequestKind::Elicitation => RequestKind::Elicitation,
                shared::RequestKind::Tool => RequestKind::Tool,
                shared::RequestKind::Other => RequestKind::Other,
            },
            title: r.title.clone(),
            body: r.body.clone(),
            decision_labels: r.decision_labels.clone(),
            decisions: r.decisions.iter().map(Into::into).collect(),
            params: r
                .params
                .iter()
                .map(|(k, v)| (k.clone(), v.into()))
                .collect(),
        }
    }
}
pub(crate) fn request(key: &str, source: &agent_core::client::ServerRequest) -> Request {
    (&shared::request(key, source)).into()
}
