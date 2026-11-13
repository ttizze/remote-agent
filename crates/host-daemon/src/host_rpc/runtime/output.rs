//! Ephemeral command output is folded by the shared terminal model. A watch
//! retains one checkpoint; a bounded raw append is published every 100 ms.
use super::*;
use agent_protocol::{
    protocol,
    sync::{Output, OutputUpdate},
};
use tokio::{
    sync::watch,
    time::{Duration, Instant},
};

pub(super) enum Change<'a> {
    Append(&'a str),
    Replace { text: &'a str, truncated: bool },
}
#[derive(Default)]
pub(super) struct Outputs(Mutex<HashMap<records::OutputKey, Live>>);
struct Live {
    execution: Uuid,
    tail: Output,
    published: watch::Sender<Publication>,
    pending: Option<String>,
    scheduled: bool,
}
#[derive(Clone)]
struct Publication {
    from: u64,
    tail: Output,
    append: Option<String>,
}

pub struct Subscription {
    receiver: watch::Receiver<Publication>,
    sequence: u64,
    next_frame: Option<Instant>,
}
impl Subscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            self.receiver.changed().await.ok()?;
            if let Some(next) = self.next_frame {
                tokio::time::sleep_until(next).await;
            }
            let next = self.receiver.borrow_and_update().clone();
            if next.tail.sequence <= self.sequence {
                continue;
            }
            let update = if self.sequence == next.from
                && let Some(text) = next.append
            {
                self.next_frame = Some(Instant::now() + Duration::from_millis(100));
                OutputUpdate::Append {
                    from: self.sequence,
                    through: next.tail.sequence,
                    text,
                }
            } else {
                self.next_frame = Some(Instant::now() + Duration::from_millis(500));
                OutputUpdate::Tail(next.tail.clone())
            };
            self.sequence = next.tail.sequence;
            return protocol::encode(update).ok();
        }
    }
}
impl Live {
    fn new(execution: Uuid, tail: Output) -> Self {
        Self {
            execution,
            published: watch::channel(Publication {
                from: tail.sequence,
                tail: tail.clone(),
                append: None,
            })
            .0,
            tail,
            pending: Some(String::new()),
            scheduled: false,
        }
    }
}
impl Outputs {
    pub(super) fn write(
        self: &Arc<Self>,
        execution: Uuid,
        key: records::OutputKey,
        change: Change<'_>,
    ) -> Result<(), String> {
        let mut streams = self.0.lock().unwrap();
        let live = streams
            .entry(key.clone())
            .or_insert_with(|| Live::new(execution, Output::default()));
        let through = live
            .tail
            .sequence
            .checked_add(1)
            .ok_or("command output sequence overflow")?;
        match change {
            Change::Append(text) => {
                if text.is_empty() {
                    return Ok(());
                }
                live.tail = live
                    .tail
                    .apply(OutputUpdate::Append {
                        from: live.tail.sequence,
                        through,
                        text: text.into(),
                    })
                    .map_err(|error| error.to_string())?;
                if let Some(pending) = &mut live.pending {
                    pending.push_str(text);
                    if pending.encode_utf16().count() > 16 * 1024 {
                        live.pending = None;
                    }
                }
            }
            Change::Replace { text, truncated } => {
                live.tail = Output::new(through, text);
                live.tail.truncated |= truncated;
                live.pending = None;
            }
        }
        if live.scheduled {
            return Ok(());
        }
        live.scheduled = true;
        let owner = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(owner) = owner.upgrade()
                && let Some(live) = owner.0.lock().unwrap().get_mut(&key)
            {
                live.scheduled = false;
                let from = live.published.borrow().tail.sequence;
                let append = live.pending.replace(String::new());
                live.published.send_replace(Publication {
                    from,
                    tail: live.tail.clone(),
                    append,
                });
            }
        });
        Ok(())
    }
    pub(super) fn finish(&self, key: &records::OutputKey, final_text: &str) -> Output {
        let live = self.0.lock().unwrap().remove(key);
        if final_text.is_empty()
            && let Some(live) = live
        {
            return live.tail;
        }
        Output::persisted(final_text)
    }
    pub(super) fn finish_attempt(
        &self,
        session: &SessionRef,
        execution: Uuid,
    ) -> Vec<(records::OutputKey, Output)> {
        let mut streams = self.0.lock().unwrap();
        let keys: Vec<_> = streams
            .iter()
            .filter(|(key, live)| &key.session == session && live.execution == execution)
            .map(|(key, _)| key.clone())
            .collect();
        keys.into_iter()
            .map(|key| {
                let live = streams.remove(&key).expect("selected live output");
                (key, live.tail)
            })
            .collect()
    }
    pub(super) fn subscribe(
        &self,
        key: &records::OutputKey,
        initial: Option<(Uuid, Output)>,
    ) -> Option<(Output, Subscription)> {
        let mut streams = self.0.lock().unwrap();
        if let Some((execution, tail)) = initial {
            streams
                .entry(key.clone())
                .or_insert_with(|| Live::new(execution, tail));
        }
        let live = streams.get(key)?;
        Some((
            live.tail.clone(),
            Subscription {
                receiver: live.published.subscribe(),
                sequence: live.tail.sequence,
                next_frame: None,
            },
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn live_appends_and_slow_checkpoint_replacements_fold_to_the_same_terminal() {
        let outputs = Arc::new(Outputs::default());
        let execution = Uuid::new_v4();
        let key = records::OutputKey {
            session: SessionRef::new(None, Uuid::new_v4().to_string()).unwrap(),
            item: "command".into(),
        };
        let (mut fast, mut watching) = outputs
            .subscribe(&key, Some((execution, Output::new(0, "saved"))))
            .unwrap();
        let (mut slow, mut lagging) = outputs.subscribe(&key, None).unwrap();
        for text in [" new", "\rprogress 1%", "\rprogress 99%\x1b[K"] {
            outputs
                .write(execution, key.clone(), Change::Append(text))
                .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            let update: OutputUpdate = protocol::decode(&watching.recv().await.unwrap()).unwrap();
            fast = fast.apply(update).unwrap();
        }
        let update: OutputUpdate = protocol::decode(&lagging.recv().await.unwrap()).unwrap();
        assert!(matches!(update, OutputUpdate::Tail(_)));
        slow = slow.apply(update).unwrap();
        assert_eq!(fast, slow);
        assert_eq!(fast.text, "progress 99%");
        assert_eq!(outputs.finish(&key, "").text, fast.text);
        assert!(watching.recv().await.is_none());
    }
}
