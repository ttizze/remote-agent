//! Native facts become T3 nodes and turn items at this boundary. No Bex decision
//! state or serialized display body is stored inside a canonical item.
use super::*;
use agent_protocol::{
    execution::ItemStatus,
    items::{Item, ItemBody},
    orchestration as model,
};
use serde_json::json;

mod observations;
mod requests;

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
    model::derived_id(
        "turn-item",
        &["provider", provider.driver(), "native-item", &key],
    )
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
    pub provider: ProviderKind,
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
        let terminal = !matches!(
            status,
            model::TurnItemStatus::Idle
                | model::TurnItemStatus::Pending
                | model::TurnItemStatus::Running
                | model::TurnItemStatus::Waiting
        );
        let node_id = model::derived_id(
            "node",
            &["provider", self.provider.driver(), "native-item", key],
        );
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
                id: model::derived_id(
                    "turn-item",
                    &["provider", self.provider.driver(), "native-item", key],
                ),
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

    pub(super) fn item(&self, item: &Item) -> Result<Vec<Artifact>, String> {
        use model::{NodeKind as Kind, TurnItemBody as Body, TurnItemStatus as Status};
        if item.is_deferred() {
            return Err("native item must be fully read before it is committed".into());
        }
        let status = item_status(item.status, self.completed);
        let streaming = status == Status::Running;
        let id = item.id.as_str();
        let (kind, body) = match item.body() {
            ItemBody::AssistantText { text, phase } => (
                Kind::AssistantMessage,
                Body::AssistantMessage {
                    message_id: model::derived_id(
                        "message",
                        &["provider", self.provider.driver(), "native-item", id],
                    ),
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
                ..
            } => (
                Kind::ToolCall,
                Body::CommandExecution {
                    input: command.clone(),
                    output: Some(output.clone()),
                    exit_code: *exit_code,
                    output_omitted: None,
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
                        diff_str: first.diff.clone(),
                    },
                )
            }
            ItemBody::Plan { text } => {
                let plan_id = model::derived_id(
                    "plan",
                    &["provider", self.provider.driver(), "native-item", id],
                );
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
                arguments,
                result,
                error,
                ..
            } => (
                Kind::ToolCall,
                Body::DynamicTool {
                    tool_name: Some(tool.clone()),
                    input: arguments.clone(),
                    output: result.clone().or_else(|| {
                        error
                            .as_ref()
                            .map(|error| json!({"isError":true,"error":error}))
                    }),
                    output_omitted: None,
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
            ItemBody::UserMessage { .. }
            | ItemBody::ImageGeneration { .. }
            | ItemBody::Subagent { .. }
            | ItemBody::Notification { .. } => {
                return Err("native item requires its owning resource handler".into());
            }
        };
        Ok(vec![self.artifact(id, Some(id), status, kind, body)])
    }
}
