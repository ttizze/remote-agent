//! Group summaries of orchestration tool calls: what succeeded, counted by
//! the entities the calls touched.
use super::tool_catalog::ToolSummaryAction;
use crate::js_text::js_trim;
use crate::view::quantity;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

type Record = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCallOutcome {
    Completed,
    Failed,
    Unfinished,
}

/// One call's input and output as the provider retained them.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSummaryCall {
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub outcome: ToolCallOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ToolCallsSummary {
    pub label: String,
    pub failed_count: u32,
}

fn as_record(value: Option<&Value>) -> Option<&Record> {
    value.and_then(Value::as_object)
}

fn id(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !js_trim(value).is_empty())
}

#[derive(Default)]
struct ToolResult<'a> {
    data: Option<Cow<'a, Record>>,
    failed: bool,
}

static FAILURE_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?:Error|Failure)$").expect("failure tag compiles"));

/// Reads the structured/JSON MCP result envelopes the provider adapters retain.
fn read_result(value: Option<&Value>, depth: usize) -> ToolResult<'_> {
    if depth > 4 {
        return ToolResult::default();
    }
    match value {
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => {
                let result = read_result(Some(&parsed), depth + 1);
                ToolResult {
                    data: result.data.map(|data| Cow::Owned(data.into_owned())),
                    failed: result.failed,
                }
            }
            Err(_) => ToolResult::default(),
        },
        Some(Value::Array(blocks)) => {
            let mut result = ToolResult::default();
            for block in blocks {
                let record = block.as_object();
                let text = match record.and_then(|record| record.get("text")) {
                    Some(text) if !text.is_null() => Some(text),
                    _ => as_record(record.and_then(|record| record.get("content")))
                        .and_then(|content| content.get("text")),
                };
                let text = match as_record(text).and_then(|text| text.get("text")) {
                    Some(inner) if !inner.is_null() => Some(inner),
                    _ => text,
                };
                let block = read_result(text, depth + 1);
                if result.data.is_none() {
                    result.data = block.data;
                }
                result.failed |= block.failed;
            }
            result
        }
        Some(Value::Object(record)) => {
            let failed = record.get("isError") == Some(&Value::Bool(true))
                || record.get("is_error") == Some(&Value::Bool(true))
                || record
                    .get("_tag")
                    .and_then(Value::as_str)
                    .is_some_and(|tag| FAILURE_TAG.is_match(tag))
                || record.get("error").is_some_and(|error| !error.is_null());
            let content = match record.get("structuredContent") {
                Some(content) if !content.is_null() => Some(content),
                _ => record.get("content"),
            };
            match content {
                Some(content) => {
                    let result = read_result(Some(content), depth + 1);
                    ToolResult {
                        data: result.data,
                        failed: failed || result.failed,
                    }
                }
                None => ToolResult {
                    data: Some(Cow::Borrowed(record)),
                    failed,
                },
            }
        }
        _ => ToolResult::default(),
    }
}

fn read_input(value: Option<&Value>) -> Option<Cow<'_, Record>> {
    let input = read_result(value, 0).data?;
    // Cursor retains its MCP args envelope; other adapters retain the arguments directly.
    if input.get("toolName").is_some_and(Value::is_string) {
        return match input {
            Cow::Borrowed(input) => as_record(input.get("args")).map(Cow::Borrowed),
            Cow::Owned(input) => as_record(input.get("args")).cloned().map(Cow::Owned),
        };
    }
    Some(input)
}

/// MCP errors can be returned as data even when the provider completed the tool call.
pub fn tool_result_indicates_failure(output: Option<&Value>) -> bool {
    read_result(output, 0).failed
}

fn count_entities(ids: &[Option<&str>]) -> usize {
    ids.iter().flatten().collect::<HashSet<_>>().len()
        + ids.iter().filter(|id| id.is_none()).count()
}

struct ReadCall<'a> {
    input: Option<Cow<'a, Record>>,
    output: Option<Cow<'a, Record>>,
    outcome: ToolCallOutcome,
}

impl ReadCall<'_> {
    fn output(&self, key: &str) -> Option<&Value> {
        self.output.as_ref().and_then(|output| output.get(key))
    }
    fn input(&self, key: &str) -> Option<&Value> {
        self.input.as_ref().and_then(|input| input.get(key))
    }
}

/// Counts successful effects separately from failed or unfinished tool calls.
pub fn summarize_tool_calls(
    action: ToolSummaryAction,
    calls: &[ToolSummaryCall],
) -> ToolCallsSummary {
    let results: Vec<ReadCall> = calls
        .iter()
        .map(|call| {
            let result = read_result(call.output.as_ref(), 0);
            ReadCall {
                input: read_input(call.input.as_ref()),
                output: result.data,
                outcome: if result.failed {
                    ToolCallOutcome::Failed
                } else {
                    call.outcome
                },
            }
        })
        .collect();
    let completed: Vec<&ReadCall> = results
        .iter()
        .filter(|call| call.outcome == ToolCallOutcome::Completed)
        .collect();
    let failed_count = results
        .iter()
        .filter(|call| call.outcome == ToolCallOutcome::Failed)
        .count();
    let selected: Vec<&ReadCall> = if completed.is_empty() {
        results.iter().collect()
    } else {
        completed.clone()
    };
    let times = quantity(selected.len(), "time");
    let phrase = |past: &str, infinitive: &str, object: &str| {
        if completed.is_empty() {
            format!("Tried to {infinitive} {object}")
        } else {
            format!("{past} {object}")
        }
    };
    let entity_ids = |key: &str| -> Vec<Option<&str>> {
        selected
            .iter()
            .map(|call| id(call.output(key)).or_else(|| id(call.input(key))))
            .collect()
    };
    let project_ids: Vec<Option<&str>> = selected
        .iter()
        .map(|call| {
            id(call.output("id"))
                .or_else(|| id(call.output("projectId")))
                .or_else(|| id(call.input("projectId")))
        })
        .collect();
    let thread_ids: Vec<Option<&str>> = selected
        .iter()
        .map(|call| {
            id(call.output("threadId"))
                .or_else(|| id(as_record(call.output("thread")).and_then(|t| t.get("threadId"))))
                .or_else(|| id(call.input("threadId")))
        })
        .collect();
    let targets_known = thread_ids.iter().all(Option::is_some);
    let distinct_threads = thread_ids.iter().collect::<HashSet<_>>().len();
    use ToolSummaryAction as A;
    let label = match action {
        A::ThreadSend => {
            let message_ids: Vec<Option<&str>> = selected
                .iter()
                .map(|call| id(call.output("messageId")))
                .collect();
            let messages = count_entities(&message_ids);
            let object = if !targets_known {
                quantity(messages, "message")
            } else if messages == distinct_threads && messages > 1 {
                format!("messages to {}", quantity(distinct_threads, "thread"))
            } else {
                format!(
                    "{} to {}",
                    quantity(messages, "message"),
                    quantity(distinct_threads, "thread")
                )
            };
            phrase("Sent", "send", &object)
        }
        A::ThreadCreate => {
            let mut created = HashSet::new();
            let results_known = !completed.is_empty()
                && completed.iter().all(|call| {
                    let threads: Vec<Option<&Record>> = match call.output("threads") {
                        Some(Value::Array(threads)) => {
                            threads.iter().map(Value::as_object).collect()
                        }
                        _ => vec![call.output.as_deref()],
                    };
                    threads.into_iter().all(|thread| {
                        let thread_value = |key: &str| thread.and_then(|thread| thread.get(key));
                        if thread_value("status").and_then(Value::as_str) == Some("rolled_back") {
                            return true;
                        }
                        let Some(thread_id) = id(thread_value("threadId")) else {
                            return false;
                        };
                        created.insert(thread_id.to_owned());
                        true
                    })
                });
            if results_known {
                format!("Created {}", quantity(created.len(), "thread"))
            } else {
                format!("Requested thread creation {times}")
            }
        }
        A::Delegate => phrase(
            "Delegated",
            "delegate",
            &quantity(count_entities(&entity_ids("taskId")), "task"),
        ),
        A::ThreadRead | A::ThreadWait => {
            let targets = if targets_known {
                quantity(distinct_threads, "thread")
            } else {
                format!("threads {times}")
            };
            if action == A::ThreadRead {
                phrase("Read", "read", &targets)
            } else {
                phrase("Waited on", "wait on", &targets)
            }
        }
        A::ThreadList => phrase("Listed", "list", &format!("threads {times}")),
        A::ThreadInterrupt => phrase(
            "Requested interrupts for",
            "interrupt",
            &quantity(count_entities(&thread_ids), "thread"),
        ),
        A::TaskStatus => phrase("Checked", "check", &format!("task status {times}")),
        A::TaskCancel => phrase(
            "Requested cancellation of",
            "cancel",
            &quantity(count_entities(&entity_ids("taskId")), "task"),
        ),
        A::ThreadConfiguration => {
            phrase("Checked", "check", &format!("thread configuration {times}"))
        }
        A::ThreadConfigure => phrase("Set", "set", &format!("thread model {times}")),
        A::ThreadFork => phrase(
            "Requested",
            "request",
            &quantity(selected.len(), "thread fork"),
        ),
        A::ThreadMerge => phrase(
            "Requested",
            "request",
            &quantity(selected.len(), "context merge"),
        ),
        A::ThreadSearch => phrase("Searched", "search", &format!("threads {times}")),
        A::ThreadTransfers => phrase("Checked", "check", &format!("thread transfers {times}")),
        A::ThreadOrganize => phrase("Organized", "organize", &format!("threads {times}")),
        A::ThreadUpdate => phrase(
            "Updated",
            "update",
            &quantity(count_entities(&thread_ids), "thread"),
        ),
        A::QueueList => phrase("Listed", "list", &format!("queued messages {times}")),
        A::QueueRead => phrase(
            "Read",
            "read",
            &quantity(count_entities(&entity_ids("queuedRunId")), "queued message"),
        ),
        A::QueueEdit => phrase(
            "Edited",
            "edit",
            &quantity(count_entities(&entity_ids("queuedRunId")), "queued message"),
        ),
        A::QueueCancel => phrase(
            "Requested cancellation of",
            "cancel",
            &quantity(count_entities(&entity_ids("queuedRunId")), "queued run"),
        ),
        A::QueueReorder => phrase(
            "Reordered",
            "reorder",
            &quantity(count_entities(&entity_ids("queuedRunId")), "queued run"),
        ),
        A::QueueSteer => phrase(
            "Requested steering with",
            "steer with",
            &quantity(count_entities(&entity_ids("queuedRunId")), "queued message"),
        ),
        A::QuestionList => phrase("Listed", "list", &format!("pending questions {times}")),
        A::QuestionRead => phrase(
            "Read",
            "read",
            &quantity(
                count_entities(&entity_ids("requestId")),
                "pending question request",
            ),
        ),
        A::QuestionRespond => phrase(
            "Answered",
            "answer",
            &quantity(
                count_entities(&entity_ids("requestId")),
                "pending question request",
            ),
        ),
        A::ProjectList => phrase("Listed", "list", &format!("projects {times}")),
        A::ProjectRead => phrase(
            "Read",
            "read",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::ProjectCreate => phrase(
            "Registered",
            "register",
            &quantity(count_entities(&project_ids), "project"),
        ),
        A::Capabilities => phrase(
            "Checked",
            "check",
            &format!("orchestration capabilities {times}"),
        ),
    };
    ToolCallsSummary {
        label,
        failed_count: crate::view::count(failed_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToolSummaryAction as A;
    use serde_json::json;

    fn completed(input: Value, output: Option<Value>) -> ToolSummaryCall {
        ToolSummaryCall {
            input: Some(input),
            output,
            outcome: ToolCallOutcome::Completed,
        }
    }

    fn summary(label: &str, failed_count: u32) -> ToolCallsSummary {
        ToolCallsSummary {
            label: label.into(),
            failed_count,
        }
    }

    #[test]
    fn counts_registered_projects_repository_destinations_and_accepted_thread_launches() {
        assert_eq!(
            summarize_tool_calls(
                A::ProjectCreate,
                &[
                    completed(json!({}), Some(json!({"id": "project-1"}))),
                    completed(json!({}), Some(json!({"id": "project-1"}))),
                    completed(json!({}), Some(json!({"id": "project-2"}))),
                ]
            )
            .label,
            "Registered 2 projects"
        );
        assert_eq!(
            summarize_tool_calls(
                A::ThreadCreate,
                &[completed(
                    json!({}),
                    Some(json!({"threadId": "launched-thread", "status": "preparing"}))
                )]
            )
            .label,
            "Created 1 thread"
        );
    }

    #[test]
    fn deduplicates_the_queued_run_target() {
        for (action, label) in [
            (A::QueueRead, "Read 1 queued message"),
            (A::QueueEdit, "Edited 1 queued message"),
            (A::QueueCancel, "Requested cancellation of 1 queued run"),
            (A::QueueReorder, "Reordered 1 queued run"),
            (A::QueueSteer, "Requested steering with 1 queued message"),
        ] {
            assert_eq!(
                summarize_tool_calls(
                    action,
                    &[
                        completed(json!({"queuedRunId": "queued-1"}), None),
                        completed(json!({"queuedRunId": "queued-1"}), None),
                        ToolSummaryCall {
                            input: Some(json!({"queuedRunId": "queued-2"})),
                            output: None,
                            outcome: ToolCallOutcome::Unfinished,
                        },
                    ]
                ),
                summary(label, 0),
                "{action:?}"
            );
        }
    }

    #[test]
    fn counts_answered_requests_rather_than_pretending_every_request_contains_one_question() {
        assert_eq!(
            summarize_tool_calls(
                A::QuestionRespond,
                &[
                    completed(
                        json!({"requestId": "request-1", "answers": {"one": ["Yes"], "two": ["No"]}}),
                        None
                    ),
                    completed(json!({"requestId": "request-1"}), None),
                    completed(json!({"requestId": "request-2"}), None),
                ]
            )
            .label,
            "Answered 2 pending question requests"
        );
    }

    #[test]
    fn keeps_repeated_manual_runs_separate_and_describes_asynchronous_controls_as_requests() {
        assert_eq!(
            summarize_tool_calls(
                A::ThreadFork,
                &[completed(
                    json!({}),
                    Some(json!({"targetThreadId": "fork", "sequence": 3}))
                )]
            )
            .label,
            "Requested 1 thread fork"
        );
        assert_eq!(
            summarize_tool_calls(
                A::ThreadMerge,
                &[completed(
                    json!({"targetThreadId": "parent"}),
                    Some(json!({"sequence": 4}))
                )]
            )
            .label,
            "Requested 1 context merge"
        );
    }

    #[test]
    fn counts_messages_and_distinct_destinations_across_delivery_modes_deduplicating_retries() {
        let calls: Vec<_> = (0..5)
            .map(|i| {
                completed(
                    json!({
                        "threadId": format!("thread-{}", i % 2),
                        "mode": (["auto", "queue", "steer", "restart"][i % 4]),
                    }),
                    Some(json!({
                        "messageId": format!("message-{i}"),
                        "threadId": format!("thread-{}", i % 2),
                    })),
                )
            })
            .collect();
        let mut retried = calls.clone();
        retried.push(calls[0].clone());
        assert_eq!(
            summarize_tool_calls(A::ThreadSend, &retried),
            summary("Sent 5 messages to 2 threads", 0)
        );
    }

    #[test]
    fn reads_provider_result_envelopes_without_treating_json_in_the_message_as_result_data() {
        let result = json!({"messageId": "message-1", "threadId": "actual-thread"});
        let json = result.to_string();
        let outputs = [
            result.clone(),
            json!({"structuredContent": result}),
            json!([{"type": "text", "text": json}]),
            json!({"content": [{"text": {"text": json}}], "isError": false}),
            Value::String(json.clone()),
        ];
        let calls: Vec<_> = outputs
            .into_iter()
            .map(|output| {
                completed(
                    json!({
                        "toolName": "thread_send",
                        "args": {"threadId": "input-thread", "message": "{\"threadId\":\"fake\"}"},
                    }),
                    Some(output),
                )
            })
            .collect();
        assert_eq!(
            summarize_tool_calls(A::ThreadSend, &calls).label,
            "Sent 1 message to 1 thread"
        );
        assert_eq!(
            summarize_tool_calls(
                A::ThreadSend,
                &[
                    completed(
                        json!({"toolName": "thread_send", "args": {"threadId": "input-thread"}}),
                        None
                    ),
                    completed(json!({"threadId": "input-thread"}), None),
                ]
            )
            .label,
            "Sent 2 messages to 1 thread"
        );
    }

    #[test]
    fn falls_back_to_message_counts_when_a_destination_is_missing_or_a_result_is_malformed() {
        assert_eq!(
            summarize_tool_calls(
                A::ThreadSend,
                &[
                    completed(json!({"threadId": "known"}), None),
                    completed(
                        json!({"message": "{\"threadId\":\"not-a-destination\"}"}),
                        Some(json!("{truncated"))
                    ),
                    ToolSummaryCall {
                        input: None,
                        output: Some(json!("Message sent")),
                        outcome: ToolCallOutcome::Completed,
                    },
                ]
            )
            .label,
            "Sent 3 messages"
        );
    }

    #[test]
    fn counts_batch_created_threads_excludes_rollbacks_and_deduplicates_returned_thread_ids() {
        let mut threads: Vec<_> = (0..4)
            .map(|i| json!({"threadId": format!("thread-{i}"), "status": "running"}))
            .collect();
        threads.push(json!({"threadId": "rolled-back", "status": "rolled_back"}));
        assert_eq!(
            summarize_tool_calls(
                A::ThreadCreate,
                &[
                    completed(json!({}), Some(json!({"threads": threads}))),
                    completed(
                        json!({}),
                        Some(json!({"threadId": "thread-0", "status": "running"}))
                    ),
                ]
            )
            .label,
            "Created 4 threads"
        );
        assert_eq!(
            summarize_tool_calls(
                A::ThreadCreate,
                &[completed(
                    json!({"threads": [{"title": "Requested, not confirmed"}]}),
                    None
                )]
            )
            .label,
            "Requested thread creation 1 time"
        );
    }

    #[test]
    fn excludes_failed_and_unfinished_sends_even_if_the_provider_reports_completed() {
        let calls = [
            completed(
                json!({"threadId": "success"}),
                Some(json!({"messageId": "ok", "threadId": "success"})),
            ),
            completed(
                json!({"threadId": "failed"}),
                Some(json!({"isError": true, "structuredContent": {"threadId": "failed"}})),
            ),
            completed(
                json!({"threadId": "failed"}),
                Some(json!([
                    {"type": "text", "text": json!({"_tag": "OrchestratorMcpFailure"}).to_string()}
                ])),
            ),
            ToolSummaryCall {
                input: Some(json!({"threadId": "cancelled"})),
                output: None,
                outcome: ToolCallOutcome::Unfinished,
            },
        ];
        assert_eq!(
            summarize_tool_calls(A::ThreadSend, &calls),
            summary("Sent 1 message to 1 thread", 2)
        );
        assert_eq!(
            summarize_tool_calls(A::ThreadSend, &calls[1..2]),
            summary("Tried to send 1 message to 1 thread", 1)
        );
    }

    #[test]
    fn does_not_confuse_a_childs_failure_or_wait_timeout_with_failure_of_the_orchestration_call() {
        let failed_child =
            json!({"taskId": "task-1", "status": "failed", "summary": "command not found"});
        assert_eq!(
            summarize_tool_calls(
                A::Delegate,
                &[
                    completed(json!({}), Some(failed_child.clone())),
                    completed(json!({}), Some(failed_child.clone())),
                ]
            ),
            summary("Delegated 1 task", 0)
        );
        assert_eq!(
            summarize_tool_calls(
                A::TaskStatus,
                &vec![completed(json!({"taskId": "task-1"}), Some(failed_child)); 4]
            )
            .label,
            "Checked task status 4 times"
        );
        assert_eq!(
            summarize_tool_calls(
                A::ThreadWait,
                &[completed(
                    json!({"threadId": "thread-1"}),
                    Some(json!({"threadId": "thread-1", "timedOut": true}))
                )]
            ),
            summary("Waited on 1 thread", 0)
        );
    }

    #[test]
    fn describes_control_requests_without_claiming_that_a_thread_stopped_or_a_task_was_deleted() {
        assert_eq!(
            summarize_tool_calls(
                A::ThreadInterrupt,
                &[completed(
                    json!({"threadId": "thread-1"}),
                    Some(json!({"threadId": "thread-1", "status": "interrupt_requested"}))
                )]
            )
            .label,
            "Requested interrupts for 1 thread"
        );
        assert_eq!(
            summarize_tool_calls(
                A::TaskCancel,
                &[completed(
                    json!({"taskId": "task-1"}),
                    Some(json!({"taskId": "task-1", "status": "cancel_requested"}))
                )]
            )
            .label,
            "Requested cancellation of 1 task"
        );
    }

    /// The reference covers returned error tags through the dropped browser tools.
    #[test]
    fn treats_a_returned_error_tag_as_a_failed_call_even_if_the_provider_says_completed() {
        for tag in ["WorktreeMcpFailure", "PullRequestOperationError"] {
            let output = json!([{
                "type": "content",
                "content": {"type": "text", "text": json!({"_tag": tag, "message": "Unavailable"}).to_string()},
            }]);
            assert_eq!(
                summarize_tool_calls(A::ThreadList, &[completed(json!({}), Some(output))]),
                summary("Tried to list threads 1 time", 1),
                "{tag}"
            );
        }
    }
}
