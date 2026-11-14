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
/// Move parsed history arrays without rebuilding their nested JSON containers.
fn optional_array(value: Value) -> Result<Option<Vec<Value>>, serde_json::Error> {
    match value {
        Value::Null => Ok(None),
        Value::Array(values) => Ok(Some(values)),
        value => serde_json::from_value(value),
    }
}
/// Move a field without allocating a map entry when it is absent.
fn take_value(value: &mut Value, key: &str) -> Value {
    value.get_mut(key).map(Value::take).unwrap_or(Value::Null)
}
fn take_string(value: &mut Value, key: &str) -> String {
    match take_value(value, key) {
        Value::String(text) => text,
        _ => String::new(),
    }
}
pub(super) fn parse_model(
    mut value: Value,
) -> Result<agent_protocol::models::Model, serde_json::Error> {
    if let Some(tiers) = value["serviceTiers"].as_array_mut() {
        for tier in tiers {
            if let Some(tier) = tier.as_object_mut() {
                let fast = matches!(
                    tier.get("id").and_then(Value::as_str),
                    Some("priority" | "fast" | "ultrafast")
                );
                tier.insert("fast".into(), fast.into());
            }
        }
    }
    let id = value["model"].take();
    value["model"] = serde_json::json!({"provider":ProviderKind::Codex,"id":id});
    let settings = [
        "defaultReasoningEffort",
        "supportedReasoningEfforts",
        "serviceTiers",
        "defaultServiceTier",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), value[key].take()))
    .collect::<serde_json::Map<_, _>>();
    value["settings"] = Value::Object(settings);
    value["settingsDiscovery"] = Value::Bool(false);
    value["credentialRoute"] = Value::Null;
    serde_json::from_value(value)
}

pub(super) fn include_configured_model(
    models: &mut Vec<agent_protocol::models::Model>,
    config: &Value,
    account_type: Option<&str>,
) {
    if account_type != Some("apiKey") {
        return;
    }
    let Some(id) = config["model"].as_str().filter(|id| !id.trim().is_empty()) else {
        return;
    };
    if models.iter().any(|model| model.model.id == id) {
        return;
    }
    // Configuration establishes the ID, not its supported effort/tier catalog.
    models.push(agent_protocol::models::Model {
        id: id.into(),
        model: agent_protocol::models::ModelRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        },
        display_name: id.into(),
        credential_route: None,
        settings: None,
        settings_discovery: false,
        is_default: Some(false),
    });
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

/// The attempt a retry's message names, as Codex writes it ("Reconnecting...
/// 2/5"): the first `attempt/max` in it, both from 1.
fn retry_progress(message: &str) -> Option<(u32, u32)> {
    message.split_whitespace().find_map(|word| {
        let (attempt, max) = word
            .trim_matches(|c: char| !c.is_ascii_digit() && c != '/')
            .split_once('/')?;
        let (attempt, max) = (attempt.parse().ok()?, max.parse().ok()?);
        (attempt > 0 && max > 0).then_some((attempt, max))
    })
}

pub(crate) fn execution_error(value: &Value, retrying: bool) -> ExecutionError {
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
    ExecutionError {
        category,
        message: value["message"]
            .as_str()
            .or_else(|| value.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "native execution failed without a message".into()),
        details: value["additionalDetails"].as_str().map(str::to_owned),
        provider_code: code.map(str::to_owned),
        retry: retrying.then(|| {
            let (attempt, max) = retry_progress(value["message"].as_str().unwrap_or_default())
                .map_or((None, None), |(attempt, max)| (Some(attempt), Some(max)));
            RetryEvidence::new(attempt, max)
        }),
    }
}

fn parts(value: Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text],
        Value::Array(parts) => parts
            .into_iter()
            .filter_map(|mut part| match part {
                Value::String(text) => Some(text),
                _ => match take_value(&mut part, "text") {
                    Value::String(text) => Some(text),
                    _ => None,
                },
            })
            .collect(),
        _ => vec![],
    }
}
fn message_parts(value: Value) -> Vec<MessagePart> {
    let Value::Array(parts) = value else {
        return Vec::new();
    };
    parts
        .into_iter()
        .filter_map(|mut part| {
            Some(match part["type"].as_str() {
                Some("localImage") => MessagePart::Image {
                    source: take_string(&mut part, "path"),
                },
                Some("image") => MessagePart::Image {
                    source: take_string(&mut part, "url"),
                },
                Some("skill") => MessagePart::Invocation {
                    name: take_string(&mut part, "name"),
                    path: take_string(&mut part, "path"),
                },
                Some("mention")
                    if part["path"].as_str().is_some_and(|path| {
                        path.starts_with("plugin://") || path.starts_with("app://")
                    }) =>
                {
                    MessagePart::Invocation {
                        name: take_string(&mut part, "name"),
                        path: take_string(&mut part, "path"),
                    }
                }
                Some("mention") => MessagePart::Attachment {
                    name: take_string(&mut part, "name"),
                    path: take_string(&mut part, "path"),
                },
                _ if part["text"].is_string() => MessagePart::Text {
                    text: take_string(&mut part, "text"),
                },
                _ => return None,
            })
        })
        .collect()
}

#[cfg(test)]
mod owned_parts_tests;

fn file_changes(value: Value) -> Vec<FileChange> {
    let Value::Array(changes) = value else {
        return Vec::new();
    };
    changes
        .into_iter()
        .map(|mut change| {
            let mut kind = take_value(&mut change, "kind");
            FileChange {
                path: take_string(&mut change, "path"),
                proposal: None,
                diff: match take_value(&mut change, "diff") {
                    Value::String(diff) => Some(diff),
                    _ => None,
                },
                kind: match kind.as_str().or_else(|| kind["type"].as_str()) {
                    Some("add") => FileChangeKind::Add,
                    Some("delete") => FileChangeKind::Delete,
                    Some("update") => FileChangeKind::Update {
                        move_path: match take_value(&mut kind, "movePath") {
                            Value::String(path) => Some(path),
                            _ => None,
                        },
                    },
                    _ => FileChangeKind::Unknown,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod owned_details_tests;

/// T3 Code wakes Codex with this prompt when a command its turn left running
/// ends, so that Codex reports on it:
/// https://github.com/pingdotgg/t3code/blob/dac8b11ddc25bd5da62a367fdb6c3de777ae2986/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts#L427
const BACKGROUND_COMMAND_COMPLETED: &str = "Background command completed";

pub(super) fn background_command_detail(
    command: &str,
    exit_code: Option<i32>,
    output: &str,
) -> String {
    const COMMAND_MAX: usize = 200;
    const OUTPUT_TAIL_MAX: usize = 1_000;
    let command = if command.chars().count() > COMMAND_MAX {
        format!(
            "{}...",
            command.chars().take(COMMAND_MAX).collect::<String>()
        )
    } else {
        command.to_owned()
    };
    let exit = exit_code.map_or_else(String::new, |code| format!(" (exit {code})"));
    let header = format!("{BACKGROUND_COMMAND_COMPLETED}{exit}: {command}");
    let output = output.trim_end();
    let count = output.chars().count();
    if count == 0 {
        return header;
    }
    let tail = if count > OUTPUT_TAIL_MAX {
        format!(
            "...{}",
            output
                .chars()
                .skip(count - OUTPUT_TAIL_MAX)
                .collect::<String>()
        )
    } else {
        output.to_owned()
    };
    format!("{header}\n\nOutput tail:\n{tail}")
}

fn background_command_report(
    text: Option<&str>,
    content: &[MessagePart],
) -> Option<BackgroundReport> {
    let prompt = content
        .iter()
        .find_map(|part| match part {
            MessagePart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .or(text)?;
    let rest = prompt.strip_prefix(BACKGROUND_COMMAND_COMPLETED)?;
    let (exit_code, rest) = match rest.strip_prefix(" (exit ") {
        Some(rest) => {
            let (code, rest) = rest.split_once(')')?;
            (Some(code.parse().ok()?), rest)
        }
        None => (None, rest),
    };
    let command = rest.strip_prefix(": ")?.lines().next()?;
    Some(BackgroundReport {
        kind: BackgroundTaskKind::Command,
        label: (!command.is_empty()).then(|| command.to_owned()),
        outcome: match exit_code {
            Some(0) => BackgroundOutcome::Completed,
            Some(_) => BackgroundOutcome::Failed,
            None => BackgroundOutcome::Unknown,
        },
        exit_code,
    })
}

pub(crate) fn parse_item(mut value: Value) -> Result<Item, serde_json::Error> {
    let id = field(&value, "id")?;
    let status = item_status(value["status"].as_str());
    let client_input_id = field(&value, "clientId")?;
    let body = match value["type"].as_str().unwrap_or_default() {
        "userMessage" => {
            let text: Option<String> = serde_json::from_value(take_value(&mut value, "text"))?;
            let content = message_parts(take_value(&mut value, "content"));
            // The prompt that woke Codex for its background command is that
            // command's notification, live and in history alike.
            match background_command_report(text.as_deref(), &content) {
                Some(report) => ItemBody::Notification {
                    reports: vec![report],
                },
                None => agent_protocol::context::user_message(text, content),
            }
        }
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
                parts(take_value(&mut value, "text"))
            } else {
                parts(take_value(&mut value, "content"))
            },
            summary: parts(take_value(&mut value, "summary")),
        },
        "commandExecution" => ItemBody::CommandExecution {
            command: take_string(&mut value, "command"),
            cwd: field(&value, "cwd")?,
            output: take_string(&mut value, "aggregatedOutput"),
            exit_code: field(&value, "exitCode")?,
        },
        "fileChange" => ItemBody::FileChange {
            changes: file_changes(take_value(&mut value, "changes")),
            output: take_string(&mut value, "output"),
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
            arguments: take_value(&mut value, "arguments"),
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
                .chain(value["agentThreadId"].as_str())
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
            data: serde_json::from_value(take_value(&mut value, "result"))?,
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
            text: take_string(&mut value, "review"),
        },
        "hookPrompt" => {
            let fragments = match take_value(&mut value, "fragments") {
                Value::Array(fragments) => fragments
                    .into_iter()
                    .filter_map(|mut fragment| match take_value(&mut fragment, "text") {
                        Value::String(text) => Some(text),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            ItemBody::Hook { fragments }
        }
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

pub(crate) fn parse_turn(mut value: Value) -> Result<Turn, serde_json::Error> {
    let mut turn = Turn {
        id: field(&value, "id")?,
        provider: None,
        supports_fork: None,
        status: turn_status(value["status"].as_str()),
        items: optional_array(take_value(&mut value, "items"))?
            .map(|items| {
                items
                    .into_iter()
                    .map(parse_item)
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
            .map(|error| execution_error(error, error["willRetry"] == true)),
        started_at_ms: field(&value, "startedAtMs")?,
        completed_at_ms: field(&value, "completedAtMs")?,
        queued: false,
    };
    if value["itemsView"] == "notLoaded" {
        turn.items = None;
    }
    if let (Some(ids), Some(items)) = (value["deferredItemIds"].as_array(), &mut turn.items) {
        let ids: std::collections::HashSet<_> = ids.iter().filter_map(Value::as_str).collect();
        for item in items {
            if ids.contains(item.id.as_str()) {
                Arc::make_mut(item).defer();
            }
        }
    }
    Ok(turn)
}

/// Codex's summary view omits images. Read full bounded pages, then
/// keep the conversation content here while leaving work details on demand.
pub(super) fn summary_items(items: Vec<Arc<Item>>) -> Vec<Arc<Item>> {
    // A notification starts a turn as its user message does.
    let user = items.iter().position(|item| {
        matches!(
            item.body(),
            ItemBody::UserMessage { .. } | ItemBody::Notification { .. }
        )
    });
    let answer = items.iter().rposition(|item| {
        matches!(
            item.body(),
            ItemBody::AssistantText {
                phase: AssistantPhase::Final | AssistantPhase::Unknown,
                ..
            }
        )
    });
    items
        .into_iter()
        .enumerate()
        .filter(|(index, item)| {
            Some(*index) == user || Some(*index) == answer || item.is_image_output()
        })
        .map(|(_, item)| item)
        .collect()
}

pub(crate) fn parse_thread(mut value: Value) -> Result<Thread, serde_json::Error> {
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
        turns: optional_array(take_value(&mut value, "turns"))?
            .map(|turns| {
                turns
                    .into_iter()
                    .map(parse_turn)
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
pub(crate) fn parse_thread_response(mut value: Value) -> Result<ThreadResponse, serde_json::Error> {
    Ok(ThreadResponse {
        // Reads carry the saved model on Thread; start/resume resolve it on the response.
        model: field::<Option<String>>(&value, "model")?
            .or(field(&value["thread"], "model")?)
            .map(|id| agent_protocol::models::ModelRef {
                provider: ProviderKind::Codex,
                id,
            }),
        effort: field::<Option<String>>(&value, "reasoningEffort")?
            .or(field(&value["thread"], "reasoningEffort")?),
        service_tier: field(&value, "serviceTier")?,
        thread: parse_thread(value["thread"].take())?,
    })
}

pub(super) fn deserialize_item<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Arc<Item>, D::Error> {
    parse_item(Value::deserialize(deserializer)?)
        .map(Arc::new)
        .map_err(serde::de::Error::custom)
}
/// Codex's native tagged input is constructed only at the provider IO boundary.
pub(super) fn encode_input(input: &[agent_protocol::operations::Input]) -> Vec<Value> {
    use agent_protocol::operations::Input;
    input
        .iter()
        .map(|part| match part {
            Input::Context { context } => {
                serde_json::json!({"type":"text","text":context.provider_text()})
            }
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
    fn structured_context_survives_the_native_input_and_history_boundary() {
        use agent_protocol::{
            context::{ContextContent, MessageContext},
            operations::Input,
        };
        let context = MessageContext {
            id: "file".into(),
            label: "Source".into(),
            content: ContextContent::File {
                path: "/unread/source.rs".into(),
            },
        };
        let input = encode_input(&[Input::Context {
            context: Box::new(context.clone()),
        }]);
        assert_eq!(input[0]["type"], "text");
        let item = parse_item(
            json!({"type":"userMessage","id":"item","status":"completed","content":input}),
        )
        .unwrap();
        assert!(
            matches!(item.body(), ItemBody::UserMessage { text: None, content } if content == &[MessagePart::Context {context: Box::new(context)}])
        );
    }

    #[test]
    fn the_prompt_that_wakes_codex_for_its_command_reads_as_that_commands_notification() {
        let detail = background_command_detail("npm run dev", Some(0), "ready\n");
        assert_eq!(
            detail,
            "Background command completed (exit 0): npm run dev\n\nOutput tail:\nready"
        );
        for (text, outcome, exit_code) in [
            (detail, BackgroundOutcome::Completed, Some(0)),
            (
                background_command_detail("npm run dev", Some(2), ""),
                BackgroundOutcome::Failed,
                Some(2),
            ),
            (
                background_command_detail("npm run dev", None, ""),
                BackgroundOutcome::Unknown,
                None,
            ),
        ] {
            let item = parse_item(
                json!({"id":"wake","type":"userMessage","content":[{"type":"text","text":text}]}),
            )
            .unwrap();
            assert_eq!(
                item.body(),
                &ItemBody::Notification {
                    reports: vec![BackgroundReport {
                        kind: BackgroundTaskKind::Command,
                        label: Some("npm run dev".into()),
                        outcome,
                        exit_code,
                    }],
                }
            );
        }
        let human = parse_item(json!({"id":"human","type":"userMessage","content":[{"type":"text","text":"Background command completed? Check it"}]})).unwrap();
        assert!(matches!(human.body(), ItemBody::UserMessage { .. }));
    }

    #[test]
    fn native_history_arrays_reuse_owned_json_storage() {
        let values = vec![
            json!({"content":[{"nested":[1,2,3]}, {"opaque":{"$serde_json::private::RawValue":"unchanged"}}]}),
        ];
        let expected = values.clone();
        let array = values.as_ptr();
        let nested = values[0]["content"].as_array().unwrap().as_ptr();
        let result = optional_array(Value::Array(values)).unwrap().unwrap();
        assert_eq!(result, expected);
        assert_eq!(result.as_ptr(), array);
        assert_eq!(result[0]["content"].as_array().unwrap().as_ptr(), nested);
    }

    #[test]
    fn native_history_fields_keep_their_shape_validation() {
        assert!(parse_turn(json!({"id":"turn"})).unwrap().items.is_none());
        assert!(
            parse_thread(json!({"id":"thread"}))
                .unwrap()
                .turns
                .is_none()
        );
        assert!(
            parse_turn(json!({"id":"turn","items":null}))
                .unwrap()
                .items
                .is_none()
        );
        assert!(
            parse_thread(json!({"id":"thread","turns":null}))
                .unwrap()
                .turns
                .is_none()
        );
        assert!(
            parse_turn(json!({"id":"turn","items":[]}))
                .unwrap()
                .items
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_thread(json!({"id":"thread","turns":[]}))
                .unwrap()
                .turns
                .unwrap()
                .is_empty()
        );
        for invalid in [json!(false), json!(1), json!("[]"), json!({})] {
            assert!(parse_turn(json!({"id":"turn","items":invalid})).is_err());
            assert!(
                parse_turn(json!({"id":"turn","itemsView":"notLoaded","items":invalid})).is_err()
            );
            assert!(parse_thread(json!({"id":"thread","turns":invalid})).is_err());
        }
        assert!(parse_item(json!({"id":"image","type":"imageGeneration","result":[]})).is_err());
    }

    #[test]
    fn native_history_keeps_session_turn_and_item_states() {
        for (status, expected) in [
            ("active", SessionStatus::Running),
            ("idle", SessionStatus::Idle),
            ("systemError", SessionStatus::Unavailable),
        ] {
            let thread = parse_thread(json!({
                "id":"thread", "status":{"type":status},
                "turns":[{"id":"turn","status":"completed","items":[
                    {"id":"item","type":"agentMessage","status":"failed","text":"body"}
                ]}]
            }))
            .unwrap();
            assert_eq!(thread.status, expected);
            let turn = &thread.turns.as_ref().unwrap()[0];
            assert_eq!(turn.status, TurnStatus::Completed);
            let item = &turn.items.as_ref().unwrap()[0];
            assert_eq!(item.status, ItemStatus::Failed);
            assert!(matches!(item.body(), ItemBody::AssistantText { text, .. } if text == "body"));
        }
    }

    #[test]
    fn deferred_turn_ids_preserve_order_and_ignore_non_string_ids() {
        for count in [1, 63, 64, 65, 512] {
            let mut items: Vec<_> = (0..count)
                .map(|i| json!({"id":format!("item{i}"),"type":"agentMessage","text":"本文".repeat(200)}))
                .collect();
            items[0]["id"] = json!("0");
            let requested = items.last().unwrap()["id"].clone();
            let mut input = json!({"id":"turn","items":items});
            let original = parse_turn(input.clone()).unwrap();
            let mut ids = vec![
                Value::Null,
                json!(0),
                json!({"id":"item1"}),
                json!("item0suffix"),
            ];
            ids.extend((0..count).map(|_| requested.clone()));
            input["deferredItemIds"] = ids.into();
            let parsed = parse_turn(input).unwrap();
            for (index, (source, item)) in original
                .items
                .unwrap()
                .iter()
                .zip(parsed.items.unwrap())
                .enumerate()
            {
                assert_eq!(source.id, item.id);
                assert_eq!(source.status, item.status);
                assert_eq!(item.is_deferred(), index == count - 1);
                if index != count - 1 {
                    assert_eq!(source, &item);
                }
            }
        }
        for input in [
            json!({"id":"turn","deferredItemIds":["item"]}),
            json!({"id":"turn","itemsView":"notLoaded","items":[{"id":"item","type":"agentMessage","text":"body"}],"deferredItemIds":["item"]}),
        ] {
            assert!(parse_turn(input).unwrap().items.is_none());
        }
    }

    proptest::proptest! {
        #[test]
        fn deferred_turn_ids_match_only_the_requested_items(
            requested in proptest::collection::vec(proptest::prelude::any::<bool>(), 0..128),
        ) {
            let items: Vec<_> = requested.iter().enumerate().map(|(i, _)| json!({"id":format!("項目{i}"),"type":"agentMessage","text":"body"})).collect();
            let ids: Vec<_> = requested.iter().enumerate().map(|(i, requested)| {
                if *requested { json!(format!("項目{i}")) } else { json!(format!("missing{i}")) }
            }).collect();
            let parsed = parse_turn(json!({"id":"turn","items":items,"deferredItemIds":ids})).unwrap();
            let items = parsed.items.unwrap();
            proptest::prop_assert_eq!(items.len(), requested.len());
            for (index, (item, requested)) in items.iter().zip(requested).enumerate() {
                proptest::prop_assert_eq!(item.id.as_str(), format!("項目{index}"));
                proptest::prop_assert_eq!(item.is_deferred(), requested);
            }
        }
    }

    #[test]
    fn thread_reads_restore_the_model_from_native_thread_metadata() {
        let response = parse_thread_response(json!({
            "thread":{"id":"saved","model":"previous-model","reasoningEffort":"high","status":{"type":"notLoaded"}}
        }))
        .unwrap();
        assert_eq!(
            response.model,
            Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Codex,
                id: "previous-model".into(),
            })
        );
        assert_eq!(response.effort.as_deref(), Some("high"));
        let started = parse_thread_response(json!({
            "model":"configured-model",
            "reasoningEffort":"low", "serviceTier":"priority",
            "thread":{"id":"started","model":null,"reasoningEffort":"high"}
        }))
        .unwrap();
        assert_eq!(started.model.unwrap().id, "configured-model");
        assert_eq!(started.effort.as_deref(), Some("low"));
        assert_eq!(started.service_tier.as_deref(), Some("priority"));
        assert!(
            parse_thread_response(json!({"thread":{"id":"unknown","model":null}}))
                .unwrap()
                .model
                .is_none()
        );
    }

    #[test]
    fn model_speed_is_normalized_at_the_native_boundary() {
        let model = parse_model(json!({
            "id":"entry","model":"native-model","displayName":"Model",
            "defaultReasoningEffort":"medium","supportedReasoningEfforts":[],
            "serviceTiers":[{"id":"priority","name":"Fast"},{"id":"flex"},{"id":"unknown"}]
        }))
        .unwrap();
        assert_eq!(model.model.provider, ProviderKind::Codex);
        assert_eq!(model.model.id, "native-model");
        let tiers = model.settings.unwrap().service_tiers.unwrap();
        assert_eq!(
            tiers.iter().map(|tier| tier.fast).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(tiers[0].name.as_deref(), Some("Fast"));
        assert!(
            parse_model(json!({
                "id":"entry","model":"native-model","displayName":"Model",
                "defaultReasoningEffort":"medium","supportedReasoningEfforts":[],
                "serviceTiers":["invalid"]
            }))
            .is_err()
        );
    }

    #[test]
    fn configured_models_do_not_establish_chatgpt_access_or_capabilities() {
        let mut models = Vec::new();
        let config = json!({"model":"gpt-6.1-sol"});
        include_configured_model(&mut models, &config, Some("chatgpt"));
        include_configured_model(&mut models, &config, None);
        assert!(models.is_empty());
        include_configured_model(&mut models, &config, Some("apiKey"));
        include_configured_model(&mut models, &config, Some("apiKey"));
        assert_eq!(models.len(), 1);
        assert!(models[0].settings.is_none());
    }

    #[test]
    fn spawned_threads_keep_native_lineage_and_agent_labels_without_nesting_forks() {
        let child = parse_thread(json!({"id":"child", "parentThreadId":"parent", "agentNickname":"Curie", "agentRole":"reviewer", "canAcceptDirectInput":false})).unwrap();
        assert_eq!(
            child.parent_id,
            Some(SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap())
        );
        assert_eq!(child.name.as_deref(), Some("Curie"));
        assert_eq!(parse_thread(json!({"id":"child", "parentThreadId":"parent", "name":"", "agentNickname":"", "agentRole":"reviewer"})).unwrap().name.as_deref(), Some("reviewer"));
        assert_eq!(parse_thread(json!({"id":"child", "parentThreadId":"parent", "name":"Named", "agentNickname":"Curie"})).unwrap().name.as_deref(), Some("Named"));
        assert!(
            agent_protocol::session::direct_input_unavailable_reason(child.can_accept_direct_input)
                .unwrap()
                .contains("閲覧専用")
        );
        let fork = parse_thread(json!({"id":"fork", "forkedFromId":"parent"})).unwrap();
        assert!(fork.parent_id.is_none());
        assert!(parse_thread(json!({"id":"child", "parentThreadId":""})).is_err());
    }
    #[test]
    fn codex_items_keep_phase_indexes_tool_metadata_and_scoped_subagents() {
        let assistant = parse_item(
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
        let reasoning = parse_item(
            json!({"id":"r","type":"reasoning","content":["first","second"],"summary":["summary"]}),
        )
        .unwrap();
        assert!(
            matches!(reasoning.body(),ItemBody::Reasoning {content,summary} if content == &vec!["first","second"] && summary == &vec!["summary"])
        );
        let command = parse_item(json!({"id":"c","type":"commandExecution","command":"cat file","cwd":"/work","status":"completed","exitCode":3})).unwrap();
        assert_eq!(command.status, ItemStatus::Completed);
        assert!(
            matches!(command.body(),ItemBody::CommandExecution {cwd:Some(cwd),exit_code:Some(3),..} if cwd == "/work")
        );
        let tool = parse_item(json!({"id":"t","type":"mcpToolCall","tool":"read","server":"server","arguments":{},"mcpAppResourceUri":"ui://tool","pluginId":"plugin","result":{"content":[{"type":"text","text":"result"}]}})).unwrap();
        assert!(
            matches!(tool.body(),ItemBody::ToolCall {resource_uri:Some(uri),plugin_id:Some(plugin),result:Some(result),..} if uri == "ui://tool" && plugin == "plugin" && result["content"][0]["text"] == "result")
        );
        let subagent = parse_item(json!({"id":"s","type":"collabAgentToolCall","tool":"spawnAgent","senderThreadId":"same","receiverThreadIds":["same"],"agentsStates":{"same":{"status":"pendingInit"}}})).unwrap();
        assert!(
            matches!(subagent.body(),ItemBody::Subagent {states,..} if states[0].session.provider == ProviderKind::Codex && states[0].status == TurnStatus::Running)
        );
        let future = json!({"id":"u","type":"futureItem","unknown":[1,2,3]});
        let activity = parse_item(json!({"id":"activity","type":"subAgentActivity","status":"completed","kind":"started","agentThreadId":"child"})).unwrap();
        assert!(
            matches!(activity.body(), ItemBody::Subagent { receivers, .. }
            if receivers == &vec![SessionRef {provider: ProviderKind::Codex, id:"child".into()}])
        );
        assert!(
            matches!(parse_item(future.clone()).unwrap().body(),ItemBody::Custom {value,..} if value == &future)
        );
    }

    #[test]
    fn history_summary_keeps_every_image_in_order_without_work_details() {
        for phase in [Value::Null, json!("final_answer")] {
            let turn = parse_turn(json!({"id":"turn","items":[
                {"id":"user","type":"userMessage","content":[]},
                {"id":"commentary","type":"agentMessage","phase":"commentary","text":"Working"},
                {"id":"command","type":"commandExecution","command":"generate","aggregatedOutput":"large output"},
                {"id":"pending","type":"imageGeneration","status":"inProgress","result":""},
                {"id":"later-user","type":"userMessage","content":[]},
                {"id":"saved","type":"imageGeneration","status":"completed","savedPath":"/saved.png","result":"original"},
                {"id":"old-answer","type":"agentMessage","text":"Earlier answer"},
                {"id":"inline","type":"imageGeneration","status":"completed","result":"base64"},
                {"id":"failed","type":"imageGeneration","status":"failed"},
                {"id":"screenshot","type":"imageView","path":"file:///tmp/blog.png"},
                {"id":"mcp-image","type":"mcpToolCall","tool":"screenshot","result":{"content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]}},
                {"id":"exec-image","type":"dynamicToolCall","tool":"exec","contentItems":[{"type":"inputImage","imageUrl":"data:image/png;base64,aW1hZ2U="}]},
                {"id":"text-tool","type":"dynamicToolCall","tool":"exec","contentItems":[{"type":"inputText","text":"No images"}]},
                {"id":"invalid-image","type":"mcpToolCall","tool":"screenshot","result":{"content":[{"type":"image","mimeType":"image/png","data":3}]}},
                {"id":"answer","type":"agentMessage","phase":phase,"text":"Here are the images"},
                {"id":"tail-commentary","type":"agentMessage","phase":"commentary","text":"Activity"}
            ]})).unwrap();
            let summary = summary_items(turn.items.unwrap());
            assert_eq!(
                summary
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                [
                    "user",
                    "pending",
                    "saved",
                    "inline",
                    "failed",
                    "screenshot",
                    "mcp-image",
                    "exec-image",
                    "answer"
                ]
            );
            assert_eq!(summary[1].status, ItemStatus::Running);
            assert!(
                matches!(summary[2].body(), ItemBody::ImageGeneration {saved_path:Some(path), data:Some(data), ..} if path == "/saved.png" && data == "original")
            );
            assert!(
                matches!(summary[3].body(), ItemBody::ImageGeneration {saved_path:None, data:Some(data), ..} if data == "base64")
            );
            assert_eq!(summary[4].status, ItemStatus::Failed);
            assert!(
                matches!(summary[5].body(), ItemBody::ImageView { path } if path == "file:///tmp/blog.png")
            );
            assert!(
                matches!(summary[6].body(), ItemBody::ToolCall { result:Some(result),.. } if result["content"][0]["data"] == "aW1hZ2U=")
            );
            assert!(
                matches!(summary[7].body(), ItemBody::ToolCall { content,.. } if matches!(&content[0], agent_protocol::requests::ToolContent::Image { data_url } if data_url == "data:image/png;base64,aW1hZ2U="))
            );
        }
        assert!(summary_items(vec![]).is_empty());
        let image = Arc::new(parse_item(json!({"id":"image","type":"imageGeneration"})).unwrap());
        assert_eq!(summary_items(vec![image.clone()]), vec![image]);
    }
    #[test]
    fn codex_errors_preserve_native_evidence_without_inventing_periodic_limits() {
        for (code, category) in [
            ("usageLimitExceeded", ErrorCategory::UsageLimit),
            ("contextWindowExceeded", ErrorCategory::ContextLimit),
            ("unauthorized", ErrorCategory::Auth),
        ] {
            assert_eq!(
                execution_error(&json!({"message":"failure","codexErrorInfo":code}), false)
                    .category,
                category
            );
        }
        let error = execution_error(
            &json!({"message":"retry","codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":429}}}),
            true,
        );
        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.retry, Some(RetryEvidence::new(None, None)));
        // Codex numbers a reconnect only in its message.
        let error = execution_error(
            &json!({"message":"Reconnecting... 2/5","codexErrorInfo":"responseStreamDisconnected"}),
            true,
        );
        assert_eq!(error.retry, Some(RetryEvidence::new(Some(2), Some(5))));
        let future = json!({"futureFailure":{"detail":[1,{"unknown":true}]}});
        assert_eq!(
            execution_error(
                &json!({"message":"future failure","codexErrorInfo":future}),
                false
            )
            .category,
            ErrorCategory::Provider(future)
        );
    }
}
