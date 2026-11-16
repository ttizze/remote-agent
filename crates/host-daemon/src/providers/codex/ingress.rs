//! T3 ProviderTextDeltaCoalescer: raw JSONL reception only updates memory and
//! enqueues ordered batches. Host persistence and its receipts run downstream.
use agent_transport::peer::{PeerEvent, Reply};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::Instant};

// CodexAdapterV2.ts at 1a12c4af uses 50ms for assistant and reasoning text.
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);

pub(super) struct Batch {
    pub events: Vec<PeerEvent>,
    pub through: Option<u64>,
}
impl Batch {
    fn single(event: PeerEvent) -> Self {
        let through = match &event {
            PeerEvent::Message(message) => Some(message.sequence),
            PeerEvent::Response { sequence } => Some(*sequence),
            PeerEvent::Closed(_) => None,
        };
        Self {
            events: vec![event],
            through,
        }
    }
}
struct Text {
    envelope: Value,
    value: String,
    sequence: u64,
}
#[derive(Default)]
struct Pending {
    text: Vec<Text>,
    deadline: Option<Instant>,
}
impl Pending {
    fn append(&mut self, event: &PeerEvent) -> bool {
        let PeerEvent::Message(message) = event else {
            return false;
        };
        let Ok(mut envelope) = serde_json::from_str::<Value>(&message.value) else {
            return false;
        };
        if envelope.get("id").is_some()
            || !matches!(
                envelope["method"].as_str(),
                Some(
                    "item/agentMessage/delta"
                        | "item/reasoning/textDelta"
                        | "item/reasoning/summaryTextDelta"
                )
            )
        {
            return false;
        }
        let Some(params) = envelope.get_mut("params").and_then(Value::as_object_mut) else {
            return false;
        };
        let Some(Value::String(delta)) = params.remove("delta") else {
            return false;
        };
        if let Some(pending) = self
            .text
            .iter_mut()
            .find(|pending| pending.envelope == envelope)
        {
            pending.value.push_str(&delta);
            pending.sequence = message.sequence;
        } else {
            self.text.push(Text {
                envelope,
                value: delta,
                sequence: message.sequence,
            });
        }
        self.deadline
            .get_or_insert_with(|| Instant::now() + FLUSH_INTERVAL);
        true
    }
    fn flush(&mut self, output: &mpsc::UnboundedSender<Batch>) -> bool {
        self.deadline = None;
        if self.text.is_empty() {
            return true;
        }
        let through = self.text.iter().map(|text| text.sequence).max();
        let events = std::mem::take(&mut self.text)
            .into_iter()
            .map(|mut text| {
                text.envelope["params"]["delta"] = Value::String(text.value);
                PeerEvent::Message(Reply {
                    sequence: text.sequence,
                    value: Arc::from(
                        serde_json::to_string(&text.envelope).expect("JSON value serializes"),
                    ),
                })
            })
            .collect();
        output.send(Batch { events, through }).is_ok()
    }
}

pub(super) fn receive(
    mut events: mpsc::UnboundedReceiver<PeerEvent>,
) -> mpsc::UnboundedReceiver<Batch> {
    let (output, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut pending = Pending::default();
        loop {
            let deadline = pending.deadline.unwrap_or_else(Instant::now);
            tokio::select! {
                _=output.closed()=>return,
                _=tokio::time::sleep_until(deadline),if pending.deadline.is_some()=>{
                    if !pending.flush(&output){return;}
                }
                event=events.recv()=>{
                    let Some(event)=event else {pending.flush(&output);return};
                    if pending.append(&event){continue;}
                    // Control, completion, request and response records fence
                    // every earlier delta. Different item/part keys never mix.
                    if !pending.flush(&output) || output.send(Batch::single(event)).is_err(){return;}
                }
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests;
