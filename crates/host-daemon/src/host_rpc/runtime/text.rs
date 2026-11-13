//! Provider resources collect complete item bodies at Markdown boundaries.
//! Paragraphs are committed at least 400 ms apart; quiet partial text after 800 ms.
use super::*;
use agent_protocol::{conversation::Item, ids::ItemId, session::TextField};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

struct Buffered {
    attempt: Attempt,
    item: Item,
    changed: Instant,
    boundary: bool,
}
#[derive(Default)]
pub(super) struct Text {
    pending: HashMap<ItemId, Buffered>,
    last_flush: Option<Instant>,
    worker: Option<Uuid>,
    wake: Arc<Notify>,
}
impl Text {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    fn deadline(&self, token: Uuid) -> Option<Instant> {
        if self.worker != Some(token) {
            return None;
        }
        self.pending
            .values()
            .map(|entry| {
                let due = if entry.boundary {
                    entry.changed
                } else {
                    entry.changed + Duration::from_millis(800)
                };
                self.last_flush
                    .map_or(due, |last| due.max(last + Duration::from_millis(400)))
            })
            .min()
    }
    fn ready(&mut self, token: Uuid) -> Option<Vec<Buffered>> {
        if self.worker != Some(token) {
            return None;
        }
        let now = Instant::now();
        if self
            .last_flush
            .is_some_and(|last| now < last + Duration::from_millis(400))
        {
            return Some(Vec::new());
        }
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, entry)| {
                entry.boundary || now >= entry.changed + Duration::from_millis(800)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let ready: Vec<_> = ids
            .into_iter()
            .map(|id| self.pending.remove(&id).expect("selected buffer"))
            .collect();
        if !ready.is_empty() {
            self.last_flush = Some(now);
        }
        if self.pending.is_empty() {
            self.worker = None;
        }
        Some(ready)
    }
}
impl resources::Source {
    pub(super) fn replace_text(&self, id: &ItemId) {
        let mut text = self.text.lock().unwrap();
        if text.pending.remove(id).is_some() {
            text.last_flush = Some(Instant::now());
        }
        if text.pending.is_empty() {
            text.worker = None;
        }
        text.wake.notify_one();
    }
    pub(super) fn take_text(&self) -> Vec<(Attempt, Observation)> {
        let mut text = self.text.lock().unwrap();
        text.worker = None;
        if !text.pending.is_empty() {
            text.last_flush = Some(Instant::now());
        }
        text.wake.notify_one();
        std::mem::take(&mut text.pending)
            .into_values()
            .map(|entry| {
                (
                    entry.attempt,
                    Observation::Item {
                        deferred: entry.item.deferred,
                        id: entry.item.id,
                        status: entry.item.status,
                        body: entry.item.body,
                    },
                )
            })
            .collect()
    }
}

fn boundary(body: &agent_protocol::items::ItemBody, field: TextField, delta: &str) -> bool {
    use agent_protocol::items::ItemBody;
    let previous = match (body, field) {
        (ItemBody::AssistantText { text, .. }, TextField::AssistantText) => Some(text.as_str()),
        (ItemBody::Reasoning { content, .. }, TextField::ReasoningContent { index }) => {
            content.get(index as usize).map(String::as_str)
        }
        (ItemBody::Reasoning { summary, .. }, TextField::ReasoningSummary { index }) => {
            summary.get(index as usize).map(String::as_str)
        }
        _ => None,
    };
    delta.contains("\n\n")
        || delta.starts_with('\n') && previous.is_some_and(|text| text.ends_with('\n'))
}

impl Runtime {
    pub(super) fn text_delta(
        service: &HostRpcService,
        source: &Arc<resources::Source>,
        attempt: &Attempt,
        native: &str,
        field: TextField,
        delta: &str,
    ) -> Result<(), String> {
        let id = decision::provider_item_id(attempt.id, native);
        if field == TextField::CommandOutput {
            return service.inner.conversation.outputs.append(
                attempt.id,
                records::OutputKey {
                    session: source.target.clone(),
                    item: id,
                },
                delta,
            );
        }
        // The resource update lock serializes producers and its flush worker.
        // Move an existing body instead of copying all earlier text per delta.
        let pending = source.text.lock().unwrap().pending.remove(&id);
        let pending_changed = pending.as_ref().map(|entry| entry.changed);
        let (item, had_boundary) = match pending {
            Some(entry) => (Some(entry.item), entry.boundary),
            None => (
                service
                    .inner
                    .conversation
                    .storage
                    .item(&source.target, &id)?,
                false,
            ),
        };
        let Some(mut item) = item else {
            return Ok(());
        };
        let boundary = had_boundary || boundary(&item.body, field, delta);
        if let Err(error) = agent_protocol::session::text_delta(&mut item.body, field, delta) {
            if let Some(changed) = pending_changed {
                source.text.lock().unwrap().pending.insert(
                    id,
                    Buffered {
                        attempt: attempt.clone(),
                        item,
                        changed,
                        boundary: had_boundary,
                    },
                );
            }
            return Err(error.to_string());
        }
        let mut text = source.text.lock().unwrap();
        text.pending.insert(
            id.clone(),
            Buffered {
                attempt: attempt.clone(),
                item: Item { id, ..item },
                changed: Instant::now(),
                boundary,
            },
        );
        text.wake.notify_one();
        if text.worker.is_some() {
            return Ok(());
        }
        let token = Uuid::new_v4();
        text.worker = Some(token);
        let wake = text.wake.clone();
        let owner = Arc::downgrade(&service.inner);
        let source = source.clone();
        let native =
            SessionRef::new(attempt.provider, attempt.source.clone()).map_err(str::to_owned)?;
        tokio::spawn(async move {
            loop {
                let Some(deadline) = source.text.lock().unwrap().deadline(token) else {
                    break;
                };
                tokio::select! { _ = tokio::time::sleep_until(deadline) => {}, _ = wake.notified() => { continue; } }
                let Some(inner) = owner.upgrade() else {
                    break;
                };
                let service = HostRpcService { inner };
                let _updates = source.updates.lock().await;
                let Some(ready) = source.text.lock().unwrap().ready(token) else {
                    break;
                };
                for entry in ready {
                    let item = entry.item.clone();
                    let result = async {
                        let native_turn = service
                            .inner
                            .conversation
                            .storage
                            .execution(&source.target, entry.attempt.id)?
                            .and_then(|execution| execution.native_turn);
                        Self::event(
                            &service,
                            &source.target,
                            Event::Provider {
                                attempt: entry.attempt.id,
                                native_turn,
                                native_items: Vec::new(),
                                observation: Observation::Item {
                                    deferred: item.deferred,
                                    id: item.id,
                                    status: item.status,
                                    body: item.body,
                                },
                            },
                        )
                        .await
                    }
                    .await;
                    if let Err(error) = result {
                        tracing::error!(target: "agent", operation="conversation.text", %error);
                        let mut text = source.text.lock().unwrap();
                        text.worker = Some(token);
                        text.pending.insert(
                            entry.item.id.clone(),
                            Buffered {
                                boundary: false,
                                changed: Instant::now(),
                                ..entry
                            },
                        );
                    }
                }
            }
            drop(source);
            if let Some(inner) = owner.upgrade() {
                inner.conversation.retire_source(&native);
            }
        });
        Ok(())
    }
}
