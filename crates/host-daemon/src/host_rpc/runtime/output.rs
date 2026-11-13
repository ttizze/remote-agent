//! Live command bytes belong to their resource, outside durable row sync.
//! A watch channel bounds every slow subscriber to one 64 KiB tail.
use super::*;
use agent_protocol::protocol;
use agent_protocol::sync::{Output, OutputUpdate};
use tokio::sync::watch;

#[derive(Default)]
pub(super) struct Outputs(Mutex<HashMap<records::OutputKey, Live>>);
struct Live {
    execution: Uuid,
    tail: Output,
    published: watch::Sender<Output>,
    scheduled: bool,
}

pub struct Subscription {
    receiver: watch::Receiver<Output>,
    previous: Output,
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.receiver.changed().await.ok()?;
        let next = self.receiver.borrow_and_update().clone();
        let end = self.previous.offset + self.previous.text.len() as u64;
        let update = if end >= next.offset && end <= next.offset + next.text.len() as u64 {
            let start = usize::try_from(end - next.offset).ok()?;
            match next.text.get(start..) {
                Some(text) => OutputUpdate::Append {
                    offset: end,
                    text: text.into(),
                },
                None => OutputUpdate::Tail(next.clone()),
            }
        } else {
            OutputUpdate::Tail(next.clone())
        };
        self.previous = next;
        protocol::encode(update).ok()
    }
}

impl Outputs {
    pub(super) fn update(
        self: &Arc<Self>,
        execution: Uuid,
        key: records::OutputKey,
        update: OutputUpdate,
    ) -> Result<(), String> {
        let mut streams = self.0.lock().unwrap();
        let live = streams.entry(key.clone()).or_insert_with(|| Live {
            execution,
            tail: Output::default(),
            published: watch::channel(Output::default()).0,
            scheduled: false,
        });
        live.tail = live.tail.apply(update).map_err(|error| error.to_string())?;
        if live.scheduled {
            return Ok(());
        }
        live.scheduled = true;
        let owner = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(owner) = owner.upgrade()
                && let Some(live) = owner.0.lock().unwrap().get_mut(&key)
            {
                live.scheduled = false;
                live.published.send_replace(live.tail.clone());
            }
        });
        Ok(())
    }

    pub(super) fn append(
        self: &Arc<Self>,
        execution: Uuid,
        key: records::OutputKey,
        text: &str,
    ) -> Result<(), String> {
        let offset = self
            .0
            .lock()
            .unwrap()
            .get(&key)
            .map_or(0, |live| live.tail.offset + live.tail.text.len() as u64);
        self.update(
            execution,
            key,
            OutputUpdate::Append {
                offset,
                text: text.into(),
            },
        )
    }

    pub(super) fn finish(&self, key: &records::OutputKey, final_text: &str) -> Output {
        let stored = Output::default()
            .apply(OutputUpdate::Tail(Output {
                offset: 0,
                text: final_text.into(),
            }))
            .expect("output length fits u64");
        let Some(live) = self.0.lock().unwrap().remove(key) else {
            return stored;
        };
        if stored.offset + stored.text.len() as u64
            >= live.tail.offset + live.tail.text.len() as u64
        {
            stored
        } else {
            live.tail
        }
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
            streams.entry(key.clone()).or_insert_with(|| Live {
                execution,
                published: watch::channel(tail.clone()).0,
                tail,
                scheduled: false,
            });
        }
        let live = streams.get(key)?;
        Some((
            live.tail.clone(),
            Subscription {
                receiver: live.published.subscribe(),
                previous: live.tail.clone(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn subscribing_before_the_first_live_bytes_keeps_the_running_output_watch_open() {
        let outputs = Arc::new(Outputs::default());
        let execution = Uuid::new_v4();
        let key = records::OutputKey {
            session: SessionRef::new(None, Uuid::new_v4().to_string()).unwrap(),
            item: "command".into(),
        };
        let (initial, mut watch) = outputs
            .subscribe(
                &key,
                Some((
                    execution,
                    Output {
                        offset: 0,
                        text: "saved".into(),
                    },
                )),
            )
            .unwrap();
        assert_eq!(initial.text, "saved");
        outputs.append(execution, key.clone(), " new").unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        let update: OutputUpdate = protocol::decode(&watch.recv().await.unwrap()).unwrap();
        assert_eq!(initial.apply(update).unwrap().text, "saved new");
        assert_eq!(outputs.finish(&key, "saved new").text, "saved new");
        assert!(watch.recv().await.is_none());
    }
}
