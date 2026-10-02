//! Build the shared, ordered turn layout and its stable row identities.
use super::super::{GroupKind, ItemMetadata, Role, project_items, source_order};
use super::{
    ActivityPresentation, ConversationRow, ConversationRowContent, PendingItems, RenderedItem,
    RenderedTurn, request, status::turn_error,
};
use crate::models;
use agent_protocol::requests::Request as WireRequest;
use std::{collections::HashMap, sync::Arc};

impl RenderedTurn {
    pub(super) fn items(&self) -> impl Iterator<Item = &Arc<RenderedItem>> {
        self.rows.iter().filter_map(|row| match &row.content {
            ConversationRowContent::User { item }
            | ConversationRowContent::Activity { item, .. }
            | ConversationRowContent::Response { item, .. } => Some(item),
            _ => None,
        })
    }
}

pub(super) fn render_turn(
    provider: Option<crate::session::ProviderKind>,
    supports_fork: bool,
    source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    previous: Option<&Arc<RenderedTurn>>,
) -> Arc<RenderedTurn> {
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
                client_id: Some(id),
                kind: GroupKind::User,
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
        if let Some(summary) = &segment.label {
            push(ActivityHeader {
                activity: ActivityPresentation {
                    id: segment.id.clone(),
                    status: source.status.label().into(),
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
