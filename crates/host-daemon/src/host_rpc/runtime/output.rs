//! Ephemeral command output is folded by the shared terminal model. A watch
//! retains one checkpoint; a bounded raw append is published every 100 ms.
use super::*;
use agent_protocol::{
    protocol,
    sync::{Output, OutputStream, OutputUpdate},
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
    tail: OutputStream,
    published: watch::Sender<Publication>,
    recent: Arc<std::collections::VecDeque<Append>>,
    recent_units: usize,
    scheduled: bool,
    written: Instant,
    overflowed_through: u64,
}
#[derive(Clone)]
struct Append {
    from: u64,
    through: u64,
    text: Arc<str>,
    units: usize,
}
#[derive(Clone)]
struct Publication {
    tail: Output,
    recent: Arc<std::collections::VecDeque<Append>>,
    overflowed_through: u64,
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
            let mut through = self.sequence;
            let mut text = String::new();
            for append in next
                .recent
                .iter()
                .filter(|append| append.through > self.sequence)
            {
                if append.from != through {
                    break;
                }
                text.push_str(&append.text);
                through = append.through;
            }
            let update = if through == next.tail.sequence {
                OutputUpdate::Append {
                    from: self.sequence,
                    through,
                    text,
                }
            } else {
                OutputUpdate::Tail(next.tail.clone())
            };
            let delay = if self.sequence < next.overflowed_through {
                500
            } else {
                100
            };
            self.next_frame = Some(Instant::now() + Duration::from_millis(delay));
            self.sequence = next.tail.sequence;
            return protocol::encode(update).ok();
        }
    }
}
impl Live {
    fn new(execution: Uuid, tail: Output) -> Self {
        let published = watch::channel(Publication {
            tail: tail.clone(),
            recent: Arc::default(),
            overflowed_through: 0,
        })
        .0;
        Self {
            execution,
            tail: tail.into(),
            published,
            recent: Arc::default(),
            recent_units: 0,
            scheduled: false,
            written: Instant::now(),
            overflowed_through: 0,
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
        if matches!(change, Change::Append(text) if text.is_empty()) {
            return Ok(());
        }
        let mut streams = self.0.lock().unwrap();
        let live = streams
            .entry(key.clone())
            .or_insert_with(|| Live::new(execution, Output::default()));
        let through = live
            .tail
            .sequence()
            .checked_add(1)
            .ok_or("command output sequence overflow")?;
        match change {
            Change::Append(text) => {
                let from = live.tail.sequence();
                live.tail.append(through, text);
                let units = text.encode_utf16().count();
                let recent = Arc::make_mut(&mut live.recent);
                recent.push_back(Append {
                    from,
                    through,
                    text: Arc::from(text),
                    units,
                });
                live.recent_units += units;
                while live.recent_units > 16 * 1024 {
                    let removed = recent.pop_front().expect("pending append budget exceeded");
                    live.recent_units -= removed.units;
                    live.overflowed_through = removed.through;
                }
            }
            Change::Replace { text, truncated } => {
                let mut tail = Output::new(through, text);
                tail.truncated |= truncated;
                live.tail = tail.into();
                live.recent = Arc::default();
                live.recent_units = 0;
                live.overflowed_through = 0;
            }
        }
        live.written = Instant::now();
        if live.scheduled {
            return Ok(());
        }
        live.scheduled = true;
        Self::cap(&mut streams);
        let owner = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(owner) = owner.upgrade()
                && let Some(live) = owner.0.lock().unwrap().get_mut(&key)
            {
                live.scheduled = false;
                live.published.send_replace(Publication {
                    tail: live.tail.checkpoint(),
                    recent: live.recent.clone(),
                    overflowed_through: live.overflowed_through,
                });
            }
        });
        Ok(())
    }
    fn cap(streams: &mut HashMap<records::OutputKey, Live>) {
        while streams.len() > 64 {
            let oldest = streams
                .iter()
                .min_by_key(|(_, live)| live.written)
                .map(|(key, _)| key.clone())
                .expect("live command limit exceeded");
            streams.remove(&oldest);
        }
    }
    pub(super) fn finish(&self, key: &records::OutputKey, final_text: &str) -> Output {
        let live = self.0.lock().unwrap().remove(key);
        if final_text.is_empty()
            && let Some(live) = live
        {
            return live.tail.checkpoint();
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
                (key, live.tail.checkpoint())
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
        Self::cap(&mut streams);
        let live = streams.get(key)?;
        Some((
            live.tail.checkpoint(),
            Subscription {
                receiver: live.published.subscribe(),
                sequence: live.tail.sequence(),
                next_frame: None,
            },
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn fast_and_lagging_appends_fold_to_the_same_terminal() {
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
        assert!(matches!(update, OutputUpdate::Append { .. }));
        slow = slow.apply(update).unwrap();
        assert_eq!(fast, slow);
        assert_eq!(fast.text, "progress 99%");
        assert_eq!(outputs.finish(&key, "").text, fast.text);
        assert!(watching.recv().await.is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn late_and_once_lagging_viewers_return_to_append_cadence() {
        let outputs = Arc::new(Outputs::default());
        let execution = Uuid::new_v4();
        let key = records::OutputKey {
            session: SessionRef::new(None, execution.to_string()).unwrap(),
            item: "command".into(),
        };
        outputs
            .write(execution, key.clone(), Change::Append("before expanding\n"))
            .unwrap();
        let (mut folded, mut subscription) = outputs.subscribe(&key, None).unwrap();
        for index in 0..12 {
            outputs
                .write(execution, key.clone(), Change::Append("next\n"))
                .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
            if index == 4 {
                continue;
            } // miss one publication, then catch up
            let before = Instant::now();
            let update: OutputUpdate =
                protocol::decode(&subscription.recv().await.unwrap()).unwrap();
            assert_eq!(
                Instant::now(),
                before,
                "ordinary catch-up must not add 500ms"
            );
            assert!(
                matches!(update, OutputUpdate::Append { .. }),
                "late and once-lagging viewers retain every small append"
            );
            folded = folded.apply(update).unwrap();
        }
        assert_eq!(folded.text, outputs.finish(&key, "").text);
        assert!(subscription.recv().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn live_limit_evicts_the_least_recently_written_command_and_ends_its_stream() {
        let outputs = Arc::new(Outputs::default());
        let execution = Uuid::new_v4();
        let key = |n| records::OutputKey {
            session: SessionRef::new(None, execution.to_string()).unwrap(),
            item: format!("command-{n}").into(),
        };
        let (_, mut oldest) = outputs
            .subscribe(&key(0), Some((execution, Output::default())))
            .unwrap();
        tokio::time::advance(Duration::from_millis(1)).await;
        let (_, mut touched) = outputs
            .subscribe(&key(1), Some((execution, Output::default())))
            .unwrap();
        for n in 2..65 {
            tokio::time::advance(Duration::from_millis(1)).await;
            outputs
                .write(execution, key(n), Change::Append("x"))
                .unwrap();
            if n == 63 {
                outputs
                    .write(execution, key(1), Change::Append("recent"))
                    .unwrap();
            }
        }
        assert!(oldest.recv().await.is_none());
        assert!(outputs.subscribe(&key(0), None).is_none());
        assert!(outputs.subscribe(&key(1), None).is_some());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(touched.recv().await.is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn append_budget_counts_utf16_and_only_overflow_uses_the_longer_pause() {
        for units in [16 * 1024, 16 * 1024 + 1] {
            let outputs = Arc::new(Outputs::default());
            let execution = Uuid::new_v4();
            let key = records::OutputKey {
                session: SessionRef::new(None, execution.to_string()).unwrap(),
                item: "unicode".into(),
            };
            let (initial, mut stream) = outputs
                .subscribe(&key, Some((execution, Output::default())))
                .unwrap();
            outputs
                .write(execution, key.clone(), Change::Append(&"日".repeat(units)))
                .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            let update: OutputUpdate = protocol::decode(&stream.recv().await.unwrap()).unwrap();
            assert_eq!(
                matches!(update, OutputUpdate::Append { .. }),
                units == 16 * 1024
            );
            let folded = initial.apply(update).unwrap();
            outputs
                .write(execution, key.clone(), Change::Append("!"))
                .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            let before = Instant::now();
            let update: OutputUpdate = protocol::decode(&stream.recv().await.unwrap()).unwrap();
            assert_eq!(
                Instant::now() - before,
                Duration::from_millis(if units > 16 * 1024 { 400 } else { 0 })
            );
            assert!(matches!(update, OutputUpdate::Append { .. }));
            assert_eq!(
                folded.apply(update).unwrap().text,
                outputs.finish(&key, "").text
            );
        }
    }
}
