//! Provider resources collect complete item bodies at Markdown boundaries.
//! Paragraphs are committed at least 400 ms apart; quiet partial text after 800 ms.
use super::*;
use agent_protocol::{ids::ItemId, session::TextField};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Buffered {
    attempt: Attempt,
    text: String,
    delivered: usize,
    changed: Instant,
    last_flush: Option<Instant>,
    boundary: bool,
}
impl Buffered {
    fn append(&mut self, delta: &str) {
        self.text.push_str(delta);
        self.changed = Instant::now();
        self.boundary = self
            .last_flush
            .is_none_or(|last| self.changed >= last + Duration::from_millis(400))
            && paragraph_boundary(&self.text) > self.delivered;
    }
    fn deadline(&self) -> Option<Instant> {
        (self.text.len() != self.delivered).then_some(if self.boundary {
            self.changed
        } else {
            self.changed + Duration::from_millis(800)
        })
    }
}
#[derive(Default)]
pub(super) struct Text {
    // T3 tracks each reasoning part independently, just like assistant text.
    pending: HashMap<(ItemId, TextField), Buffered>,
    worker: Option<Uuid>,
    wake: Arc<Notify>,
}
impl Text {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    fn deadline(&self, token: Uuid) -> Option<Instant> {
        (self.worker == Some(token))
            .then(|| self.pending.values().filter_map(Buffered::deadline).min())
            .flatten()
    }
    fn ready(&mut self, token: Uuid) -> Option<Vec<((ItemId, TextField), Buffered)>> {
        if self.worker != Some(token) {
            return None;
        }
        let now = Instant::now();
        let mut ready = Vec::new();
        for (key, entry) in &mut self.pending {
            if entry.deadline().is_none_or(|deadline| deadline > now) {
                continue;
            }
            let length = if now >= entry.changed + Duration::from_millis(800) {
                entry.text.len()
            } else {
                paragraph_boundary(&entry.text).max(entry.delivered)
            };
            entry.boundary = false;
            entry.delivered = length;
            entry.last_flush = Some(now);
            let mut publication = entry.clone();
            publication.text.truncate(length);
            ready.push((key.clone(), publication));
        }
        if self
            .pending
            .values()
            .all(|entry| entry.deadline().is_none())
        {
            self.worker = None;
        }
        Some(ready)
    }
}
impl resources::Source {
    pub(super) fn replace_text(&self, id: &ItemId) {
        let mut text = self.text.lock().unwrap();
        text.pending.retain(|(item, _), _| item != id);
        if text
            .pending
            .values()
            .all(|entry| entry.deadline().is_none())
        {
            text.worker = None;
        }
        text.wake.notify_one();
    }
    fn take_text(&self) -> HashMap<(ItemId, TextField), Buffered> {
        let mut text = self.text.lock().unwrap();
        text.worker = None;
        text.wake.notify_one();
        std::mem::take(&mut text.pending)
    }
}

impl Runtime {
    pub(super) async fn flush_text(
        service: &HostRpcService,
        source: &resources::Source,
    ) -> Result<(), String> {
        for ((id, field), entry) in source.take_text() {
            if entry.text.len() != entry.delivered {
                Self::publish_text(service, source, &id, field, &entry).await?;
            }
        }
        Ok(())
    }
    async fn publish_text(
        service: &HostRpcService,
        source: &resources::Source,
        id: &ItemId,
        field: TextField,
        entry: &Buffered,
    ) -> Result<(), String> {
        let Some(mut item) = service
            .inner
            .conversation
            .storage
            .item(&source.target, id)?
        else {
            return Ok(());
        };
        *agent_protocol::session::text_part(&mut item.body, field)
            .map_err(|error| error.to_string())? = entry.text.clone();
        let native_turn = service
            .inner
            .conversation
            .storage
            .execution(&source.target, entry.attempt.id)?
            .and_then(|execution| execution.native_turn);
        Self::event(
            service,
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
        .map(|_| ())
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
        let key = (id, field);
        let pending = source.text.lock().unwrap().pending.remove(&key);
        let mut entry = match pending {
            Some(entry) => entry,
            None => {
                let Some(mut item) = service
                    .inner
                    .conversation
                    .storage
                    .item(&source.target, &key.0)?
                else {
                    return Ok(());
                };
                let text = agent_protocol::session::text_part(&mut item.body, field)
                    .map_err(|error| error.to_string())?;
                Buffered {
                    attempt: attempt.clone(),
                    delivered: text.len(),
                    text: std::mem::take(text),
                    changed: Instant::now(),
                    last_flush: None,
                    boundary: false,
                }
            }
        };
        entry.append(delta);
        let mut text = source.text.lock().unwrap();
        text.pending.insert(key, entry);
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
                let deadline = {
                    let mut text = source.text.lock().unwrap();
                    let deadline = text.deadline(token);
                    if deadline.is_none() && text.worker == Some(token) {
                        text.worker = None;
                    }
                    deadline
                };
                let Some(deadline) = deadline else {
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
                for ((id, field), entry) in ready {
                    if let Err(error) =
                        Self::publish_text(&service, &source, &id, field, &entry).await
                    {
                        tracing::error!(target: "agent", operation="conversation.text", %error);
                        let delivered = service
                            .inner
                            .conversation
                            .storage
                            .item(&source.target, &id)
                            .ok()
                            .flatten()
                            .and_then(|mut item| {
                                agent_protocol::session::text_part(&mut item.body, field)
                                    .ok()
                                    .map(|text| text.len())
                            });
                        let mut text = source.text.lock().unwrap();
                        text.worker = Some(token);
                        if let Some(pending) = text.pending.get_mut(&(id, field)) {
                            pending.boundary = false;
                            pending.changed = Instant::now();
                            pending.delivered = delivered.unwrap_or(0);
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

    fn entry(text: &str) -> Buffered {
        let mut entry = Buffered {
            attempt: Attempt {
                id: Uuid::nil(),
                turn: "turn".into(),
                provider: ProviderKind::Codex,
                source: "source".into(),
            },
            text: String::new(),
            delivered: 0,
            changed: Instant::now(),
            last_flush: None,
            boundary: false,
        };
        entry.append(text);
        entry
    }
    #[tokio::test(start_paused = true)]
    async fn reasoning_parts_keep_independent_idle_deadlines_and_delivery_intervals() {
        let token = Uuid::new_v4();
        let mut text = Text {
            worker: Some(token),
            ..Default::default()
        };
        let first = ("reason".into(), TextField::ReasoningSummary { index: 0 });
        let second = ("reason".into(), TextField::ReasoningSummary { index: 1 });
        text.pending.insert(first.clone(), entry("A\n\nx"));
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\n");
        tokio::time::advance(Duration::from_millis(300)).await;
        text.pending.insert(second.clone(), entry("B\n\ny"));
        assert_eq!(text.ready(token).unwrap()[0].1.text, "B\n\n");
        tokio::time::advance(Duration::from_millis(300)).await;
        text.pending.get_mut(&second).unwrap().append(" more");
        tokio::time::advance(Duration::from_millis(200)).await;
        let idle = text.ready(token).unwrap();
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].0, first);
        assert_eq!(idle[0].1.text, "A\n\nx");
        // An idle flush still counts towards T3's 400ms minimum interval.
        tokio::time::advance(Duration::from_millis(100)).await;
        text.pending.get_mut(&first).unwrap().append("\n\nnext");
        assert!(text.ready(token).unwrap().is_empty());
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(text.ready(token).unwrap()[0].1.text, "B\n\ny more");
        tokio::time::advance(Duration::from_millis(300)).await;
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\nx\n\nnext");
        assert!(text.deadline(token).is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn unfinished_blocks_do_not_delay_the_next_complete_paragraph() {
        let token = Uuid::new_v4();
        let key = ("a".into(), TextField::AssistantText);
        let mut text = Text {
            worker: Some(token),
            ..Default::default()
        };
        text.pending.insert(key.clone(), entry("A\n\nx"));
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\n");
        tokio::time::advance(Duration::from_millis(400)).await;
        text.pending.get_mut(&key).unwrap().append("y");
        assert!(text.ready(token).unwrap().is_empty());
        tokio::time::advance(Duration::from_millis(100)).await;
        text.pending.get_mut(&key).unwrap().append("\n\n");
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\nxy\n\n");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_delivery_never_shrinks_to_an_earlier_paragraph_boundary() {
        let token = Uuid::new_v4();
        let mut text = Text {
            worker: Some(token),
            ..Default::default()
        };
        let key = ("a".into(), TextField::AssistantText);
        text.pending.insert(key.clone(), entry("A\n\nB"));
        tokio::time::advance(Duration::from_millis(800)).await;
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\nB");
        tokio::time::advance(Duration::from_millis(400)).await;
        text.pending.get_mut(&key).unwrap().append(" more");
        text.worker = Some(token);
        assert!(text.ready(token).unwrap().is_empty());
        text.pending.get_mut(&key).unwrap().append(".\n\nC");
        assert_eq!(text.ready(token).unwrap()[0].1.text, "A\n\nB more.\n\n");
    }
}
