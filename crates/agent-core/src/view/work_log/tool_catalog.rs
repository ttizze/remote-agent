//! The orchestration tools the Host serves over MCP, with the labels cards,
//! activity rows and group summaries share.
use super::js_text::{JS_DOT, JS_SPACE};
use regex::Regex;
use std::sync::LazyLock;

/// The logo a recognized orchestration tool is branded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolLogo {
    App,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCatalogPresentation {
    pub display_name: String,
    pub logo: ToolLogo,
}

/// How a group summary counts the calls of one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSummaryAction {
    Capabilities,
    Delegate,
    TaskStatus,
    TaskCancel,
    ThreadCreate,
    ThreadList,
    ThreadRead,
    ThreadSend,
    ThreadWait,
    ThreadInterrupt,
    ThreadConfiguration,
    ThreadConfigure,
    ThreadFork,
    ThreadMerge,
    ThreadSearch,
    ThreadTransfers,
    ThreadOrganize,
    ThreadUpdate,
    QueueList,
    QueueRead,
    QueueEdit,
    QueueCancel,
    QueueReorder,
    QueueSteer,
    QuestionList,
    QuestionRead,
    QuestionRespond,
    ProjectList,
    ProjectRead,
    ProjectCreate,
}

impl ToolSummaryAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::Delegate => "delegate",
            Self::TaskStatus => "task-status",
            Self::TaskCancel => "task-cancel",
            Self::ThreadCreate => "thread-create",
            Self::ThreadList => "thread-list",
            Self::ThreadRead => "thread-read",
            Self::ThreadSend => "thread-send",
            Self::ThreadWait => "thread-wait",
            Self::ThreadInterrupt => "thread-interrupt",
            Self::ThreadConfiguration => "thread-configuration",
            Self::ThreadConfigure => "thread-configure",
            Self::ThreadFork => "thread-fork",
            Self::ThreadMerge => "thread-merge",
            Self::ThreadSearch => "thread-search",
            Self::ThreadTransfers => "thread-transfers",
            Self::ThreadOrganize => "thread-organize",
            Self::ThreadUpdate => "thread-update",
            Self::QueueList => "queue-list",
            Self::QueueRead => "queue-read",
            Self::QueueEdit => "queue-edit",
            Self::QueueCancel => "queue-cancel",
            Self::QueueReorder => "queue-reorder",
            Self::QueueSteer => "queue-steer",
            Self::QuestionList => "question-list",
            Self::QuestionRead => "question-read",
            Self::QuestionRespond => "question-respond",
            Self::ProjectList => "project-list",
            Self::ProjectRead => "project-read",
            Self::ProjectCreate => "project-create",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub display_name: String,
    /// The action, its running and completed forms, and its object.
    pub labels: [&'static str; 4],
    pub summary_action: ToolSummaryAction,
}

/// The Host's MCP server name.
const SERVER_NAME: &str = "orchestration";

use ToolSummaryAction as A;

/// Every served tool, in the Host catalog's order.
const TOOLS: [(&str, [&str; 4], ToolSummaryAction); 31] = [
    (
        "orchestrator_capabilities",
        ["Get", "Getting", "Got", "orchestration capabilities"],
        A::Capabilities,
    ),
    (
        "delegate_task",
        ["Delegate", "Delegating", "Delegated", "a child task"],
        A::Delegate,
    ),
    (
        "task_status",
        ["Get", "Getting", "Got", "delegated task status"],
        A::TaskStatus,
    ),
    (
        "task_cancel",
        [
            "Cancel",
            "Canceling",
            "Requested cancellation of",
            "delegated task",
        ],
        A::TaskCancel,
    ),
    (
        "create_threads",
        ["Create", "Creating", "Created", "threads"],
        A::ThreadCreate,
    ),
    (
        "thread_list",
        ["List", "Listing", "Listed", "threads"],
        A::ThreadList,
    ),
    (
        "thread_read",
        ["Read", "Reading", "Read", "a thread"],
        A::ThreadRead,
    ),
    (
        "thread_update",
        ["Update", "Updating", "Updated", "thread metadata"],
        A::ThreadUpdate,
    ),
    (
        "thread_send",
        ["Send", "Sending", "Sent", "to a thread"],
        A::ThreadSend,
    ),
    (
        "thread_wait",
        ["Wait", "Waiting", "Waited", "for a thread"],
        A::ThreadWait,
    ),
    (
        "thread_interrupt",
        [
            "Interrupt",
            "Interrupting",
            "Requested an interrupt of",
            "a thread",
        ],
        A::ThreadInterrupt,
    ),
    (
        "thread_search",
        ["Search", "Searching", "Searched", "thread content"],
        A::ThreadSearch,
    ),
    (
        "thread_fork",
        ["Fork", "Forking", "Requested a fork of", "this thread"],
        A::ThreadFork,
    ),
    (
        "thread_merge_back",
        ["Merge", "Merging", "Requested a merge of", "thread context"],
        A::ThreadMerge,
    ),
    (
        "thread_transfers",
        ["Read", "Reading", "Read", "thread transfers"],
        A::ThreadTransfers,
    ),
    (
        "thread_configuration",
        ["Read", "Reading", "Read", "thread configuration"],
        A::ThreadConfiguration,
    ),
    (
        "thread_configure",
        ["Set", "Setting", "Set", "thread model"],
        A::ThreadConfigure,
    ),
    (
        "pending_request_list",
        ["List", "Listing", "Listed", "pending questions"],
        A::QuestionList,
    ),
    (
        "pending_request_read",
        ["Read", "Reading", "Read", "pending questions"],
        A::QuestionRead,
    ),
    (
        "pending_request_respond",
        ["Answer", "Answering", "Answered", "pending questions"],
        A::QuestionRespond,
    ),
    (
        "thread_organize",
        ["Organize", "Organizing", "Organized", "a thread"],
        A::ThreadOrganize,
    ),
    (
        "queue_list",
        ["List", "Listing", "Listed", "queued messages"],
        A::QueueList,
    ),
    (
        "queue_read",
        ["Read", "Reading", "Read", "a queued message"],
        A::QueueRead,
    ),
    (
        "queue_edit",
        ["Edit", "Editing", "Edited", "a queued message"],
        A::QueueEdit,
    ),
    (
        "queue_cancel",
        [
            "Cancel",
            "Canceling",
            "Requested cancellation of",
            "a queued run",
        ],
        A::QueueCancel,
    ),
    (
        "queue_reorder",
        ["Reorder", "Reordering", "Reordered", "a queued run"],
        A::QueueReorder,
    ),
    (
        "queue_promote_to_steer",
        [
            "Steer with",
            "Steering with",
            "Requested steering with",
            "a queued message",
        ],
        A::QueueSteer,
    ),
    (
        "thread_launch",
        ["Launch", "Launching", "Launched", "a project thread"],
        A::ThreadCreate,
    ),
    (
        "project_list",
        ["List", "Listing", "Listed", "projects"],
        A::ProjectList,
    ),
    (
        "project_read",
        ["Read", "Reading", "Read", "a project"],
        A::ProjectRead,
    ),
    (
        "project_create",
        ["Register", "Registering", "Registered", "a project"],
        A::ProjectCreate,
    ),
];

/// The served tool names, used to gate loose name matching.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().map(|(name, ..)| *name)
}

fn definition(name: &str) -> Option<ToolDefinition> {
    TOOLS
        .iter()
        .find(|(tool, ..)| *tool == name)
        .map(|(_, labels, summary_action)| ToolDefinition {
            display_name: format!("{} {}", labels[0], labels[3]),
            labels: *labels,
            summary_action: *summary_action,
        })
}

static COMPLETION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i){JS_SPACE}+(?:complete|completed){JS_SPACE}*$"
    ))
    .expect("completion suffix pattern compiles")
});
static MCP_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^mcp__(?<server>{JS_DOT}+?)__(?<tool>{JS_DOT}+)$"
    ))
    .expect("MCP name pattern compiles")
});
static NAMESPACED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^(?<server>{SERVER_NAME})(?:[.:/]|{JS_SPACE}*·{JS_SPACE}*)(?<tool>{JS_DOT}+)$"
    ))
    .expect("namespaced name pattern compiles")
});
static PREFIXED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^(?:mcp[-_]{{1,2}})?{SERVER_NAME}(?:__|[-_.:/ ])(?<tool>{JS_DOT}+)$"
    ))
    .expect("prefixed name pattern compiles")
});

/// A label without a trailing "complete"/"completed".
pub(crate) fn without_completion_suffix(value: &str) -> String {
    super::js_text::js_trim(&COMPLETION_SUFFIX.replace(value, "")).into()
}

/// Providers disagree on how the injected server prefixes its tools:
/// `mcp__orchestration__x` (Claude), `orchestration.x` (Codex), plus single
/// underscore, colon, slash, dash, and space separators. The prefix match is
/// deliberately loose because the inventory is the real gate.
fn resolve_tool_name(value: &str) -> Option<String> {
    let label = without_completion_suffix(value);
    if let Some(captures) = MCP_NAME.captures(&label) {
        return (captures["server"].to_lowercase() == SERVER_NAME)
            .then(|| captures["tool"].to_owned());
    }
    if let Some(captures) = NAMESPACED_NAME.captures(&label) {
        return Some(captures["tool"].to_owned());
    }
    let candidate = PREFIXED_NAME
        .captures(&label)
        .and_then(|captures| captures.name("tool"))
        .map_or(label.as_str(), |tool| tool.as_str());
    tool_names()
        .any(|name| name == candidate)
        .then(|| candidate.to_owned())
}

pub fn resolve_tool_definition(tool_name: Option<&str>) -> Option<ToolDefinition> {
    definition(&resolve_tool_name(tool_name?)?)
}

pub fn resolve_tool_presentation(tool_name: Option<&str>) -> Option<ToolCatalogPresentation> {
    resolve_tool_definition(tool_name).map(|definition| ToolCatalogPresentation {
        display_name: definition.display_name,
        logo: ToolLogo::App,
    })
}

pub fn resolve_tool_summary_action(tool_name: Option<&str>) -> Option<ToolSummaryAction> {
    resolve_tool_definition(tool_name).map(|definition| definition.summary_action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presentation(display_name: &str) -> Option<ToolCatalogPresentation> {
        Some(ToolCatalogPresentation {
            display_name: display_name.into(),
            logo: ToolLogo::App,
        })
    }

    #[test]
    fn recognizes_every_tool_across_provider_prefixes_and_completion_suffixes() {
        for tool in tool_names() {
            let expected = resolve_tool_presentation(Some(tool));
            assert!(expected.is_some(), "{tool}");
            for prefix in [
                "mcp__orchestration__",
                "mcp__Orchestration__",
                "Orchestration.",
                "orchestration/",
                "orchestration:",
                "mcp_orchestration_",
                "Orchestration ",
                "orchestration · ",
            ] {
                assert_eq!(
                    resolve_tool_presentation(Some(&format!("{prefix}{tool} completed"))),
                    expected,
                    "{prefix}{tool}"
                );
            }
            assert_eq!(
                resolve_tool_presentation(Some(&format!("mcp__another-server__{tool}"))),
                None,
                "{tool}"
            );
        }
    }

    #[test]
    fn pretty_prints_claude_mcp_tool_names() {
        assert_eq!(
            resolve_tool_presentation(Some("mcp__orchestration__thread_read")),
            presentation("Read a thread")
        );
    }

    #[test]
    fn pretty_prints_codex_mcp_tool_names() {
        assert_eq!(
            resolve_tool_presentation(Some("orchestration.create_threads")),
            presentation("Create threads")
        );
    }

    #[test]
    fn pretty_prints_thread_metadata_updates() {
        assert_eq!(
            resolve_tool_presentation(Some("mcp__orchestration__thread_update")),
            presentation("Update thread metadata")
        );
    }

    #[test]
    fn pretty_prints_bare_mcp_toolkit_names() {
        assert_eq!(
            resolve_tool_presentation(Some("queue_list")),
            presentation("List queued messages")
        );
    }

    #[test]
    fn matches_the_separator_variants_acp_registry_agents_emit() {
        for name in [
            "mcp_orchestration_delegate_task",
            "orchestration:delegate_task",
            "orchestration/delegate_task",
            "orchestration delegate_task",
            "Orchestration delegate_task",
            "orchestration__delegate_task",
        ] {
            assert_eq!(
                resolve_tool_presentation(Some(name)).map(|p| p.display_name),
                Some("Delegate a child task".into()),
                "{name}"
            );
        }
    }

    #[test]
    fn keeps_unknown_mcp_tools_on_the_generic_renderer_path() {
        assert_eq!(
            resolve_tool_presentation(Some("mcp__github__search_issues")),
            None
        );
        assert_eq!(
            resolve_tool_presentation(Some("orchestration.not_a_real_tool")),
            None
        );
    }
}
