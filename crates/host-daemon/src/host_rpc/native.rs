//! Pure translation of Codex native history into BEX values.
use agent_protocol::{
    execution::*,
    ids::ItemId,
    items::*,
    models::{Thread, ThreadResponse, Turn},
    session::{ProviderKind, SessionRef},
};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::Value;
use std::sync::Arc;

fn field<T: DeserializeOwned>(value: &Value, key: &str) -> Result<T, serde_json::Error> {
    serde_json::from_value(value[key].clone())
}
fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().into()
}
fn take_field<T: DeserializeOwned>(value: &mut Value, key: &str) -> Result<T, serde_json::Error> {
    serde_json::from_value(value.get_mut(key).map(Value::take).unwrap_or(Value::Null))
}
fn take_string(value: &mut Value, key: &str) -> String {
    match value.get_mut(key).map(Value::take) {
        Some(Value::String(text)) => text,
        _ => String::new(),
    }
}
pub(crate) fn session_status(value: &Value) -> SessionStatus {
    match value["type"].as_str() {
        Some("active") => SessionStatus::Running,
        Some("idle") => SessionStatus::Idle,
        Some("systemError") => SessionStatus::Unavailable,
        _ => SessionStatus::Unknown,
    }
}
pub(crate) fn turn_status(value: Option<&str>) -> TurnStatus {
    match value {
        Some("inProgress" | "running" | "pendingInit") => TurnStatus::Running,
        Some("completed") => TurnStatus::Completed,
        Some("failed" | "errored") => TurnStatus::Failed,
        Some("interrupted" | "shutdown") => TurnStatus::Interrupted,
        _ => TurnStatus::Unknown,
    }
}
pub(crate) fn item_status(value: Option<&str>) -> ItemStatus {
    match value {
        Some("inProgress" | "running") => ItemStatus::Running,
        Some("completed") => ItemStatus::Completed,
        Some("failed") => ItemStatus::Failed,
        Some("declined") => ItemStatus::Declined,
        Some("interrupted") => ItemStatus::Interrupted,
        _ => ItemStatus::Unknown,
    }
}

pub(crate) fn codex_error(value: &Value, retrying: bool) -> ExecutionError {
    let info = &value["codexErrorInfo"];
    let code = info.as_str().or_else(|| {
        info.as_object()
            .and_then(|fields| fields.keys().next().map(String::as_str))
    });
    let mut category = match code {
        Some("contextWindowExceeded") => ErrorCategory::ContextLimit,
        Some("sessionBudgetExceeded") => ErrorCategory::SessionLimit,
        Some("usageLimitExceeded") => ErrorCategory::UsageLimit,
        Some("serverOverloaded") => ErrorCategory::Overloaded,
        Some("cyberPolicy" | "misalignmentPolicyViolation") => ErrorCategory::Policy,
        Some("internalServerError") => ErrorCategory::Internal,
        Some("unauthorized") => ErrorCategory::Auth,
        Some("badRequest") => ErrorCategory::InvalidInput,
        Some("threadRollbackFailed") => ErrorCategory::Rollback,
        Some("sandboxError") => ErrorCategory::Sandbox,
        Some("activeTurnNotSteerable") => ErrorCategory::InputUnavailable,
        Some(
            "httpConnectionFailed"
            | "responseStreamConnectionFailed"
            | "responseStreamDisconnected"
            | "responseTooManyFailedAttempts",
        ) => ErrorCategory::Network,
        _ if !info.is_null() => ErrorCategory::Provider(info.clone()),
        _ => ErrorCategory::Other,
    };
    let status = code
        .map(|code| &info[code]["httpStatusCode"])
        .unwrap_or(&Value::Null);
    let http_status = status.as_u64().and_then(|v| v.try_into().ok());
    if category == ErrorCategory::Network {
        category = match http_status {
            Some(429) => ErrorCategory::RateLimited,
            Some(503) => ErrorCategory::Overloaded,
            _ => category,
        };
    }
    let overloaded = category == ErrorCategory::Overloaded
        || matches!(status.as_u64(), Some(429 | 503))
        || matches!(status.as_str(), Some("429" | "503"));
    ExecutionError {
        category,
        message: value["message"]
            .as_str()
            .or_else(|| value.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "native execution failed without a message".into()),
        details: value["additionalDetails"].as_str().map(str::to_owned),
        provider_code: code.map(str::to_owned),
        retry: retrying.then_some(RetryEvidence {
            retrying,
            overloaded,
        }),
    }
}

fn parts(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|v| v.as_str().or_else(|| v["text"].as_str()))
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    }
}
pub(crate) fn message_parts(value: &Value) -> Vec<MessagePart> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            Some(match part["type"].as_str() {
                Some("localImage") => MessagePart::Image {
                    source: string(part, "path"),
                },
                Some("image") => MessagePart::Image {
                    source: string(part, "url"),
                },
                Some("skill") => MessagePart::Invocation {
                    name: string(part, "name"),
                    path: string(part, "path"),
                },
                Some("mention")
                    if part["path"].as_str().is_some_and(|path| {
                        path.starts_with("plugin://") || path.starts_with("app://")
                    }) =>
                {
                    MessagePart::Invocation {
                        name: string(part, "name"),
                        path: string(part, "path"),
                    }
                }
                Some("mention") => MessagePart::Attachment {
                    name: string(part, "name"),
                    path: string(part, "path"),
                },
                _ if part["text"].is_string() => MessagePart::Text {
                    text: string(part, "text"),
                },
                _ => return None,
            })
        })
        .collect()
}

pub(crate) fn codex_item(mut value: Value) -> Result<Item, serde_json::Error> {
    let id = field(&value, "id")?;
    let status = item_status(value["status"].as_str());
    let client_input_id = field(&value, "clientId")?;
    let body = match value["type"].as_str().unwrap_or_default() {
        "userMessage" => ItemBody::UserMessage {
            text: field(&value, "text")?,
            content: message_parts(&value["content"]),
        },
        "agentMessage" => ItemBody::AssistantText {
            text: take_string(&mut value, "text"),
            phase: match value["phase"].as_str() {
                Some("commentary") => AssistantPhase::Commentary,
                Some("final_answer") => AssistantPhase::Final,
                _ => AssistantPhase::Unknown,
            },
        },
        "reasoning" => ItemBody::Reasoning {
            content: if value["content"].is_null() {
                parts(&value["text"])
            } else {
                parts(&value["content"])
            },
            summary: parts(&value["summary"]),
        },
        "commandExecution" => ItemBody::CommandExecution {
            command: take_string(&mut value, "command"),
            cwd: field(&value, "cwd")?,
            output: take_string(&mut value, "aggregatedOutput"),
            exit_code: field(&value, "exitCode")?,
        },
        "fileChange" => ItemBody::FileChange {
            changes: value["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|change| FileChange {
                    path: string(change, "path"),
                    proposal: None,
                    diff: change["diff"].as_str().map(str::to_owned),
                    kind: match change["kind"]
                        .as_str()
                        .or_else(|| change["kind"]["type"].as_str())
                    {
                        Some("add") => FileChangeKind::Add,
                        Some("delete") => FileChangeKind::Delete,
                        Some("update") => FileChangeKind::Update {
                            move_path: change["kind"]["movePath"].as_str().map(str::to_owned),
                        },
                        _ => FileChangeKind::Unknown,
                    },
                })
                .collect(),
            output: string(&value, "output"),
        },
        "mcpToolCall" | "dynamicToolCall" => ItemBody::ToolCall {
            resource_uri: field(&value, "mcpAppResourceUri")?,
            plugin_id: field(&value, "pluginId")?,
            kind: if value["type"] == "mcpToolCall" {
                ToolKind::Mcp
            } else {
                ToolKind::Dynamic
            },
            tool: string(&value, "tool"),
            server: field(&value, "server")?,
            namespace: field(&value, "namespace")?,
            arguments: value
                .get_mut("arguments")
                .map(Value::take)
                .unwrap_or(Value::Null),
            result: value
                .get_mut("result")
                .filter(|v| !v.is_null())
                .map(Value::take),
            error: value
                .get_mut("error")
                .filter(|v| !v.is_null())
                .map(Value::take),
            content: value["contentItems"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| match v["type"].as_str() {
                    Some("inputText") => Some(agent_protocol::requests::ToolContent::Text {
                        text: string(v, "text"),
                    }),
                    Some("inputImage") => Some(agent_protocol::requests::ToolContent::Image {
                        data_url: string(v, "imageUrl"),
                    }),
                    _ => None,
                })
                .collect(),
            success: field(&value, "success")?,
            duration_ms: field(&value, "durationMs")?,
        },
        "collabAgentToolCall" | "subAgentActivity" => ItemBody::Subagent {
            tool: string(&value, "tool"),
            prompt: field(&value, "prompt")?,
            model: field(&value, "model")?,
            effort: field(&value, "reasoningEffort")?,
            sender: value["senderThreadId"].as_str().map(|id| SessionRef {
                provider: ProviderKind::Codex,
                id: id.into(),
            }),
            receivers: value["receiverThreadIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|id| SessionRef {
                    provider: ProviderKind::Codex,
                    id: id.into(),
                })
                .collect(),
            states: value["agentsStates"]
                .as_object()
                .into_iter()
                .flat_map(|fields| fields.iter())
                .map(|(id, state)| SubagentState {
                    session: SessionRef {
                        provider: ProviderKind::Codex,
                        id: id.clone(),
                    },
                    status: turn_status(state["status"].as_str()),
                    message: state["message"].as_str().map(str::to_owned),
                })
                .collect(),
            agent_id: None,
            result: None,
        },
        "imageGeneration" => ItemBody::ImageGeneration {
            saved_path: field(&value, "savedPath")?,
            data: take_field(&mut value, "result")?,
            revised_prompt: field(&value, "revisedPrompt")?,
        },
        "imageView" => ItemBody::ImageView {
            path: string(&value, "path"),
        },
        "webSearch" => ItemBody::WebSearch {
            query: string(&value, "query"),
            action: match value["action"]["type"].as_str() {
                Some("search") => Some(WebSearchAction::Search {
                    query: field(&value["action"], "query")?,
                    queries: field::<Option<Vec<String>>>(&value["action"], "queries")?
                        .unwrap_or_default(),
                }),
                Some("openPage") => Some(WebSearchAction::OpenPage {
                    url: field(&value["action"], "url")?,
                }),
                Some("findInPage") => Some(WebSearchAction::Find {
                    url: field(&value["action"], "url")?,
                    pattern: field(&value["action"], "pattern")?,
                }),
                _ => None,
            },
        },
        "plan" => ItemBody::Plan {
            text: take_string(&mut value, "text"),
        },
        "contextCompaction" => ItemBody::Compaction {},
        "enteredReviewMode" | "exitedReviewMode" => ItemBody::Review {
            entering: value["type"] == "enteredReviewMode",
            text: string(&value, "review"),
        },
        "hookPrompt" => ItemBody::Hook {
            fragments: value["fragments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["text"].as_str())
                .map(str::to_owned)
                .collect(),
        },
        "sleep" => ItemBody::Sleep {},
        _ => ItemBody::Custom {
            provider: ProviderKind::Codex,
            kind: string(&value, "type"),
            value,
        },
    };
    let mut item = Item::new(id, status, body);
    item.client_input_id = client_input_id;
    Ok(item)
}

pub(crate) fn approval_review(
    id: ItemId,
    review: &Value,
    action: &Value,
    target_item_id: Option<ItemId>,
    started_at_ms: Option<u64>,
    completed_at_ms: Option<u64>,
) -> Result<Item, serde_json::Error> {
    let status = match review["status"].as_str() {
        Some("inProgress") => ApprovalReviewStatus::Running,
        Some("approved") => ApprovalReviewStatus::Approved,
        Some("denied") => ApprovalReviewStatus::Denied,
        Some("timedOut") => ApprovalReviewStatus::TimedOut,
        Some("aborted") => ApprovalReviewStatus::Aborted,
        _ => ApprovalReviewStatus::Unknown,
    };
    let description = match action["type"].as_str() {
        Some("command") => string(action, "command"),
        Some("execve") => format!(
            "{} {}",
            string(action, "program"),
            action["argv"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Some("applyPatch") => "ファイル変更".into(),
        Some("networkAccess") => format!("ネットワーク接続: {}", string(action, "target")),
        Some("mcpToolCall") => format!(
            "{}: {}",
            string(action, "server"),
            string(action, "toolName")
        ),
        Some("requestPermissions") => "追加権限".into(),
        _ => "操作の自動確認".into(),
    };
    Ok(Item::new(
        id,
        if status == ApprovalReviewStatus::Running {
            ItemStatus::Running
        } else {
            ItemStatus::Completed
        },
        ItemBody::AutomaticApproval {
            status,
            reason: field(review, "rationale")?,
            risk: field(review, "riskLevel")?,
            authorization: field(review, "userAuthorization")?,
            description,
            details: (!action.is_null())
                .then(|| serde_json::to_string_pretty(action).expect("action serializes")),
            target_item_id,
            started_at_ms,
            completed_at_ms,
        },
    ))
}

pub(crate) fn codex_turn(mut value: Value) -> Result<Turn, serde_json::Error> {
    let mut turn = Turn {
        id: field(&value, "id")?,
        status: turn_status(value["status"].as_str()),
        items: take_field::<Option<Vec<Value>>>(&mut value, "items")?
            .map(|items| {
                items
                    .into_iter()
                    .map(codex_item)
                    .map(|v| v.map(Arc::new))
                    .collect()
            })
            .transpose()?,
        items_summary: value["itemsView"] == "summary",
        started_at: field(&value, "startedAt")?,
        duration_ms: field(&value, "durationMs")?,
        error: value
            .get("error")
            .filter(|v| !v.is_null())
            .map(|error| codex_error(error, error["willRetry"] == true)),
        started_at_ms: field(&value, "startedAtMs")?,
        completed_at_ms: field(&value, "completedAtMs")?,
    };
    if value["itemsView"] == "notLoaded" {
        turn.items = None;
    }
    if let Some(ids) = value["deferredItemIds"].as_array() {
        for item in turn.items.iter_mut().flatten() {
            if ids.iter().any(|id| id.as_str() == Some(&item.id)) {
                Arc::make_mut(item).defer();
            }
        }
    }
    Ok(turn)
}

pub(crate) fn codex_thread(mut value: Value) -> Result<Thread, serde_json::Error> {
    let parent_id = field::<Option<String>>(&value, "parentThreadId")?
        .map(|id| SessionRef::new(ProviderKind::Codex, id))
        .transpose()
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    let name = field::<Option<String>>(&value, "name")?
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            parent_id.as_ref()?;
            value["agentNickname"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .or_else(|| value["agentRole"].as_str())
                .map(str::to_owned)
        });
    Ok(Thread {
        id: Some(
            SessionRef::new(ProviderKind::Codex, field(&value, "id")?)
                .map_err(<serde_json::Error as serde::de::Error>::custom)?,
        ),
        parent_id,
        can_accept_direct_input: field(&value, "canAcceptDirectInput")?,
        name,
        cwd: field(&value, "cwd")?,
        status: session_status(&value["status"]),
        turns: take_field::<Option<Vec<Value>>>(&mut value, "turns")?
            .map(|turns| {
                turns
                    .into_iter()
                    .map(codex_turn)
                    .map(|v| v.map(Arc::new))
                    .collect()
            })
            .transpose()?,
        preview: field(&value, "preview")?,
        updated_at: field(&value, "updatedAt")?,
        history_has_more: field(&value, "historyHasMore")?,
        history_limit: field(&value, "historyLimit")?,
        ..Default::default()
    })
}
pub(crate) fn codex_thread_response(mut value: Value) -> Result<ThreadResponse, serde_json::Error> {
    Ok(ThreadResponse {
        model: field::<Option<String>>(&value, "model")?.map(|id| {
            agent_protocol::models::ModelRef {
                provider: ProviderKind::Codex,
                id,
            }
        }),
        thread: codex_thread(value["thread"].take())?,
    })
}

pub(super) fn deserialize_item<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<Item>, D::Error> {
    codex_item(Value::deserialize(deserializer)?)
        .map(Arc::new)
        .map_err(serde::de::Error::custom)
}
/// Codex's native tagged input is constructed only at the provider IO boundary.
pub(super) fn codex_input(input: &[agent_protocol::operations::Input]) -> Vec<Value> {
    use agent_protocol::operations::Input;
    input
        .iter()
        .map(|part| match part {
            Input::Text { text } => serde_json::json!({"type":"text","text":text}),
            Input::Skill { name, path } => {
                serde_json::json!({"type":"skill","name":name,"path":path})
            }
            Input::LocalImage { path } => serde_json::json!({"type":"localImage","path":path}),
            Input::Mention { name, path } => {
                serde_json::json!({"type":"mention","name":name,"path":path})
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn spawned_threads_keep_native_lineage_and_agent_labels_without_nesting_forks() {
        let child = codex_thread(json!({"id":"child", "parentThreadId":"parent", "agentNickname":"Curie", "agentRole":"reviewer", "canAcceptDirectInput":false})).unwrap();
        assert_eq!(
            child.parent_id,
            Some(SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap())
        );
        assert_eq!(child.name.as_deref(), Some("Curie"));
        assert_eq!(codex_thread(json!({"id":"child", "parentThreadId":"parent", "name":"", "agentNickname":"", "agentRole":"reviewer"})).unwrap().name.as_deref(), Some("reviewer"));
        assert_eq!(codex_thread(json!({"id":"child", "parentThreadId":"parent", "name":"Named", "agentNickname":"Curie"})).unwrap().name.as_deref(), Some("Named"));
        assert!(
            agent_protocol::session::input_unavailable_reason(&child)
                .unwrap()
                .contains("閲覧専用")
        );
        let fork = codex_thread(json!({"id":"fork", "forkedFromId":"parent"})).unwrap();
        assert!(fork.parent_id.is_none());
        assert!(codex_thread(json!({"id":"child", "parentThreadId":""})).is_err());
    }
    #[test]
    fn codex_items_keep_phase_indexes_tool_metadata_and_scoped_subagents() {
        let assistant = codex_item(
            json!({"id":"a","type":"agentMessage","text":"answer","phase":"final_answer"}),
        )
        .unwrap();
        assert!(matches!(
            assistant.body(),
            ItemBody::AssistantText {
                phase: AssistantPhase::Final,
                ..
            }
        ));
        let reasoning = codex_item(
            json!({"id":"r","type":"reasoning","content":["first","second"],"summary":["summary"]}),
        )
        .unwrap();
        assert!(
            matches!(reasoning.body(),ItemBody::Reasoning {content,summary} if content == &vec!["first","second"] && summary == &vec!["summary"])
        );
        let command = codex_item(json!({"id":"c","type":"commandExecution","command":"cat file","cwd":"/work","status":"completed","exitCode":3})).unwrap();
        assert_eq!(command.status, ItemStatus::Completed);
        assert!(
            matches!(command.body(),ItemBody::CommandExecution {cwd:Some(cwd),exit_code:Some(3),..} if cwd == "/work")
        );
        let tool = codex_item(json!({"id":"t","type":"mcpToolCall","tool":"read","server":"server","arguments":{},"mcpAppResourceUri":"ui://tool","pluginId":"plugin","result":{"content":[{"type":"text","text":"result"}]}})).unwrap();
        assert!(
            matches!(tool.body(),ItemBody::ToolCall {resource_uri:Some(uri),plugin_id:Some(plugin),result:Some(result),..} if uri == "ui://tool" && plugin == "plugin" && result["content"][0]["text"] == "result")
        );
        let subagent = codex_item(json!({"id":"s","type":"collabAgentToolCall","tool":"spawnAgent","senderThreadId":"same","receiverThreadIds":["same"],"agentsStates":{"same":{"status":"pendingInit"}}})).unwrap();
        assert!(
            matches!(subagent.body(),ItemBody::Subagent {states,..} if states[0].session.provider == ProviderKind::Codex && states[0].status == TurnStatus::Running)
        );
        let future = json!({"id":"u","type":"futureItem","unknown":[1,2,3]});
        assert!(
            matches!(codex_item(future.clone()).unwrap().body(),ItemBody::Custom {value,..} if value == &future)
        );
    }
    #[test]
    fn codex_errors_preserve_native_evidence_without_inventing_periodic_limits() {
        for (code, category) in [
            ("usageLimitExceeded", ErrorCategory::UsageLimit),
            ("contextWindowExceeded", ErrorCategory::ContextLimit),
            ("unauthorized", ErrorCategory::Auth),
        ] {
            assert_eq!(
                codex_error(&json!({"message":"failure","codexErrorInfo":code}), false).category,
                category
            );
        }
        let error = codex_error(
            &json!({"message":"retry","codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":429}}}),
            true,
        );
        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert!(error.retry.unwrap().overloaded);
        let future = json!({"futureFailure":{"detail":[1,{"unknown":true}]}});
        assert_eq!(
            codex_error(
                &json!({"message":"future failure","codexErrorInfo":future}),
                false
            )
            .category,
            ErrorCategory::Provider(future)
        );
    }
}
