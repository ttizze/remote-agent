//! Item identity and delivery size are independent of its typed body.
use crate::{
    execution::*,
    ids::*,
    requests::ToolContent,
    session::{ProviderKind, SessionRef},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    #[serde(default)]
    pub status: ItemStatus,
    pub client_input_id: Option<ClientInputId>,
    pub body: ItemContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ItemContent {
    Inline { body: Box<ItemBody> },
    Deferred { summary: Box<ItemBody> },
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantPhase {
    Commentary,
    Final,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolKind {
    Local,
    Mcp,
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MessagePart {
    Text { text: String },
    Image { source: String },
    Attachment { name: String, path: String },
    Invocation { name: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ItemBody {
    UserMessage {
        text: Option<String>,
        content: Vec<MessagePart>,
    },
    AssistantText {
        text: String,
        phase: AssistantPhase,
    },
    Reasoning {
        content: Vec<String>,
        summary: Vec<String>,
    },
    ToolCall {
        kind: ToolKind,
        tool: String,
        server: Option<String>,
        namespace: Option<String>,
        resource_uri: Option<String>,
        plugin_id: Option<String>,
        #[serde(with = "crate::protocol::json")]
        arguments: Value,
        #[serde(with = "crate::protocol::json")]
        result: Option<Value>,
        #[serde(with = "crate::protocol::json")]
        error: Option<Value>,
        content: Vec<ToolContent>,
        success: Option<bool>,
        duration_ms: Option<u64>,
    },
    CommandExecution {
        command: String,
        cwd: Option<String>,
        output: String,
        exit_code: Option<i32>,
    },
    FileChange {
        changes: Vec<FileChange>,
        output: String,
    },
    Subagent {
        tool: String,
        prompt: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        sender: Option<SessionRef>,
        receivers: Vec<SessionRef>,
        states: Vec<SubagentState>,
        agent_id: Option<String>,
        #[serde(with = "crate::protocol::json")]
        result: Option<Value>,
    },
    ImageGeneration {
        saved_path: Option<String>,
        data: Option<String>,
        revised_prompt: Option<String>,
    },
    ImageView {
        path: String,
    },
    WebSearch {
        query: String,
        action: Option<WebSearchAction>,
    },
    Plan {
        text: String,
    },
    Compaction {},
    Review {
        entering: bool,
        text: String,
    },
    Hook {
        fragments: Vec<String>,
    },
    AutomaticApproval {
        status: ApprovalReviewStatus,
        reason: Option<String>,
        risk: Option<String>,
        authorization: Option<String>,
        description: String,
        details: Option<String>,
        target_item_id: Option<ItemId>,
        started_at_ms: Option<u64>,
        completed_at_ms: Option<u64>,
    },
    Attachment {
        kind: AttachmentKind,
        #[serde(with = "crate::protocol::json")]
        content: Value,
    },
    Sleep {},
    Custom {
        provider: ProviderKind,
        kind: String,
        #[serde(with = "crate::protocol::json")]
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub diff: Option<String>,
    pub proposal: Option<FileProposal>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FileProposal {
    Write {
        content: String,
    },
    Edit {
        old_text: String,
        new_text: String,
        replace_all: bool,
    },
    Notebook {
        cell_id: Option<String>,
        source: String,
        mode: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FileChangeKind {
    Add,
    Delete,
    Update { move_path: Option<String> },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentState {
    pub session: SessionRef,
    pub status: TurnStatus,
    pub message: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WebSearchAction {
    Search {
        query: Option<String>,
        queries: Vec<String>,
    },
    OpenPage {
        url: Option<String>,
    },
    Find {
        url: Option<String>,
        pattern: Option<String>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalReviewStatus {
    Running,
    Approved,
    Denied,
    TimedOut,
    Aborted,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentKind {
    HookResult,
    FileEdit,
    SessionUpdate,
    Other,
}

impl Item {
    pub fn new(id: ItemId, status: ItemStatus, body: ItemBody) -> Self {
        Self {
            id,
            status,
            client_input_id: None,
            body: ItemContent::Inline {
                body: Box::new(body),
            },
        }
    }
    pub fn body(&self) -> &ItemBody {
        match &self.body {
            ItemContent::Inline { body } | ItemContent::Deferred { summary: body } => body,
        }
    }
    pub fn body_mut(&mut self) -> &mut ItemBody {
        match &mut self.body {
            ItemContent::Inline { body } | ItemContent::Deferred { summary: body } => body,
        }
    }
    pub fn is_deferred(&self) -> bool {
        matches!(self.body, ItemContent::Deferred { .. })
    }
    /// Preserve only information used by a collapsed row. Original details stay with the provider.
    pub fn defer(&mut self) {
        let mut summary = match std::mem::replace(
            &mut self.body,
            ItemContent::Inline {
                body: Box::new(ItemBody::Sleep {}),
            },
        ) {
            ItemContent::Inline { body } | ItemContent::Deferred { summary: body } => body,
        };
        match summary.as_mut() {
            ItemBody::UserMessage { text, content } => {
                if let Some(text) = text {
                    truncate(text);
                }
                content.retain(|part| !matches!(part, MessagePart::Image { source } if source.starts_with("data:")));
                for part in content {
                    if let MessagePart::Text { text } = part {
                        truncate(text);
                    }
                }
            }
            ItemBody::AssistantText { text, .. } => truncate(text),
            ItemBody::Plan { text } | ItemBody::Review { text, .. } => truncate(text),
            ItemBody::Reasoning { content, summary } => {
                content.clear();
                summary.clear();
            }
            ItemBody::CommandExecution {
                command, output, ..
            } => {
                *command = crate::models::compact_title(command);
                output.clear();
            }
            ItemBody::FileChange { changes, output } => {
                for change in changes {
                    change.diff = None;
                    change.proposal = None;
                }
                output.clear();
            }
            ItemBody::ToolCall {
                arguments,
                result,
                error,
                content,
                ..
            } => {
                *arguments = Value::Null;
                *result = None;
                *error = None;
                content.clear();
            }
            ItemBody::Subagent { prompt, result, .. } => {
                *prompt = None;
                *result = None;
            }
            ItemBody::ImageGeneration {
                data,
                revised_prompt,
                ..
            } => {
                *data = None;
                *revised_prompt = None;
            }
            ItemBody::Attachment { content, .. } | ItemBody::Custom { value: content, .. } => {
                *content = Value::Null
            }
            ItemBody::Hook { fragments } => fragments.clear(),
            ItemBody::AutomaticApproval {
                reason,
                description,
                details,
                ..
            } => {
                truncate(description);
                *details = None;
                if let Some(reason) = reason {
                    truncate(reason);
                }
            }
            _ => {}
        }
        self.body = ItemContent::Deferred { summary };
    }
}
fn truncate(text: &mut String) {
    let mut end = text.len().min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
