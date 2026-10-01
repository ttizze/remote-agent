//! Adapt core conversation projection to the native virtual list and lazy loads.
//!
//! Core owns presentation and row ordering. This module owns list identity,
//! scroll anchors, and generation-scoped history/item-detail requests.

use super::{Desktop, DetailLoad, OperationCompletion, Tab};
use agent_core::{
    presentation::conversation::ConversationRowContent,
    state::{Intent, PendingSubmission, operations as op},
};
use agent_protocol::{
    ids::{ItemId, TurnId},
    models::Item,
    session::SessionRef,
};
use gpui_kit::{Context, Focusable, ListOffset, Window};
use std::sync::Arc;

#[derive(Clone)]
pub(super) enum ConversationRow {
    History,
    Turn(Arc<agent_core::presentation::conversation::RenderedTurn>),
    Pending(String, Arc<PendingSubmission>),
    Request(Box<agent_core::presentation::conversation::Request>),
}
impl ConversationRow {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::History, Self::History) => true,
            (Self::Turn(a), Self::Turn(b)) => a.source.id == b.source.id,
            (Self::Pending(a, _), Self::Pending(b, _)) => a == b,
            (Self::Request(a), Self::Request(b)) => a.id == b.id,
            _ => false,
        }
    }
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Turn(a), Self::Turn(b)) => Arc::ptr_eq(a, b),
            (Self::Pending(a, x), Self::Pending(b, y)) => a == b && Arc::ptr_eq(x, y),
            (Self::Request(a), Self::Request(b)) => a == b,
            _ => false,
        }
    }
}

impl Desktop {
    pub(super) fn has_older_history(&self) -> bool {
        self.thread()
            .is_some_and(|thread| thread.history_has_more == Some(true))
    }
    pub(super) fn user_items(&self) -> impl Iterator<Item = &Arc<Item>> {
        self.thread()
            .and_then(|thread| thread.turns.as_deref())
            .unwrap_or_default()
            .iter()
            .flat_map(|turn| turn.items.as_deref().unwrap_or_default())
            .filter(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                )
            })
    }
    pub(super) fn sync_rows(&mut self, reset: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.rendered = self.snapshot.conversation_thread().map(|thread| {
            agent_core::presentation::conversation::project_conversation(
                &self.snapshot,
                thread,
                &self.rendered,
            )
        });
        let rows = conversation_rows(&self.rendered);
        if reset {
            self.list.reset(rows.len());
            self.list.scroll_to_end();
            self.rows = rows;
            return;
        }
        let old = &self.rows;
        let prefix = old
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| a.same_identity(b))
            .count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a.same_identity(b))
            .count();
        let anchor = self.list.logical_scroll_top();
        let preserve = old
            .get(anchor.item_ix)
            .zip(rows.get(anchor.item_ix))
            .filter(|(a, b)| a.same_identity(b) && !a.unchanged(b))
            .and_then(|(a, b)| {
                if let (ConversationRow::Turn(turn), ConversationRow::Turn(next)) = (a, b)
                    && let Some(first) = turn.source.items.as_ref().and_then(|items| items.first())
                    && next.source.items.as_ref().is_some_and(|items| {
                        items
                            .iter()
                            .position(|item| item.id == first.id)
                            .is_some_and(|index| index > 0)
                    })
                {
                    Some((
                        turn.source.id.clone(),
                        self.list.bounds_for_item(anchor.item_ix)?.size.height,
                    ))
                } else {
                    None
                }
            });
        if prefix + suffix != old.len() || old.len() != rows.len() {
            self.list
                .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
        }
        for index in 0..prefix {
            if !old[index].unchanged(&rows[index]) {
                self.list.remeasure_items(index..index + 1);
            }
        }
        for offset in 0..suffix {
            let index = rows.len() - suffix + offset;
            if !old[old.len() - suffix + offset].unchanged(&rows[index]) {
                self.list.remeasure_items(index..index + 1);
            }
        }
        self.rows = rows;
        if self.history_loading
            && let Some((id, old_height)) = preserve
        {
            let generation = self.snapshot.epoch;
            let owner = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| { let _ = owner.update(cx, |view, cx| {
                if view.snapshot.epoch == generation && matches!(view.rows.get(anchor.item_ix), Some(ConversationRow::Turn(turn)) if turn.source.id == id)
                    && let Some(bounds) = view.list.bounds_for_item(anchor.item_ix)
                {
                    view.list.scroll_to(ListOffset { item_ix: anchor.item_ix, offset_in_item: anchor.offset_in_item + bounds.size.height - old_height }); cx.notify();
                }
            }); });
        }
    }
    pub(super) fn pause_tail(&self) {
        if self.list.logical_scroll_top().item_ix == self.list.item_count() {
            self.list
                .scroll_by(-self.list.viewport_bounds().size.height);
        }
        self.list.pause_following_tail();
    }
    pub(super) fn remeasure_item(&self, id: &str) {
        for (index, row) in self.rows.iter().enumerate() {
            if let ConversationRow::Turn(turn) = row
                && (turn.source.id.as_str() == id
                    || turn
                        .source
                        .items
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|item| item.id.as_str() == id))
            {
                self.list.remeasure_items(index..index + 1);
            }
        }
    }
    pub(super) fn new_chat(&mut self, cwd: String, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::NewChat { cwd });
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }
    pub(super) fn open_chat(
        &mut self,
        id: SessionRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.busy += 1;
        self.perform(
            Intent::ReadThread(op::ReadThread::open(id)),
            OperationCompletion::Busy,
        );
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }
    pub(super) fn older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.history_loading {
            return;
        }
        if !self.has_older_history() {
            return;
        }
        let generation = self.snapshot.epoch;
        self.history_loading = true;
        self.history_error.clear();
        self.list.remeasure_items(0..1);
        self.perform(
            Intent::ReadOlder {
                thread_id: self.selected().expect("selected conversation").clone(),
            },
            OperationCompletion::History { generation },
        );
        cx.notify();
    }
    pub(super) fn detail(&mut self, turn_id: TurnId, item_id: ItemId) {
        let key = (turn_id.clone(), item_id.clone());
        if self
            .item_details
            .get(&key)
            .is_some_and(|detail| detail.loading)
        {
            return;
        }
        let needed = self
            .thread()
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.iter().find(|turn| turn.id == turn_id))
            .and_then(|turn| turn.items.as_ref())
            .and_then(|items| items.iter().find(|item| item.id == item_id))
            .is_some_and(|item| item.is_deferred());
        if !needed {
            return;
        }
        self.item_details.insert(
            key.clone(),
            DetailLoad {
                loading: true,
                error: None,
            },
        );
        let generation = self.snapshot.epoch;
        self.perform(
            Intent::ReadItem(op::ReadItem {
                thread_id: self.selected().expect("selected conversation").clone(),
                turn_id,
                item_id,
            }),
            OperationCompletion::Item { generation, key },
        );
    }
}

fn conversation_rows(
    rendered: &Option<Arc<agent_core::presentation::conversation::RenderedConversation>>,
) -> Vec<ConversationRow> {
    let mut rows = Vec::new();
    if let Some(rendered) = rendered {
        rows.push(ConversationRow::History);
        rows.extend(rendered.turns.iter().cloned().map(ConversationRow::Turn));
        rows.extend(rendered.queued.iter().filter_map(|item| {
            if let agent_core::presentation::conversation::ItemSource::Pending(id, pending) =
                &item.source
            {
                Some(ConversationRow::Pending(id.clone(), pending.clone()))
            } else {
                None
            }
        }));
    }
    rows.extend(
        rendered
            .iter()
            .flat_map(|conversation| &conversation.request_rows)
            .filter_map(|row| {
                if let ConversationRowContent::PendingRequest { request } = &row.content {
                    Some(ConversationRow::Request(request.clone()))
                } else {
                    None
                }
            }),
    );
    rows
}

#[cfg(test)]
mod tests {
    use super::{Arc, ConversationRow, conversation_rows};
    use agent_core::state::Snapshot;
    use agent_protocol::models::Thread;

    #[test]
    fn row_projection_keeps_repeated_turns_and_scopes_requests_without_changing_input() {
        let mut source: Arc<Thread> = Arc::new(serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"selected"},"turns":[{"id":"repeated","items":[{"id":"first","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"first answer","phase":"unknown"}}}}}],"status":"unknown"},{"id":"repeated","items":[{"id":"second","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"second answer","phase":"unknown"}}}}}],"status":"unknown"}]})).unwrap());
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id =
            Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "selected".into(),
            });
        let request = Arc::new(agent_protocol::requests::Request {
            id: "global".into(),
            target: agent_protocol::requests::RequestTarget::Session,
            delivery: agent_protocol::session::RequestDelivery::Awaiting,
            body: agent_protocol::requests::RequestBody::Elicitation {
                server: "fixture".into(),
                message: "input".into(),
                input: agent_protocol::requests::ElicitationInput::Form { fields: vec![] },
            },
        });
        Arc::make_mut(&mut source)
            .requests
            .insert(request.id.clone(), request.clone());
        let mut other = (*source).clone();
        let request = Arc::make_mut(other.requests.get_mut("global").unwrap());
        request.id = "other".into();
        other.id = Some(agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "other".into(),
        });
        Arc::make_mut(&mut snapshot.conversations)
            .insert(other.id.clone().unwrap(), Arc::new(other));
        let before = snapshot.clone();
        let rendered = Some(
            agent_core::presentation::conversation::project_conversation(
                &snapshot,
                source.clone(),
                &None,
            ),
        );
        let rows = conversation_rows(&rendered);
        let repeated = conversation_rows(&rendered);
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], ConversationRow::History));
        for (index, turn) in source.turns.as_ref().unwrap().iter().enumerate() {
            let ConversationRow::Turn(row) = &rows[index + 1] else {
                panic!("missing turn")
            };
            assert!(Arc::ptr_eq(&row.source, turn));
        }
        assert!(
            matches!(&rows[3], ConversationRow::Request(request) if request.id.as_str() == "global")
        );
        assert!(rows.iter().zip(&repeated).all(|(a, b)| a.unchanged(b)
            || matches!((a, b), (ConversationRow::History, ConversationRow::History))));
        assert_eq!(snapshot, before);
    }
}
