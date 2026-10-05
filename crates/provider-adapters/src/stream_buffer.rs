//! Bound provider delta batches before normalization allocates full records.
use serde_json::Value;

pub(crate) const WINDOW: std::time::Duration = std::time::Duration::from_millis(50);
const MAX_BYTES: usize = 64 * 1024;
#[derive(Default)]
pub(crate) struct DeltaBuffer {
    pending: Option<(String, &'static str, Value)>,
}
fn delta(frame: &Value) -> Option<(String, &'static str)> {
    let method = frame["method"].as_str().unwrap_or("");
    if matches!(
        method,
        "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta"
            | "item/plan/delta"
            | "item/commandExecution/outputDelta"
    ) && frame["params"]["delta"].is_string()
    {
        let mut identity = frame.clone();
        identity["params"]["delta"] = Value::Null;
        return Some((identity.to_string(), "/params/delta"));
    }
    if frame["type"] == "stream_event" && frame["event"]["type"] == "content_block_delta" {
        let pointer = match frame["event"]["delta"]["type"].as_str()? {
            "text_delta" => "/event/delta/text",
            "thinking_delta" => "/event/delta/thinking",
            _ => return None,
        };
        if frame.pointer(pointer)?.is_string() {
            return Some((
                format!(
                    "claude:{}:{}:{}",
                    frame["parent_tool_use_id"], frame["event"]["index"], pointer
                ),
                pointer,
            ));
        }
    }
    None
}
impl DeltaBuffer {
    pub fn push(&mut self, frame: Value) -> Vec<Value> {
        let mut ready = vec![];
        let Some((key, pointer)) = delta(&frame) else {
            ready.extend(self.flush());
            ready.push(frame);
            return ready;
        };
        if self.pending.as_ref().is_some_and(|(old, _, _)| old != &key) {
            ready.extend(self.flush());
        }
        if let Some((_, pointer, previous)) = &mut self.pending {
            let text = previous.pointer_mut(pointer).expect("pending delta");
            let Value::String(text) = text else {
                unreachable!("string delta")
            };
            text.push_str(
                frame
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .expect("delta"),
            );
        } else {
            self.pending = Some((key, pointer, frame));
        }
        if self.pending.as_ref().is_some_and(|(_, pointer, frame)| {
            frame
                .pointer(pointer)
                .and_then(Value::as_str)
                .is_some_and(|text| text.len() >= MAX_BYTES)
        }) {
            ready.extend(self.flush());
        }
        ready
    }
    pub fn flush(&mut self) -> Option<Value> {
        self.pending.take().map(|(_, _, frame)| frame)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn timer_flushes_store_text_linearly_even_for_a_steady_stream() {
        for method in [
            "item/agentMessage/delta",
            "item/plan/delta",
            "item/commandExecution/outputDelta",
        ] {
            let mut state = crate::normalize::tests::state(orchestration::Driver::Codex);
            if method == "item/commandExecution/outputDelta" {
                state = crate::normalize::codex(state, "item/started", &json!({"item":{"id":"text", "type":"commandExecution", "command":"test", "aggregatedOutput":""}}), None, &crate::now()).state;
            }
            let mut buffer = DeltaBuffer::default();
            let mut timer = tokio::time::interval(WINDOW);
            timer.tick().await;
            let text = "日本語 streaming ".repeat(64);
            let mut bytes = 0;
            let mut deltas = 0;
            for _ in 0..24 {
                assert!(
                    buffer
                        .push(json!({"method":method,"params":{"itemId":"text","delta":text}}))
                        .is_empty()
                );
                timer.tick().await;
                let frame = buffer.flush().expect("timer flush");
                let translated =
                    crate::normalize::codex(state, method, &frame["params"], None, &crate::now());
                bytes += serde_json::to_vec(&translated.payloads).unwrap().len();
                deltas += translated
                    .payloads
                    .iter()
                    .filter(|p| matches!(p, orchestration::EventPayload::TurnItemTextDelta(_)))
                    .count();
                state = translated.state;
            }
            let final_size = text.len() * 24;
            assert!(deltas >= 23, "{method}: {deltas} deltas");
            assert!(
                bytes < final_size * 3,
                "{method}: stored {bytes} for {final_size} bytes"
            );
            let actual = match &state.items["text"].body {
                orchestration::TurnItemBody::AssistantMessage { text, .. } => text,
                orchestration::TurnItemBody::ProposedPlan { markdown, .. } => markdown,
                orchestration::TurnItemBody::CommandExecution {
                    output: Some(output),
                    ..
                } => output,
                _ => panic!("streaming item"),
            };
            assert_eq!(actual, &text.repeat(24));
        }
    }
    #[test]
    fn megabyte_burst_is_exact_and_does_not_persist_ten_thousand_snapshots() {
        for method in [
            "item/agentMessage/delta",
            "item/commandExecution/outputDelta",
        ] {
            let mut buffer = DeltaBuffer::default();
            let mut text = String::new();
            let mut snapshots = 0;
            let mut serialized_text = 0;
            for _ in 0..10_000 {
                for frame in buffer.push(json!({"method":method,"params":{"threadId":"thread","itemId":"item","delta":"x".repeat(100)}})) {
                    assert_eq!(frame["method"], method);
                    text.push_str(frame["params"]["delta"].as_str().unwrap());
                    snapshots += 1;
                    serialized_text += text.len();
                }
            }
            if let Some(frame) = buffer.flush() {
                text.push_str(frame["params"]["delta"].as_str().unwrap());
                snapshots += 1;
                serialized_text += text.len();
            }
            assert_eq!(text, "x".repeat(1_000_000));
            assert!(snapshots < 20);
            assert!(serialized_text < 10_000_000);
        }
    }
    #[test]
    fn barriers_and_parallel_items_preserve_provider_order() {
        let mut buffer = DeltaBuffer::default();
        let a = json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"one"}}});
        assert!(buffer.push(a.clone()).is_empty());
        let mut b = a;
        b["event"]["index"] = json!(1);
        assert_eq!(buffer.push(b)[0]["event"]["index"], 0);
        let frames = buffer.push(json!({"type":"result"}));
        assert_eq!(frames[0]["event"]["index"], 1);
        assert_eq!(frames[1]["type"], "result");
        assert!(buffer.flush().is_none());
    }
}
