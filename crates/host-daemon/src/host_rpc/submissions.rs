//! Bounded, process-local send receipts. Nothing is persisted or replayed.
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::watch;

const CAPACITY: usize = 1024;
const RETENTION: Duration = Duration::from_secs(15 * 60);
type Outcome = Result<String, String>;
#[derive(Default)]
pub(super) struct Submissions(Mutex<HashMap<(String, String), Entry>>);
struct Entry {
    fingerprint: [u8; 32],
    at: Instant,
    outcome: watch::Sender<Option<Outcome>>,
}
pub(super) enum Admission {
    New(Receipt),
    Existing(watch::Receiver<Option<Outcome>>),
}
pub(super) struct Receipt(watch::Sender<Option<Outcome>>);
impl Receipt {
    pub(super) fn finish(self, result: Outcome) {
        self.0.send_replace(Some(result));
    }
}
impl Drop for Receipt {
    fn drop(&mut self) {
        if self.0.borrow().is_none() {
            self.0.send_replace(Some(Err(
                "submission delivery is unknown; do not automatically resend".into(),
            )));
        }
    }
}
impl Submissions {
    pub(super) fn begin(&self, method: &str, params: &Value) -> Result<Admission, &'static str> {
        let id = params["clientUserMessageId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or("clientUserMessageId is required")?;
        let target = params["threadId"]
            .as_str()
            .ok_or("session ID is required")?;
        let target = agent_core::session::SessionRef::from_thread_id(target)?;
        let key = (target.thread_id(), id.to_owned());
        let fingerprint: [u8; 32] = ring::digest::digest(
            &ring::digest::SHA256,
            format!("{method}:{params}").as_bytes(),
        )
        .as_ref()
        .try_into()
        .unwrap();
        let mut entries = self.0.lock().unwrap();
        if let Some(entry) = entries.get(&key) {
            if entry.fingerprint != fingerprint {
                return Err("submission ID was already used with different input or execution");
            }
            return Ok(Admission::Existing(entry.outcome.subscribe()));
        }
        entries
            .retain(|_, entry| entry.at.elapsed() < RETENTION || entry.outcome.borrow().is_none());
        if entries.len() >= CAPACITY {
            return Err("send receipt capacity reached; receipts are retained for 15 minutes");
        }
        let (outcome, _) = watch::channel(None);
        entries.insert(
            key,
            Entry {
                fingerprint,
                at: Instant::now(),
                outcome: outcome.clone(),
            },
        );
        Ok(Admission::New(Receipt(outcome)))
    }
}
impl Admission {
    pub(super) async fn previous(mut receiver: watch::Receiver<Option<Outcome>>) -> Outcome {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "submission outcome is unknown".to_owned())?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn simultaneous_duplicate_receives_one_receipt_and_changed_input_is_rejected() {
        let cache = Submissions::default();
        let input =
            json!({"threadId":"native", "clientUserMessageId":"send", "input":[{"text":"hello"}]});
        let Admission::New(receipt) = cache.begin("turn/start", &input).unwrap() else {
            panic!()
        };
        let Admission::Existing(waiting) = cache.begin("turn/start", &input).unwrap() else {
            panic!()
        };
        assert!(cache.begin("turn/steer", &input).is_err());
        receipt.finish(Ok("accepted".into()));
        assert_eq!(Admission::previous(waiting).await.unwrap(), "accepted");
    }
    #[tokio::test]
    async fn cancellation_retains_unknown_outcome() {
        let cache = Submissions::default();
        let input = json!({"threadId":"native", "clientUserMessageId":"send"});
        let Admission::New(receipt) = cache.begin("turn/start", &input).unwrap() else {
            panic!()
        };
        drop(receipt);
        let Admission::Existing(waiting) = cache.begin("turn/start", &input).unwrap() else {
            panic!()
        };
        assert!(
            Admission::previous(waiting)
                .await
                .unwrap_err()
                .contains("unknown")
        );
    }
}
