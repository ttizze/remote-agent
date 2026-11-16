//! Native facts become T3 nodes and turn items at this boundary. No Bex decision
//! state or serialized display body is stored inside a canonical item.
use super::*;
use agent_protocol::{
    execution::ItemStatus,
    items::{Item, ItemBody},
    orchestration as model,
};
use serde_json::json;

mod cursors;
pub(super) mod external;
mod observations;
mod requests;
mod subagents;

/// Codex/Claude declare globally strong item identities. ACP's weak item keys
/// (including DSH's constant plan ID) only identify an item within the Host's
/// native turn UUID. Keep the raw key in NativeRef for provider operations.
fn item_identity(prefix: &str, provider: ProviderKind, turn: &str, key: &str) -> String {
    let key = if provider == ProviderKind::DeepSeek {
        std::borrow::Cow::Owned(json!([turn, key]).to_string())
    } else {
        std::borrow::Cow::Borrowed(key)
    };
    model::derived_id(
        prefix,
        &["provider", provider.driver(), "native-item", key.as_ref()],
    )
}

pub(super) fn text_item_id(
    provider: ProviderKind,
    turn: &str,
    item: &str,
    field: agent_protocol::session::TextField,
) -> String {
    use agent_protocol::session::TextField;
    let key = match field {
        TextField::ReasoningSummary { index } => json!([turn, item, "summary", index]).to_string(),
        TextField::ReasoningContent { index } => json!([turn, item, "content", index]).to_string(),
        _ => item.into(),
    };
    item_identity("turn-item", provider, turn, &key)
}

pub(super) fn text_part(
    body: &mut model::TurnItemBody,
    field: agent_protocol::session::TextField,
) -> Option<&mut String> {
    use agent_protocol::session::TextField;
    match (body, field) {
        (model::TurnItemBody::AssistantMessage { text, .. }, TextField::AssistantText)
        | (
            model::TurnItemBody::Reasoning { text, .. },
            TextField::ReasoningSummary { .. } | TextField::ReasoningContent { .. },
        ) => Some(text),
        (model::TurnItemBody::CommandExecution { output, .. }, TextField::CommandOutput) => {
            Some(output.get_or_insert_with(String::new))
        }
        _ => None,
    }
}

pub(super) struct Context {
    pub thread_id: String,
    pub run_id: Option<String>,
    pub root_node_id: String,
    pub provider_thread_id: String,
    pub provider_turn_id: String,
    pub native_turn_id: String,
    pub native_thread_id: String,
    pub provider: ProviderKind,
    /// T3 createdBy of native user messages in this thread.
    pub prompt_actor: model::Actor,
    pub now: String,
    pub completed: bool,
}

pub(super) struct Artifact {
    pub node: model::ExecutionNode,
    pub item: model::TurnItem,
    pub plan: Option<model::Plan>,
}

pub(super) fn item_status(status: ItemStatus, completed: bool) -> model::TurnItemStatus {
    use model::TurnItemStatus as Status;
    match status {
        ItemStatus::Running => Status::Running,
        ItemStatus::Completed => Status::Completed,
        ItemStatus::Failed => Status::Failed,
        ItemStatus::Declined => Status::Cancelled,
        ItemStatus::Interrupted => Status::Interrupted,
        ItemStatus::Unknown => {
            if completed {
                Status::Completed
            } else {
                Status::Running
            }
        }
    }
}

impl Context {
    pub(super) fn artifact(
        &self,
        key: &str,
        native_id: Option<&str>,
        status: model::TurnItemStatus,
        kind: model::NodeKind,
        body: model::TurnItemBody,
    ) -> Artifact {
        let terminal = status.is_terminal();
        let node_id = item_identity("node", self.provider, &self.native_turn_id, key);
        let native = native_id.map(|native_id| model::ProviderRef {
            driver: self.provider.driver().into(),
            native_id: Some(native_id.into()),
            strength: if self.provider == ProviderKind::DeepSeek {
                model::NativeRefStrength::Weak
            } else {
                model::NativeRefStrength::Strong
            },
            fingerprint: None,
            ordinal: None,
        });
        Artifact {
            node: model::ExecutionNode {
                id: node_id.clone(),
                thread_id: self.thread_id.clone(),
                run_id: self.run_id.clone(),
                parent_node_id: Some(self.root_node_id.clone()),
                root_node_id: self.root_node_id.clone(),
                kind,
                status: match status {
                    model::TurnItemStatus::Idle => model::NodeStatus::Idle,
                    model::TurnItemStatus::Pending => model::NodeStatus::Pending,
                    model::TurnItemStatus::Running => model::NodeStatus::Running,
                    model::TurnItemStatus::Waiting => model::NodeStatus::Waiting,
                    model::TurnItemStatus::Completed => model::NodeStatus::Completed,
                    model::TurnItemStatus::Failed => model::NodeStatus::Failed,
                    model::TurnItemStatus::Cancelled => model::NodeStatus::Cancelled,
                    model::TurnItemStatus::Interrupted => model::NodeStatus::Interrupted,
                },
                counts_for_run: false,
                provider_thread_id: Some(self.provider_thread_id.clone()),
                provider_turn_id: Some(self.provider_turn_id.clone()),
                native_item_ref: native.clone(),
                runtime_request_id: None,
                checkpoint_scope_id: None,
                started_at: Some(self.now.clone()),
                completed_at: terminal.then(|| self.now.clone()),
            },
            item: model::TurnItem {
                detail_omitted: false,
                id: item_identity("turn-item", self.provider, &self.native_turn_id, key),
                thread_id: self.thread_id.clone(),
                run_id: self.run_id.clone(),
                node_id: Some(node_id),
                provider_thread_id: Some(self.provider_thread_id.clone()),
                provider_turn_id: Some(self.provider_turn_id.clone()),
                native_item_ref: native,
                parent_item_id: None,
                ordinal: 0,
                status,
                title: None,
                started_at: Some(self.now.clone()),
                completed_at: terminal.then(|| self.now.clone()),
                updated_at: self.now.clone(),
                body,
            },
            plan: None,
        }
    }

    fn notifications(
        &self,
        id: &str,
        status: model::TurnItemStatus,
        reports: &[agent_protocol::execution::BackgroundReport],
    ) -> Vec<Artifact> {
        use agent_protocol::execution::{BackgroundOutcome as Outcome, BackgroundTaskKind as Kind};
        reports.iter().enumerate().map(|(index,report)| {
            let kind=match report.kind {Kind::Command=>"command",Kind::Subagent=>"subagent",Kind::Monitor=>"monitor",Kind::BackgroundTask=>"background_task"};
            let outcome=match report.outcome {Outcome::Completed=>"completed",Outcome::Failed=>"failed",Outcome::Cancelled=>"cancelled",Outcome::Updated=>"updated",Outcome::Unknown=>"unknown"};
            self.artifact(&json!([id,"report",index]).to_string(),Some(id),status,model::NodeKind::System,model::TurnItemBody::Notification {
                source:json!({"kind":kind,"label":report.label,"exitCode":report.exit_code}),outcome:outcome.into(),summary:report.label.clone().unwrap_or_default(),detail:None,
            })
        }).collect()
    }

    pub(super) fn item(&self, item: &Item) -> Result<Vec<Artifact>, String> {
        use model::{NodeKind as Kind, TurnItemBody as Body, TurnItemStatus as Status};
        if item.is_deferred()
            && !matches!(
                item.body(),
                ItemBody::CommandExecution { .. }
                    | ItemBody::ToolCall { .. }
                    | ItemBody::FileChange { .. }
            )
        {
            return Err("native item must be fully read before it is committed".into());
        }
        let status = item_status(item.status, self.completed);
        let streaming = status == Status::Running;
        let id = item.id.as_str();
        let (kind, body) = match item.body() {
            ItemBody::Notification { reports } => {
                return Ok(self.notifications(id, status, reports));
            }
            ItemBody::UserMessage { text, content } => (
                Kind::System,
                Body::UserMessage {
                    created_by: self.prompt_actor,
                    creation_source: model::CreationSource::Provider,
                    scheduled_task_id: None,
                    message_id: item_identity("message", self.provider, &self.native_turn_id, id),
                    input_intent: "turn_start".into(),
                    text: text.clone().unwrap_or_else(|| {
                        content
                            .iter()
                            .filter_map(|part| match part {
                                agent_protocol::items::MessagePart::Text { text } => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }),
                    attachments: vec![],
                    context: None,
                },
            ),
            ItemBody::AssistantText { text, phase } => (
                Kind::AssistantMessage,
                Body::AssistantMessage {
                    message_id: item_identity("message", self.provider, &self.native_turn_id, id),
                    text: text.clone(),
                    phase: match phase {
                        agent_protocol::items::AssistantPhase::Commentary => {
                            Some("commentary".into())
                        }
                        agent_protocol::items::AssistantPhase::Final => Some("final_answer".into()),
                        _ => None,
                    },
                    attachments: vec![],
                    streaming,
                },
            ),
            ItemBody::Reasoning { summary, content } => {
                let mut artifacts = Vec::new();
                for (stream, parts) in [("summary", summary), ("content", content)] {
                    for (index, text) in parts
                        .iter()
                        .enumerate()
                        .filter(|(_, text)| !text.is_empty())
                    {
                        let key =
                            serde_json::to_string(&json!([self.native_turn_id, id, stream, index]))
                                .map_err(|error| error.to_string())?;
                        artifacts.push(self.artifact(
                            &key,
                            Some(id),
                            status,
                            Kind::Reasoning,
                            Body::Reasoning {
                                text: text.clone(),
                                streaming,
                            },
                        ));
                    }
                }
                return Ok(artifacts);
            }
            ItemBody::CommandExecution {
                command,
                output,
                exit_code,
                output_omitted,
                ..
            } => (
                Kind::ToolCall,
                Body::CommandExecution {
                    input: command.clone(),
                    output: Some(output.clone()),
                    exit_code: *exit_code,
                    output_omitted: (*output_omitted || item.is_deferred()).then_some(true),
                    output_indicates_failure: None,
                },
            ),
            ItemBody::FileChange { changes, .. } => {
                let Some(first) = changes.first() else {
                    return Ok(vec![]);
                };
                (
                    Kind::ToolCall,
                    Body::FileChange {
                        file_name: first.path.clone(),
                        changes: changes
                            .iter()
                            .map(|change| match &change.kind {
                                agent_protocol::items::FileChangeKind::Add => {
                                    json!({"operation":"add","path":change.path})
                                }
                                agent_protocol::items::FileChangeKind::Delete => {
                                    json!({"operation":"delete","path":change.path})
                                }
                                agent_protocol::items::FileChangeKind::Update {
                                    move_path: Some(path),
                                } => {
                                    json!({"operation":"update","path":path,"oldPath":change.path})
                                }
                                _ => json!({"operation":"update","path":change.path}),
                            })
                            .collect(),
                        diff_str: (!item.is_deferred()).then(|| first.diff.clone()).flatten(),
                    },
                )
            }
            ItemBody::Plan { text } => {
                let plan_id = item_identity("plan", self.provider, &self.native_turn_id, id);
                let mut artifact = self.artifact(
                    id,
                    Some(id),
                    status,
                    Kind::Plan,
                    Body::ProposedPlan {
                        plan_id: plan_id.clone(),
                        markdown: text.clone(),
                        streaming,
                    },
                );
                artifact.plan = Some(model::Plan {
                    id: plan_id,
                    thread_id: self.thread_id.clone(),
                    run_id: self.run_id.clone(),
                    node_id: artifact.node.id.clone(),
                    kind: model::PlanKind::ProposedPlan,
                    status: model::PlanStatus::Draft,
                    detail_in_turn_item: true,
                });
                return Ok(vec![artifact]);
            }
            ItemBody::ToolCall {
                tool,
                kind,
                server,
                namespace,
                arguments,
                result,
                error,
                ..
            } => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some(match (self.provider, kind, server.as_deref()) {
                        (
                            ProviderKind::Codex,
                            agent_protocol::items::ToolKind::Mcp,
                            Some(server),
                        ) => format!("{server}.{tool}"),
                        (
                            ProviderKind::Claude,
                            agent_protocol::items::ToolKind::Mcp,
                            Some(server),
                        ) => format!("mcp__{server}__{tool}"),
                        (ProviderKind::Codex, _, _) => namespace
                            .as_deref()
                            .map(str::trim)
                            .filter(|prefix| !prefix.is_empty())
                            .map_or_else(|| tool.clone(), |prefix| format!("{prefix}.{tool}")),
                        _ => tool.clone(),
                    }),
                    input: arguments.clone(),
                    output: result.clone().or_else(|| {
                        error
                            .as_ref()
                            .map(|error| json!({"isError":true,"error":error}))
                    }),
                    output_omitted: item.is_deferred().then_some(true),
                    viewed_image_path: None,
                },
            ),
            ItemBody::ImageView { path } => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some("view_image".into()),
                    input: json!({"path":path}),
                    output: None,
                    output_omitted: None,
                    viewed_image_path: Some(path.clone()),
                },
            ),
            ItemBody::WebSearch { query, action } => (
                Kind::ToolCall,
                Body::WebSearch {
                    patterns: match action {
                        Some(agent_protocol::items::WebSearchAction::Search {
                            queries, ..
                        }) if !queries.is_empty() => queries.clone(),
                        Some(agent_protocol::items::WebSearchAction::OpenPage {
                            url: Some(url),
                        }) => vec![url.clone()],
                        Some(agent_protocol::items::WebSearchAction::Find { url, pattern }) => {
                            url.iter().chain(pattern).cloned().collect()
                        }
                        _ => vec![query.clone()],
                    },
                    results: vec![],
                },
            ),
            ItemBody::Compaction {} => (
                Kind::System,
                Body::Compaction {
                    driver: Some(self.provider.driver().into()),
                    summary: None,
                    before_token_count: None,
                    after_token_count: None,
                },
            ),
            ItemBody::Hook { fragments } => (
                Kind::Hook,
                Body::SystemNotice {
                    message: fragments.join("\n"),
                },
            ),
            ItemBody::Review { entering, text } => (
                Kind::System,
                Body::SystemNotice {
                    message: format!(
                        "{}\n{text}",
                        if *entering {
                            "Review started"
                        } else {
                            "Review completed"
                        }
                    ),
                },
            ),
            ItemBody::Sleep {} => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some("wait".into()),
                    input: json!({}),
                    output: None,
                    output_omitted: None,
                    viewed_image_path: None,
                },
            ),
            ItemBody::Custom { kind, value, .. } => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some(kind.clone()),
                    input: value.clone(),
                    output: None,
                    output_omitted: None,
                    viewed_image_path: None,
                },
            ),
            ItemBody::Attachment { content, .. } => (
                Kind::System,
                Body::DynamicTool {
                    tool_name: Some("attachment".into()),
                    input: content.clone(),
                    output: None,
                    output_omitted: None,
                    viewed_image_path: None,
                },
            ),
            ItemBody::AutomaticApproval {
                status,
                reason,
                risk,
                authorization,
                description,
                details,
                target_item_id,
                ..
            } => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some("autoApprovalReview".into()),
                    input: json!({"status":status,"reason":reason,"riskLevel":risk,"userAuthorization":authorization,"description":description,"details":details,"targetItemId":target_item_id}),
                    output: None,
                    output_omitted: None,
                    viewed_image_path: None,
                },
            ),
            ItemBody::ImageGeneration { .. } | ItemBody::Subagent { .. } => {
                return Err("native item requires its owning resource handler".into());
            }
        };
        Ok(vec![self.artifact(id, Some(id), status, kind, body)])
    }
}

#[cfg(test)]
mod tests;

fn control_reference(provider: ProviderKind, turn: &str) -> model::ProviderRef {
    model::ProviderRef {
        driver: provider.driver().into(),
        native_id: Some(turn.into()),
        strength: if provider == ProviderKind::DeepSeek {
            model::NativeRefStrength::Weak
        } else {
            model::NativeRefStrength::Strong
        },
        fingerprint: None,
        ordinal: None,
    }
}
