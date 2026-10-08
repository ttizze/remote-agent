//! The mobile item inspector: an item's lifecycle, provenance and inputs,
//! without cached raw output.
use super::mobile::{FeedVisibility, format_item_full_detail, subagent_task};
use crate::sync::Detail;
use crate::view::time::format_duration;
use crate::view::work_log::ItemType;
use crate::view::work_log::item_detail::web_search_results;
use crate::view::work_log::item_support::resolve_item_support;
use agent_domain::{Item, ItemKind, RequestBody, ResponseCapability, State, TurnItemId};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InspectorField {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InspectorBlock {
    pub label: String,
    pub value: String,
    pub monospaced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InspectorFileLink {
    pub label: String,
    pub path: String,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InspectorWebLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityInspector {
    pub fields: Vec<InspectorField>,
    pub blocks: Vec<InspectorBlock>,
    pub file_links: Vec<InspectorFileLink>,
    pub web_links: Vec<InspectorWebLink>,
    pub structured_details: String,
}

fn format_structured(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

/// The snake-case name a status serializes to.
fn status_name(status: impl Serialize) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn spaced(value: &str) -> String {
    value.replace('_', " ")
}

fn capability_name(capability: ResponseCapability) -> &'static str {
    match capability {
        ResponseCapability::Live => "live",
        ResponseCapability::Message => "message",
        ResponseCapability::NotResumable => "not resumable",
    }
}

struct Inspector {
    fields: Vec<InspectorField>,
    blocks: Vec<InspectorBlock>,
}

impl Inspector {
    fn field(&mut self, label: &str, value: String) {
        self.fields.push(InspectorField {
            label: label.into(),
            value,
        });
    }

    fn block(&mut self, label: &str, value: Option<&str>, monospaced: bool) {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            self.blocks.push(InspectorBlock {
                label: label.into(),
                value: value.into(),
                monospaced,
            });
        }
    }
}

fn file_change_links(changes: &Value) -> Vec<InspectorFileLink> {
    let link = |change: &Value| {
        let path = ["path", "file_path", "notebook_path", "filePath"]
            .into_iter()
            .find_map(|key| change.get(key)?.as_str())?;
        let kind = change.get("kind");
        let operation = kind
            .and_then(|kind| kind.as_str().or_else(|| kind.get("type")?.as_str()))
            .unwrap_or("modify");
        let moved = kind
            .and_then(|kind| kind.get("move_path")?.as_str())
            .filter(|moved| !moved.is_empty());
        Some(match moved {
            Some(moved) => InspectorFileLink {
                label: format!("{operation} {path} → {moved}"),
                path: moved.into(),
                line: None,
            },
            None => InspectorFileLink {
                label: format!("{operation} {path}"),
                path: path.into(),
                line: None,
            },
        })
    };
    match changes {
        Value::Array(changes) => changes.iter().filter_map(link).collect(),
        change => link(change).into_iter().collect(),
    }
}

/// The inspector of one item; a loaded detail replaces the delivered item.
pub fn build_activity_inspector(
    state: &State,
    item_id: &TurnItemId,
    details: &BTreeMap<TurnItemId, Detail>,
    now_ms: i64,
) -> Option<ActivityInspector> {
    let support = resolve_item_support(state, item_id);
    let loaded_task = match details.get(item_id) {
        Some(Detail::LoadedWithTask { task, .. }) => Some(task.as_ref()),
        _ => None,
    };
    let item: Item = match details.get(item_id) {
        Some(Detail::Loaded(item)) => (**item).clone(),
        Some(Detail::LoadedWithTask { item, .. }) => (**item).clone(),
        _ => state.notification_card(support.item.as_ref()?).into_owned(),
    };
    let visibility = if state
        .inherited_items
        .iter()
        .any(|candidate| &candidate.id == item_id)
    {
        FeedVisibility::Inherited
    } else {
        FeedVisibility::Local
    };
    let mut inspector = Inspector {
        fields: vec![],
        blocks: vec![],
    };
    inspector.field("Item", spaced(ItemType::of(&item.kind).as_str()));
    inspector.field("Status", spaced(&status_name(item.status)));
    let end = item
        .completed_at
        .as_ref()
        .map_or(now_ms, |completed| completed.millis());
    inspector.field(
        "Duration",
        format_duration((end - item.started_at.millis()).max(0)),
    );
    if visibility != FeedVisibility::Local {
        inspector.field("Visibility", visibility.as_str().into());
    }
    if let Some(run) = &support.run {
        inspector.field("Run", status_name(run.status));
    }
    if let Some(attempt) = support.attempts.last() {
        inspector.field(
            "Attempt",
            format!("{} · {}", attempt.ordinal, status_name(attempt.status)),
        );
    }
    if let Some(request) = &support.request {
        inspector.field(
            "Request",
            format!(
                "{} · {}",
                status_name(request.status),
                capability_name(request.capability)
            ),
        );
    }
    if support.attempts.len() > 1 {
        let history = support
            .attempts
            .iter()
            .map(|attempt| {
                format!(
                    "Attempt {} · {}",
                    attempt.ordinal,
                    status_name(attempt.status)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        inspector.block("Attempt history", Some(&history), true);
    }

    let mut file_links = vec![];
    let mut web_links = vec![];
    match &item.kind {
        ItemKind::Reasoning => inspector.block("Reasoning", Some(&item.text), false),
        ItemKind::CommandExecution {
            command, exit_code, ..
        } => {
            inspector.block("Command", Some(command), true);
            if let Some(code) = exit_code {
                inspector.block(
                    "Exit",
                    Some(&format!("Process exited with code {code}")),
                    true,
                );
            }
        }
        ItemKind::FileChange { changes } => file_links = file_change_links(&changes.0),
        ItemKind::WebSearch { query, results } => {
            inspector.block("Queries", Some(query), true);
            for result in web_search_results(results.as_ref().map(|results| &results.0)) {
                if let Some(url) = &result.url {
                    web_links.push(InspectorWebLink {
                        label: result.title.clone().unwrap_or_else(|| url.clone()),
                        url: url.clone(),
                    });
                }
                if let Some(snippet) = result.snippet.as_deref() {
                    inspector.block(
                        result.title.as_deref().unwrap_or("Search result"),
                        Some(snippet),
                        false,
                    );
                }
            }
        }
        ItemKind::DynamicTool { input, .. } => {
            inspector.block("Input", Some(&format_structured(&input.0)), true);
        }
        ItemKind::ApprovalRequest { .. } => {
            if let Some(RequestBody::Approval { detail, .. }) =
                support.request.as_ref().map(|request| &request.body)
            {
                inspector.block("Prompt", detail.as_deref(), false);
            }
        }
        ItemKind::UserInputRequest { .. } => {
            if let Some(RequestBody::Questions { questions }) =
                support.request.as_ref().map(|request| &request.body)
            {
                let questions = questions
                    .iter()
                    .map(|question| question.question.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                inspector.block("Questions", Some(&questions), false);
            }
        }
        ItemKind::Subagent { .. } => {
            if let Some(task) = loaded_task.or_else(|| subagent_task(state, &item)) {
                inspector.block("Prompt", Some(&task.prompt), false);
                inspector.block("Progress", task.progress.as_deref(), false);
                inspector.block("Result", task.result.as_deref(), false);
                inspector.field(
                    "Delegated task",
                    format!(
                        "{} · {}",
                        if task.app_owned() {
                            "app owned"
                        } else {
                            "provider native"
                        },
                        status_name(task.status)
                    ),
                );
            }
        }
        ItemKind::Error {
            message,
            code,
            retryable,
            ..
        } => {
            inspector.block("Error", Some(message), false);
            if let Some(code) = code.as_deref().filter(|code| !code.is_empty()) {
                inspector.field("Code", code.into());
            }
            if let Some(retryable) = retryable {
                inspector.field("Retryable", if *retryable { "yes" } else { "no" }.into());
            }
        }
        ItemKind::ProposedPlan { plan } => {
            let markdown = state
                .plans
                .iter()
                .find(|candidate| &candidate.id == plan)
                .map_or_else(|| item.text.clone(), |plan| plan.markdown.clone());
            inspector.block("Plan", Some(&markdown), false);
        }
        ItemKind::TodoList { plan } => {
            let tasks = state
                .plans
                .iter()
                .find(|candidate| &candidate.id == plan)
                .map(|plan| {
                    plan.steps
                        .iter()
                        .map(|step| {
                            let mark = if step.status == "completed" {
                                "✓"
                            } else {
                                "○"
                            };
                            format!("{mark} {}", step.text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            inspector.block("Tasks", tasks.as_deref(), false);
        }
        ItemKind::Compaction { before, after } => {
            inspector.block("Summary", Some(&item.text), false);
            if before.is_some() || after.is_some() {
                let count =
                    |value: &Option<u64>| value.map_or("?".into(), |value| value.to_string());
                inspector.field(
                    "Context tokens",
                    format!("{} → {}", count(before), count(after)),
                );
            }
        }
        ItemKind::RunInterruptRequest | ItemKind::RunInterruptResult { .. } => {
            inspector.block("Message", Some(&item.text), false);
        }
        ItemKind::Fork { .. }
        | ItemKind::ThreadCreated { .. }
        | ItemKind::UserMessage { .. }
        | ItemKind::AssistantMessage { .. }
        | ItemKind::SystemNotice { .. }
        | ItemKind::Notification { .. } => {}
    }
    Some(ActivityInspector {
        fields: inspector.fields,
        blocks: inspector.blocks,
        file_links,
        web_links,
        structured_details: format_item_full_detail(visibility, &item),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::work_log::fixtures::*;
    use agent_domain::{
        Attempt, AttemptStatus, ItemStatus, RunAttemptId, RunId, RunStatus, Timestamp,
    };
    use serde_json::json;

    fn timed(item: Item) -> Item {
        let mut item = item
            .run("run-1")
            .started("2026-06-20T00:00:00.000Z")
            .completed(Some("2026-06-20T00:00:02.000Z"));
        item.attempt = Some(RunAttemptId::new("attempt-2").unwrap());
        item
    }

    fn attempt(id: &str, ordinal: u64, status: AttemptStatus) -> Attempt {
        Attempt {
            id: RunAttemptId::new(id).unwrap(),
            run: RunId::new("run-1").unwrap(),
            ordinal,
            status,
            native_thread: None,
            native_turn: None,
            native_head: None,
            accepted: true,
            usage: None,
            context_usage: None,
            turn_usage: None,
            usage_accumulator: None,
            usage_observed: false,
            rejected_limits: Default::default(),
            started_at: Timestamp::parse("2026-06-20T00:00:00.000Z").unwrap(),
            completed_at: None,
        }
    }

    /// An inherited item, as the reference inspects them.
    fn inherited(item: Item, rest: State) -> State {
        State {
            inherited_items: vec![item],
            runs: vec![run("run-1", 1, RunStatus::Completed)],
            ..rest
        }
    }

    fn inspect(state: &State, id: &str) -> ActivityInspector {
        build_activity_inspector(state, &TurnItemId::new(id).unwrap(), &BTreeMap::new(), 0).unwrap()
    }

    fn field(label: &str, value: &str) -> InspectorField {
        InspectorField {
            label: label.into(),
            value: value.into(),
        }
    }

    #[test]
    fn presents_command_lifecycle_and_exit_state_without_cached_raw_output() {
        let item = timed(command("command", "vp check")).text("all checks passed");
        let state = inherited(
            item,
            State {
                attempts: vec![
                    attempt("attempt-1", 1, AttemptStatus::Superseded),
                    attempt("attempt-2", 2, AttemptStatus::Completed),
                ],
                ..state(vec![])
            },
        );
        let model = inspect(&state, "command");
        for expected in [
            field("Duration", "2.0s"),
            field("Run", "completed"),
            field("Visibility", "inherited"),
        ] {
            assert!(model.fields.contains(&expected), "{expected:?}");
        }
        let labels: Vec<&str> = model
            .blocks
            .iter()
            .map(|block| block.label.as_str())
            .collect();
        for label in ["Command", "Exit", "Attempt history"] {
            assert!(labels.contains(&label), "{label}");
        }
        assert!(model.blocks.contains(&InspectorBlock {
            label: "Command".into(),
            value: "vp check".into(),
            monospaced: true,
        }));
        assert!(model.blocks.contains(&InspectorBlock {
            label: "Exit".into(),
            value: "Process exited with code 0".into(),
            monospaced: true,
        }));
        assert!(!labels.contains(&"Output"));
        assert!(!model.structured_details.contains("all checks passed"));
    }

    #[test]
    fn exposes_web_result_provenance_plus_dynamic_structured_data() {
        let mut search = timed(web_search("web-search", "Effect Schema docs"));
        if let ItemKind::WebSearch { results, .. } = &mut search.kind {
            *results = Some(agent_domain::Json(json!([{
                "title": "Schema",
                "url": "https://effect.website/docs/schema",
                "snippet": "Typed schemas",
            }])));
        }
        let model = inspect(&inherited(search, state(vec![])), "web-search");
        assert_eq!(
            model.web_links,
            [InspectorWebLink {
                label: "Schema".into(),
                url: "https://effect.website/docs/schema".into(),
            }]
        );
        assert!(
            model
                .blocks
                .iter()
                .any(|block| block.label == "Schema" && block.value == "Typed schemas")
        );

        let tool = timed(dynamic_tool(
            "dynamic",
            "custom",
            json!({ "nested": { "value": 1 } }),
            Some(json!({ "ok": true })),
        ));
        let model = inspect(&inherited(tool, state(vec![])), "dynamic");
        assert!(
            model
                .blocks
                .iter()
                .any(|block| block.label == "Input" && block.value.contains("\"nested\""))
        );
        assert!(model.structured_details.contains("\"DynamicTool\""));
        assert!(!model.blocks.iter().any(|block| block.label == "Output"));
        assert!(!model.structured_details.contains("\"ok\""));
    }

    #[test]
    fn keeps_file_links_without_cached_patch_bodies() {
        let item = timed(file_change(
            "file",
            json!({ "file_path": "src/example.ts", "old_string": "RAW_BEFORE", "new_string": "RAW_AFTER" }),
        ))
        .text("RAW_PATCH");
        let state = State {
            runs: vec![run("run-1", 1, RunStatus::Completed)],
            ..state(vec![item])
        };
        let model = inspect(&state, "file");
        assert_eq!(
            model.file_links,
            [InspectorFileLink {
                label: "modify src/example.ts".into(),
                path: "src/example.ts".into(),
                line: None,
            }]
        );
        assert_eq!(model.blocks, []);
        assert!(!model.structured_details.contains("RAW_"));
        assert!(model.structured_details.contains("src/example.ts"));
    }

    #[test]
    fn prefers_live_subagent_progress_from_supporting_state() {
        let item = timed(subagent("subagent", "node-1"));
        let mut task = task("node-1", "child", "Audit the reducer");
        task.progress = Some("Reading projection tests".into());
        let model = inspect(
            &inherited(
                item,
                State {
                    tasks: vec![task],
                    ..state(vec![])
                },
            ),
            "subagent",
        );
        assert!(
            model
                .fields
                .contains(&field("Delegated task", "provider native · running"))
        );
        assert!(model.blocks.contains(&InspectorBlock {
            label: "Progress".into(),
            value: "Reading projection tests".into(),
            monospaced: false,
        }));
    }

    #[test]
    fn inspects_the_loaded_detail_instead_of_the_delivered_item() {
        let delivered = timed(dynamic_tool(
            "dynamic",
            "custom",
            json!({ "truncated": true, "summary": "Input withheld" }),
            None,
        ))
        .output_omitted();
        let loaded = timed(dynamic_tool(
            "dynamic",
            "custom",
            json!({ "query": "full input" }),
            None,
        ))
        .status(ItemStatus::Completed);
        let state = State {
            runs: vec![run("run-1", 1, RunStatus::Completed)],
            ..state(vec![delivered])
        };
        let id = TurnItemId::new("dynamic").unwrap();
        let details = BTreeMap::from([(id.clone(), Detail::Loaded(Box::new(loaded)))]);
        let model = build_activity_inspector(&state, &id, &details, 0).unwrap();
        assert!(
            model
                .blocks
                .iter()
                .any(|block| block.label == "Input" && block.value.contains("full input"))
        );
    }

    #[test]
    fn routes_the_task_from_loaded_detail_to_a_subagent_inspector() {
        let delivered = timed(subagent("subagent", "node-1"));
        let loaded = delivered.clone();
        let state = State {
            runs: vec![run("run-1", 1, RunStatus::Completed)],
            ..state(vec![delivered])
        };
        let id = TurnItemId::new("subagent").unwrap();
        let mut loaded_task = task("node-1", "child", "Review the complete output");
        loaded_task.progress = Some("Still reading".into());
        let details = BTreeMap::from([(
            id.clone(),
            Detail::LoadedWithTask {
                item: Box::new(loaded),
                task: Box::new(loaded_task),
            },
        )]);
        let model = build_activity_inspector(&state, &id, &details, 0).unwrap();
        assert!(model.blocks.iter().any(|block| {
            block.label == "Prompt" && block.value == "Review the complete output"
        }));
        assert!(
            model
                .blocks
                .iter()
                .any(|block| { block.label == "Progress" && block.value == "Still reading" })
        );
    }
}
