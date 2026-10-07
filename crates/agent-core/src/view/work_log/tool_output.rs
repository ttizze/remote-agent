//! Identities and failure flags read from a tool's MCP result, and command
//! output that describes a failure.
//!
//! The Host compacts dynamic tool output with its own copy of this reader
//! before delivery; this copy reads outputs fetched with the item detail.
use crate::js_text::{JS_SPACE, js_trim, utf16_len};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::sync::LazyLock;

const MAX_PARSED_BYTES: usize = 16_384;
const MAX_METADATA_BYTES: usize = 8_192;
const MAX_ID_LENGTH: usize = 256;
const MAX_THREADS: usize = 100;
const MAX_CONTENT_BLOCKS: usize = 32;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: i64 = 128;

/// One entry of a thread creation batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactThread {
    pub thread_id: Option<String>,
    pub rolled_back: bool,
}

/// The IDs and failure metadata grouped tool summaries read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactToolOutput {
    pub is_error: bool,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
    pub task_id: Option<String>,
    /// `status: "rolled_back"`.
    pub rolled_back: bool,
    /// `thread.threadId`.
    pub thread: Option<String>,
    pub threads: Option<Vec<CompactThread>>,
}

impl CompactToolOutput {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The result object the summaries read.
    pub fn to_json(&self) -> Value {
        let mut output = Map::new();
        if self.is_error {
            output.insert("isError".into(), true.into());
        }
        for (key, id) in [
            ("threadId", &self.thread_id),
            ("messageId", &self.message_id),
            ("taskId", &self.task_id),
        ] {
            if let Some(id) = id {
                output.insert(key.into(), id.as_str().into());
            }
        }
        if self.rolled_back {
            output.insert("status".into(), "rolled_back".into());
        }
        if let Some(thread) = &self.thread {
            output.insert("thread".into(), json!({ "threadId": thread }));
        }
        if let Some(threads) = &self.threads {
            let threads = threads.iter().map(|thread| {
                let mut entry = Map::new();
                if let Some(id) = &thread.thread_id {
                    entry.insert("threadId".into(), id.as_str().into());
                }
                if thread.rolled_back {
                    entry.insert("status".into(), "rolled_back".into());
                }
                Value::Object(entry)
            });
            output.insert("threads".into(), threads.collect());
        }
        Value::Object(output)
    }
}

struct ReadBudget {
    remaining_bytes: usize,
    remaining_nodes: i64,
    exceeded: bool,
}

#[derive(Default)]
struct Envelope<'a> {
    data: Option<Cow<'a, Map<String, Value>>>,
    failed: bool,
}

/// JavaScript `typeof value === "object" && value !== null`.
fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

/// Walks MCP result envelopes for the data object and failure flags.
fn read_result<'a>(
    value: Option<&'a Value>,
    budget: &mut ReadBudget,
    depth: usize,
) -> Envelope<'a> {
    budget.remaining_nodes -= 1;
    if depth > MAX_ENVELOPE_DEPTH || budget.remaining_nodes < 0 {
        budget.exceeded = true;
        return Envelope::default();
    }
    match value {
        Some(Value::String(text)) => {
            if utf16_len(text) > budget.remaining_bytes || text.len() > budget.remaining_bytes {
                budget.exceeded = true;
                return Envelope::default();
            }
            budget.remaining_bytes -= text.len();
            let Ok(parsed) = serde_json::from_str::<Value>(text) else {
                return Envelope::default();
            };
            let nested = read_result(Some(&parsed), budget, depth + 1);
            Envelope {
                data: nested.data.map(|data| Cow::Owned(data.into_owned())),
                failed: nested.failed,
            }
        }
        Some(Value::Array(blocks)) => {
            if blocks.len() > MAX_CONTENT_BLOCKS {
                budget.exceeded = true;
                return Envelope::default();
            }
            let mut envelope = Envelope::default();
            for block in blocks {
                let text = block.as_object().and_then(|block| block.get("text"));
                let text = match text {
                    Some(text) if is_object(text) => match text.get("text") {
                        Some(inner) if !inner.is_null() => Some(inner),
                        _ => Some(text),
                    },
                    text => text,
                };
                let result = read_result(text, budget, depth + 1);
                if envelope.data.is_none() {
                    envelope.data = result.data;
                }
                envelope.failed |= result.failed;
                if budget.exceeded {
                    break;
                }
            }
            envelope
        }
        Some(Value::Object(object)) => {
            let failed = object.get("isError") == Some(&Value::Bool(true))
                || object.get("is_error") == Some(&Value::Bool(true))
                || object.get("_tag").and_then(Value::as_str) == Some("OrchestratorMcpFailure")
                || object.get("error").is_some_and(|error| !error.is_null());
            let content = match object.get("structuredContent") {
                Some(content) if !content.is_null() => Some(content),
                _ => object.get("content"),
            };
            match content {
                Some(content) => {
                    let nested = read_result(Some(content), budget, depth + 1);
                    Envelope {
                        data: nested.data,
                        failed: failed || nested.failed,
                    }
                }
                None => Envelope {
                    data: Some(Cow::Borrowed(object)),
                    failed,
                },
            }
        }
        _ => Envelope::default(),
    }
}

fn bounded_id(value: Option<&Value>) -> Option<String> {
    let id = value?.as_str()?;
    (utf16_len(id) <= MAX_ID_LENGTH && !js_trim(id).is_empty()).then(|| id.into())
}

/// Keeps only the IDs and failure metadata grouped tool summaries read.
pub fn compact_dynamic_tool_output(value: Option<&Value>) -> Option<CompactToolOutput> {
    let mut budget = ReadBudget {
        remaining_bytes: MAX_PARSED_BYTES,
        remaining_nodes: MAX_ENVELOPE_NODES,
        exceeded: false,
    };
    let result = read_result(value, &mut budget, 0);
    let mut output = CompactToolOutput {
        is_error: result.failed,
        ..CompactToolOutput::default()
    };
    if let Some(data) = result.data.filter(|_| !budget.exceeded) {
        output.thread_id = bounded_id(data.get("threadId"));
        output.message_id = bounded_id(data.get("messageId"));
        output.task_id = bounded_id(data.get("taskId"));
        output.rolled_back = data.get("status").and_then(Value::as_str) == Some("rolled_back");
        output.thread = data
            .get("thread")
            .filter(|thread| is_object(thread))
            .and_then(|thread| bounded_id(thread.get("threadId")));
        if let Some(Value::Array(entries)) = data.get("threads") {
            let threads = (entries.len() <= MAX_THREADS)
                .then(|| {
                    entries
                        .iter()
                        .map(|entry| {
                            if !is_object(entry) {
                                return None;
                            }
                            let thread_id = bounded_id(entry.get("threadId"));
                            let rolled_back =
                                entry.get("status").and_then(Value::as_str) == Some("rolled_back");
                            (thread_id.is_some() || rolled_back).then_some(CompactThread {
                                thread_id,
                                rolled_back,
                            })
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .flatten();
            if threads.is_some() {
                output.threads = threads;
            } else {
                // A partial batch or a top-level ID would turn an unknown creation
                // count into a confidently wrong one in the summary.
                output.thread_id = None;
                output.rolled_back = false;
            }
        }
    }
    if serde_json::to_vec(&output.to_json()).map_or(0, |json| json.len()) > MAX_METADATA_BYTES {
        output.threads = None;
        output.thread_id = None;
        output.rolled_back = false;
    }
    (!output.is_empty()).then_some(output)
}

static FAILURE_PATTERNS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        r"(?i)file not found|no files found|enoent|no such file|commandnotfoundexception|command not found|is not recognized as the name of a cmdlet|a parameter cannot be found that matches parameter name".to_owned(),
        format!(r"(?i)<exited with exit code{JS_SPACE}+[1-9][0-9]*{JS_SPACE}*>"),
        format!(r"(?i)exit(?:ed)? with exit code{JS_SPACE}+[1-9][0-9]*"),
        format!(r"(?i)exit code{JS_SPACE}*(?:[:]|{JS_SPACE}){JS_SPACE}*[1-9][0-9]*(?-u:\b)"),
    ]
    .map(|pattern| Regex::new(&pattern).expect("failure pattern compiles"))
});

/// Some providers report completion even when command output describes a failure.
pub fn tool_output_indicates_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    FAILURE_PATTERNS[0].is_match(text)
        || (lower.contains("cannot find path") && lower.contains("because it does not exist"))
        || (lower.contains("is not recognized") && lower.contains("the term '"))
        || FAILURE_PATTERNS[1..]
            .iter()
            .any(|pattern| pattern.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compact(value: Value) -> Option<Value> {
        compact_dynamic_tool_output(Some(&value)).map(|output| output.to_json())
    }

    #[test]
    fn extracts_ids_through_the_mcp_result_envelopes_used_by_summaries() {
        let metadata = json!({"threadId": "thread-1", "messageId": "message-1"});
        let json =
            json!({"threadId": "thread-1", "messageId": "message-1", "response": "Private response body"})
                .to_string();
        for value in [
            json!({"threadId": "thread-1", "messageId": "message-1", "response": "Private response body"}),
            json!({"structuredContent": serde_json::from_str::<Value>(&json).unwrap(), "content": "Ignored alternative body"}),
            json!([{"type": "text", "text": json}]),
            json!({"content": [{"text": {"text": json}}], "isError": false}),
            Value::String(json.clone()),
        ] {
            assert_eq!(compact(value), Some(metadata.clone()));
        }
    }

    #[test]
    fn keeps_nested_thread_identity_and_scalar_task_ids_without_result_bodies() {
        let output = json!({
            "thread": {"threadId": "thread-1", "messages": ["Private message"]},
            "taskId": "task-1",
            "status": "failed",
            "summary": "A child failed; the tool itself succeeded.",
        });
        assert_eq!(
            compact(output.clone()),
            Some(json!({"thread": {"threadId": "thread-1"}, "taskId": "task-1"}))
        );
        assert_eq!(output["thread"]["messages"], json!(["Private message"]));
    }

    #[test]
    fn keeps_explicit_envelope_failure_while_dropping_error_text_and_arbitrary_output() {
        for failure in [
            json!({"isError": true}),
            json!({"is_error": true}),
            json!({"_tag": "OrchestratorMcpFailure"}),
            json!({"error": {"message": "Private error"}}),
        ] {
            let mut value = failure.as_object().unwrap().clone();
            value.insert(
                "structuredContent".into(),
                json!({"threadId": "thread-1", "error": "Private details"}),
            );
            assert_eq!(
                compact(Value::Object(value)),
                Some(json!({"isError": true, "threadId": "thread-1"}))
            );
        }
        assert_eq!(compact(json!("Private plain text")), None);
        assert_eq!(compact(json!({"result": {"body": "Private result"}})), None);
    }

    #[test]
    fn uses_the_first_result_data_but_combines_explicit_failure_flags_across_content_blocks() {
        assert_eq!(
            compact(json!([
                {"text": json!({"threadId": "first"}).to_string()},
                {"text": json!({"threadId": "second", "isError": true, "error": "Private error"}).to_string()},
            ])),
            Some(json!({"threadId": "first", "isError": true}))
        );
        assert_eq!(
            compact(json!([{"text": "{}"}, {"text": json!({"threadId": "second"}).to_string()}])),
            None
        );
    }

    #[test]
    fn keeps_complete_thread_creation_batches_and_rollback_markers_without_row_bodies() {
        assert_eq!(
            compact(json!({
                "threads": [
                    {"threadId": "created", "status": "running", "messages": ["Private message"]},
                    {"threadId": "reverted", "status": "rolled_back", "error": "Private error"},
                    {"status": "rolled_back"},
                ],
            })),
            Some(json!({
                "threads": [
                    {"threadId": "created"},
                    {"threadId": "reverted", "status": "rolled_back"},
                    {"status": "rolled_back"},
                ],
            }))
        );
        assert_eq!(
            compact(json!({"threadId": "reverted", "status": "rolled_back"})),
            Some(json!({"threadId": "reverted", "status": "rolled_back"}))
        );
        assert_eq!(
            compact(json!({"threads": []})),
            Some(json!({"threads": []}))
        );
    }

    #[test]
    fn omits_incomplete_or_oversized_creation_evidence_instead_of_returning_a_partial_count() {
        for threads in [
            json!([{"threadId": "known"}, {"title": "No confirmed ID"}]),
            Value::Array(
                (0..101)
                    .map(|index| json!({"threadId": format!("thread-{index}")}))
                    .collect(),
            ),
            Value::Array(
                (0..100)
                    .map(|index| json!({"threadId": format!("{index:x<256}")}))
                    .collect(),
            ),
            json!([{"threadId": "x".repeat(257)}]),
        ] {
            assert_eq!(
                compact(json!({
                    "threadId": "must-not-be-counted-as-one",
                    "status": "rolled_back",
                    "taskId": "task-1",
                    "threads": threads,
                })),
                Some(json!({"taskId": "task-1"}))
            );
        }
        let normal_batch = json!({
            "threads": (0..100)
                .map(|index| json!({"threadId": format!("00000000-0000-4000-8000-{index:012}")}))
                .collect::<Vec<_>>(),
        });
        let compacted = compact(normal_batch.clone());
        assert_eq!(compacted, Some(normal_batch));
        assert!(serde_json::to_vec(&compacted).unwrap().len() <= 8_192);
    }

    #[test]
    fn bounds_parsing_and_envelope_scanning_while_preserving_an_outer_failure_marker() {
        let oversized_json = json!({"threadId": "hidden", "body": "😄".repeat(5_000)}).to_string();
        for content in [
            Value::String("x".repeat(1_000_000)),
            Value::String(oversized_json),
            Value::Array(
                (0..33)
                    .map(|_| json!({"text": json!({"threadId": "hidden"}).to_string()}))
                    .collect(),
            ),
            json!({"content": {"content": {"content": {"content": {"threadId": "too-deep"}}}}}),
        ] {
            assert_eq!(
                compact(json!({"isError": true, "content": content})),
                Some(json!({"isError": true}))
            );
        }
        assert_eq!(
            compact(json!({"threadId": "known", "body": "x".repeat(1_000_000)})),
            Some(json!({"threadId": "known"}))
        );
    }

    #[test]
    fn is_idempotent_for_compact_metadata() {
        let compacted = compact_dynamic_tool_output(Some(&json!({
            "isError": true,
            "structuredContent": {
                "threadId": "thread-1",
                "threads": [{"threadId": "thread-1"}, {"status": "rolled_back"}],
                "output": "Private output",
            },
        })))
        .unwrap();
        assert_eq!(
            compact_dynamic_tool_output(Some(&compacted.to_json())),
            Some(compacted)
        );
    }

    #[test]
    fn preserves_the_command_failure_phrases_across_shells_without_copying_the_output() {
        for text in [
            "FILE NOT FOUND",
            "No files found",
            "ENOENT",
            "No such file or directory",
            "CommandNotFoundException",
            "command not found",
            "Cannot find path 'a' because it does not exist",
            "The term 'example' is not recognized",
            "is not recognized as the name of a cmdlet",
            "A parameter cannot be found that matches parameter name",
            "<exited with exit code 2>",
            "Exited with exit code 1",
            "exit code: 127",
        ] {
            assert!(tool_output_indicates_failure(text), "{text}");
        }
    }

    #[test]
    fn does_not_mark_successful_exit_codes_or_incomplete_failure_phrases() {
        for text in [
            "Done",
            "exit code: 0",
            "Exited with exit code 0",
            "cannot find path",
            "is not recognized",
        ] {
            assert!(!tool_output_indicates_failure(text), "{text}");
        }
    }
}
