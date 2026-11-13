//! Provider resources collect complete item bodies at Markdown boundaries.
//! Paragraphs are committed at least 400 ms apart; quiet partial text after 800 ms.
use super::*;
use agent_protocol::{conversation::Item, ids::ItemId, session::TextField};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Buffered {
    attempt: Attempt,
    item: Item,
    delivered: agent_protocol::items::ItemBody,
    changed: Instant,
    last_flush: Option<Instant>,
    boundary: bool,
}
#[derive(Default)]
pub(super) struct Text {
    pending: HashMap<ItemId, Buffered>,
    worker: Option<Uuid>,
    wake: Arc<Notify>,
}
impl Text {
    pub(super) fn is_empty(&self) -> bool {
        self.pending
            .values()
            .all(|entry| entry.item.body == entry.delivered)
    }
    fn deadline(&self, token: Uuid) -> Option<Instant> {
        (self.worker == Some(token))
            .then(|| {
                self.pending
                    .values()
                    .filter(|entry| entry.item.body != entry.delivered)
                    .map(|entry| {
                        if entry.boundary {
                            entry.changed
                        } else {
                            entry.changed + Duration::from_millis(800)
                        }
                    })
                    .min()
            })
            .flatten()
    }
    fn ready(&mut self, token: Uuid) -> Option<Vec<Buffered>> {
        if self.worker != Some(token) {
            return None;
        }
        let now = Instant::now();
        let mut ready = Vec::new();
        for entry in self
            .pending
            .values_mut()
            .filter(|entry| entry.item.body != entry.delivered)
        {
            let body = if now >= entry.changed + Duration::from_millis(800) {
                entry.item.body.clone()
            } else if entry.boundary {
                paragraph_body(&entry.item.body, &entry.delivered)
            } else {
                continue;
            };
            entry.boundary = false;
            if body == entry.delivered {
                continue;
            }
            entry.last_flush = Some(now);
            entry.delivered = body.clone();
            let mut publication = entry.clone();
            publication.item.body = body;
            ready.push(publication);
        }
        if self.is_empty() {
            self.worker = None;
        }
        Some(ready)
    }
}
impl resources::Source {
    pub(super) fn replace_text(&self, id: &ItemId) {
        let mut text = self.text.lock().unwrap();
        text.pending.remove(id);
        if text.is_empty() {
            text.worker = None;
        }
        text.wake.notify_one();
    }
    pub(super) fn take_text(&self) -> Vec<(Attempt, Observation)> {
        let mut text = self.text.lock().unwrap();
        text.worker = None;
        text.wake.notify_one();
        std::mem::take(&mut text.pending)
            .into_values()
            .filter(|entry| entry.item.body != entry.delivered)
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

impl Runtime {
    pub(super) async fn flush_text(
        service: &HostRpcService,
        source: &resources::Source,
    ) -> Result<(), String> {
        for (attempt, observation) in source.take_text() {
            Self::event(
                service,
                &source.target,
                Event::Provider {
                    attempt: attempt.id,
                    native_turn: service
                        .inner
                        .conversation
                        .storage
                        .execution(&source.target, attempt.id)?
                        .and_then(|execution| execution.native_turn),
                    native_items: Vec::new(),
                    observation,
                },
            )
            .await?;
        }
        Ok(())
    }
}

// T3 Code splitBufferedAssistantText: only complete Markdown blocks are ready.
fn paragraph_boundary(text: &str) -> usize {
    use std::sync::LazyLock;
    static FENCE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^( *)(`{3,}|~{3,})").unwrap());
    static LIST: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^[ \t]*(?:[-*+]|[0-9]{1,9}[.)])[ \t]").unwrap());
    static HEADING: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^#{1,6}(?:[ \t]|$)").unwrap());
    let mut fence: Option<(char, usize, usize)> = None;
    let mut boundary = 0;
    let mut start = 0;
    let mut title = false;
    loop {
        let newline = text[start..].find('\n').map(|offset| start + offset);
        let line = text[start..newline.unwrap_or(text.len())].trim_end_matches([' ', '\t', '\r']);
        if fence.is_none() && start > 0 && !title && LIST.is_match(line) {
            boundary = start;
        }
        let Some(newline) = newline else {
            break;
        };
        if let Some(marker) = FENCE.captures(line) {
            let indent = marker[1].len();
            let marker = &marker[2];
            let ch = marker.chars().next().unwrap();
            match fence {
                None => {
                    fence = Some((ch, marker.len(), indent));
                    title = false;
                }
                Some((open, length, depth))
                    if ch == open
                        && marker.len() >= length
                        && indent <= depth + 3
                        && line.len() == indent + marker.len() =>
                {
                    fence = None;
                    boundary = newline + 1;
                }
                _ => {}
            }
        } else if fence.is_none() && line.chars().all(|ch| ch == ' ' || ch == '\t') && start > 0 {
            if !title {
                boundary = newline + 1;
            }
        } else if fence.is_none() {
            if start > 0 && !title && HEADING.is_match(line) {
                boundary = start;
            }
            let trimmed = line.trim_start_matches(' ');
            let section = line.len() - trimmed.len() <= 3
                && (HEADING.is_match(trimmed)
                    || trimmed
                        .strip_suffix(':')
                        .unwrap_or(trimmed)
                        .strip_prefix("**")
                        .and_then(|value| value.strip_suffix("**"))
                        .is_some_and(|value| {
                            !value.is_empty() && !value.contains("**") && !value.ends_with('*')
                        }));
            title = section;
        }
        start = newline + 1;
    }
    boundary
}

fn has_paragraph(
    body: &agent_protocol::items::ItemBody,
    delivered: &agent_protocol::items::ItemBody,
) -> bool {
    use agent_protocol::items::ItemBody;
    let advanced = |text: &str, old: &str| paragraph_boundary(text) > old.len();
    match (body, delivered) {
        (ItemBody::AssistantText { text, .. }, ItemBody::AssistantText { text: old, .. }) => {
            advanced(text, old)
        }
        (
            ItemBody::Reasoning { content, summary },
            ItemBody::Reasoning {
                content: old_content,
                summary: old_summary,
            },
        ) => [(content, old_content), (summary, old_summary)]
            .into_iter()
            .any(|(values, old)| {
                values
                    .iter()
                    .enumerate()
                    .any(|(i, text)| advanced(text, old.get(i).map_or("", String::as_str)))
            }),
        _ => false,
    }
}

fn paragraph_body(
    body: &agent_protocol::items::ItemBody,
    delivered: &agent_protocol::items::ItemBody,
) -> agent_protocol::items::ItemBody {
    use agent_protocol::items::ItemBody;
    fn trim(text: &mut String, delivered: &str) {
        let floor = if text.starts_with(delivered) {
            delivered.len()
        } else {
            0
        };
        text.truncate(paragraph_boundary(text).max(floor));
    }
    let mut body = body.clone();
    match (&mut body, delivered) {
        (ItemBody::AssistantText { text, .. }, ItemBody::AssistantText { text: old, .. }) => {
            trim(text, old)
        }
        (
            ItemBody::Reasoning { content, summary },
            ItemBody::Reasoning {
                content: old_content,
                summary: old_summary,
            },
        ) => {
            for (values, old) in [(content, old_content), (summary, old_summary)] {
                for (index, value) in values.iter_mut().enumerate() {
                    trim(value, old.get(index).map_or("", String::as_str));
                }
            }
        }
        _ => {}
    }
    body
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
            return service.inner.conversation.outputs.write(
                attempt.id,
                records::OutputKey {
                    session: source.target.clone(),
                    item: id,
                },
                super::output::Change::Append(delta),
            );
        }
        // The resource update lock serializes producers and its flush worker.
        // Move an existing body instead of copying all earlier text per delta.
        let pending = source.text.lock().unwrap().pending.remove(&id);
        let mut entry = match pending {
            Some(entry) => entry,
            None => {
                let Some(item) = service
                    .inner
                    .conversation
                    .storage
                    .item(&source.target, &id)?
                else {
                    return Ok(());
                };
                Buffered {
                    attempt: attempt.clone(),
                    delivered: item.body.clone(),
                    item,
                    changed: Instant::now(),
                    last_flush: None,
                    boundary: false,
                }
            }
        };
        let result = agent_protocol::session::text_delta(&mut entry.item.body, field, delta);
        if result.is_ok() {
            entry.changed = Instant::now();
            entry.boundary = entry
                .last_flush
                .is_none_or(|last| entry.changed >= last + Duration::from_millis(400))
                && has_paragraph(&entry.item.body, &entry.delivered);
        }
        let mut text = source.text.lock().unwrap();
        text.pending.insert(id, entry);
        result.map_err(|error| error.to_string())?;
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
                        let delivered = service
                            .inner
                            .conversation
                            .storage
                            .item(&source.target, &entry.item.id)
                            .ok()
                            .flatten()
                            .map(|item| item.body);
                        let mut text = source.text.lock().unwrap();
                        text.worker = Some(token);
                        if let Some(pending) = text.pending.get_mut(&entry.item.id) {
                            pending.boundary = false;
                            pending.changed = Instant::now();
                            if let Some(delivered) = delivered {
                                pending.delivered = delivered;
                            }
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::items::{AssistantPhase, ItemBody};

    #[test]
    fn markdown_boundaries_hold_unfinished_blocks_and_section_titles() {
        for (text, ready) in [
            ("Intro\n\n```rs\nx()\n\n", "Intro\n\n"),
            ("```rs\nx()\n```\nrest", "```rs\nx()\n```\n"),
            ("Intro\n\n## Results\n\n", "Intro\n\n"),
            ("**Results**\n\nBody\n\nNext", "**Results**\n\nBody\n\n"),
            ("Steps:\n\n- one\n- tw", "Steps:\n\n- one\n"),
            ("Intro.\n\n| a | b |\n|---|---|\n| 1 |", "Intro.\n\n"),
            ("Text\n# Heading\nbody", "Text\n"),
            ("~~~\na\n```\n\n", ""),
            ("  \n", ""),
        ] {
            assert_eq!(&text[..paragraph_boundary(text)], ready, "{text:?}");
        }
    }

    fn body(text: &str) -> ItemBody {
        ItemBody::AssistantText {
            text: text.into(),
            phase: AssistantPhase::default(),
        }
    }
    fn entry(id: &str, text: &str) -> Buffered {
        Buffered {
            attempt: Attempt {
                id: Uuid::nil(),
                turn: "turn".into(),
                provider: ProviderKind::Codex,
                source: "source".into(),
            },
            item: Item {
                id: id.into(),
                turn: "turn".into(),
                position: 1,
                status: agent_protocol::execution::ItemStatus::Running,
                deferred: false,
                input: None,
                body: body(text),
            },
            delivered: body(""),
            changed: Instant::now(),
            last_flush: None,
            boundary: true,
        }
    }
    #[tokio::test(start_paused = true)]
    async fn paragraphs_deliver_only_the_prefix_and_each_item_keeps_its_own_idle_deadline() {
        let token = Uuid::new_v4();
        let mut text = Text {
            worker: Some(token),
            ..Default::default()
        };
        text.pending.insert("a".into(), entry("a", "A\n\nx"));
        let first = text.ready(token).unwrap();
        assert_eq!(first[0].item.body, body("A\n\n"));
        assert!(!text.is_empty());
        tokio::time::advance(Duration::from_millis(300)).await;
        text.pending.insert("b".into(), entry("b", "B\n\ny"));
        let second = text.ready(token).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].item.body, body("B\n\n"));
        tokio::time::advance(Duration::from_millis(500)).await;
        let idle = text.ready(token).unwrap();
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].item.body, body("A\n\nx"));
        tokio::time::advance(Duration::from_millis(300)).await;
        assert_eq!(text.ready(token).unwrap()[0].item.body, body("B\n\ny"));
        assert!(text.is_empty());
    }
    #[test]
    fn idle_delivery_never_shrinks_to_an_earlier_paragraph_boundary() {
        assert_eq!(
            paragraph_body(&body("A\n\nB more"), &body("A\n\nB")),
            body("A\n\nB")
        );
        assert_eq!(
            paragraph_body(&body("A\n\nB more.\n\nC"), &body("A\n\nB")),
            body("A\n\nB more.\n\n")
        );
    }
}
