//! What an expanded work row shows: the call, the formatted output, and
//! whether the Host withheld content `LoadItemDetail` fetches.
use crate::js_text::js_trim;
use agent_domain::{Item, ItemKind, ItemStatus, RequestBody, State};
use serde_json::Value;

const MAX_TEXT_BLOCK_DEPTH: usize = 4;

/// Delivery replaces a large tool input with `{ summary, truncated: true }`.
fn is_summarized_value(value: &Value) -> bool {
    value.get("truncated") == Some(&Value::Bool(true))
        && value.get("summary").is_some_and(Value::is_string)
}

fn text_from_blocks(value: &Value, depth: usize) -> Option<String> {
    if depth > MAX_TEXT_BLOCK_DEPTH {
        return None;
    }
    let block = match value {
        Value::String(text) => return Some(text.clone()),
        Value::Array(blocks) => {
            return blocks
                .iter()
                .map(|block| text_from_blocks(block, depth + 1))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join("\n"));
        }
        Value::Object(block) => block,
        _ => return None,
    };
    let string = |key: &str| block.get(key).and_then(Value::as_str);
    let kind = string("type");
    if kind == Some("text")
        && let Some(text) = string("text")
    {
        return Some(text.to_owned());
    }
    if kind == Some("image") {
        return Some("[image]".to_owned());
    }
    if kind == Some("resource_link")
        && let Some(uri) = string("uri")
    {
        return Some(uri.to_owned());
    }
    if kind == Some("resource")
        && let Some(Value::Object(resource)) = block.get("resource")
    {
        let field = |key: &str| resource.get(key).and_then(Value::as_str);
        if let Some(text) = field("text").or_else(|| field("uri")) {
            return Some(text.to_owned());
        }
    }
    let keys: Vec<&str> = block
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "isError" && *key != "is_error")
        .collect();
    // MCP and provider tool results wrap their text in `content`.
    // `structuredContent` usually repeats it as data, so it only shows when
    // the text is empty.
    if keys == ["content"] {
        return text_from_blocks(&block["content"], depth + 1);
    }
    if keys.len() == 2 && keys.contains(&"content") && keys.contains(&"structuredContent") {
        return text_from_blocks(&block["content"], depth + 1)
            .filter(|text| !js_trim(text).is_empty());
    }
    None
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Tools often return JSON as minified text, one document per line. Indents each.
fn pretty_json_text(text: &str) -> String {
    let trimmed = js_trim(text);
    if !trimmed.starts_with(['[', '{']) {
        return text.to_owned();
    }
    if let Ok(whole) = serde_json::from_str::<Value>(trimmed) {
        return pretty_json(&whole);
    }
    let documents: Option<Vec<Value>> = trimmed
        .split('\n')
        .filter(|line| !js_trim(line).is_empty())
        .map(|line| serde_json::from_str(js_trim(line)).ok())
        .collect();
    match documents {
        Some(documents) => documents
            .iter()
            .map(pretty_json)
            .collect::<Vec<_>>()
            .join("\n\n"),
        None => text.to_owned(),
    }
}

/// A tool input or output for display: text blocks as text, the rest as JSON.
fn format_tool_value(value: Option<&Value>) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    if let Some(text) = text_from_blocks(value, 0) {
        return (!js_trim(&text).is_empty()).then(|| pretty_json_text(&text));
    }
    let json = pretty_json(value);
    (json != "{}" && json != "[]").then_some(json)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallArg {
    pub key: String,
    pub value: String,
}

/// The call a tool row's body shows above its result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallLines {
    pub command: Option<String>,
    pub args: Option<Vec<ToolCallArg>>,
    pub args_text: Option<String>,
}

impl ToolCallLines {
    pub fn is_empty(&self) -> bool {
        self.command.is_none() && self.args.is_none() && self.args_text.is_none()
    }
}

/// The full command, the arguments as `key value` pairs, or formatted text
/// when the arguments are not flat. A command wins over arguments.
pub fn tool_call_lines(command: Option<&str>, args: Option<&Value>) -> ToolCallLines {
    if let Some(command) = command {
        // The row title truncates to its width, so the body has the full command.
        let command = js_trim(command);
        return ToolCallLines {
            command: (!command.is_empty()).then(|| command.to_owned()),
            ..ToolCallLines::default()
        };
    }
    if let Some(args @ Value::Object(entries)) = args
        && !is_summarized_value(args)
    {
        let entries: Vec<ToolCallArg> = entries
            .iter()
            .map(|(key, value)| ToolCallArg {
                key: key.clone(),
                // An empty string or null can be the point of a call (a reset), so show it.
                value: match value {
                    Value::String(text) if !text.is_empty() => text.clone(),
                    value => value.to_string(),
                },
            })
            .collect();
        return ToolCallLines {
            args: (!entries.is_empty()).then_some(entries),
            ..ToolCallLines::default()
        };
    }
    ToolCallLines {
        args_text: format_tool_value(args),
        ..ToolCallLines::default()
    }
}

/// Cache key for a fetched item. A running item keeps one key, so an open row
/// fetches once while it streams and again when it finishes.
pub fn turn_item_detail_revision(item: &Item) -> String {
    match item.status {
        ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting => "live".to_owned(),
        _ => item
            .completed_at
            .as_ref()
            .unwrap_or(&item.started_at)
            .as_str()
            .to_owned(),
    }
}

/// True when the delivered item withholds content `LoadItemDetail` returns.
pub fn turn_item_needs_detail_fetch(item: &Item) -> bool {
    match &item.kind {
        ItemKind::CommandExecution { .. } => item.output_omitted,
        ItemKind::DynamicTool { input, .. } => item.output_omitted || is_summarized_value(&input.0),
        _ => false,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSearchResult {
    pub title: Option<String>,
    pub url: Option<String>,
    pub snippet: Option<String>,
}

/// Search results as titled links. Claude nests its links under each
/// result's `content` and adds plain-text summaries, which are skipped.
pub fn web_search_results(results: Option<&Value>) -> Vec<WebSearchResult> {
    let result = |value: &Value| {
        let string = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        WebSearchResult {
            title: string("title"),
            url: string("url").filter(|url| !js_trim(url).is_empty()),
            snippet: string("snippet"),
        }
    };
    let Some(Value::Array(results)) = results else {
        return Vec::new();
    };
    results
        .iter()
        .filter(|entry| entry.is_object())
        .flat_map(|entry| match entry.get("content") {
            Some(Value::Array(links)) => links
                .iter()
                .filter(|link| link.is_object())
                .map(result)
                .collect(),
            _ => vec![result(entry)],
        })
        .collect()
}

/// The tool output a fetched item carries, formatted for display.
pub fn turn_item_output_text(item: &Item) -> Option<String> {
    match &item.kind {
        ItemKind::CommandExecution { .. } => {
            (!js_trim(&item.text).is_empty()).then(|| item.text.clone())
        }
        ItemKind::DynamicTool { output, .. } => {
            if item.output_omitted {
                None
            } else {
                format_tool_value(output.as_ref().map(|output| &output.0))
            }
        }
        ItemKind::WebSearch { results, .. } => {
            let results = web_search_results(results.as_ref().map(|results| &results.0));
            (!results.is_empty()).then(|| {
                results
                    .iter()
                    .map(|result| {
                        let title = result
                            .title
                            .as_deref()
                            .map(js_trim)
                            .filter(|title| !title.is_empty());
                        [
                            title.or(result.url.as_deref()),
                            result
                                .title
                                .as_deref()
                                .filter(|title| !title.is_empty())
                                .and(result.url.as_deref()),
                            result.snippet.as_deref().map(js_trim),
                        ]
                        .into_iter()
                        .flatten()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
        }
        _ => None,
    }
}

/// Whether expanding the item shows anything. A row without content must not
/// offer a disclosure that opens to an empty panel.
pub fn turn_item_has_detail(item: &Item, state: &State) -> bool {
    let plan = |id| state.plans.iter().find(|plan| &plan.id == id);
    let request = |id| state.requests.iter().find(|request| &request.id == id);
    match &item.kind {
        ItemKind::Reasoning => !js_trim(&item.text).is_empty(),
        ItemKind::CommandExecution {
            command, exit_code, ..
        } => {
            tool_call_lines(Some(command), None).command.is_some()
                || item.output_omitted
                || !js_trim(&item.text).is_empty()
                || exit_code.is_some_and(|code| code != 0)
        }
        ItemKind::FileChange { .. } | ItemKind::Fork { .. } => true,
        ItemKind::WebSearch { query, results } => {
            !web_search_results(results.as_ref().map(|results| &results.0)).is_empty()
                || !js_trim(query).is_empty()
        }
        ItemKind::DynamicTool { input, .. } => {
            item.output_omitted || !tool_call_lines(None, Some(&input.0)).is_empty()
        }
        ItemKind::ApprovalRequest { request: id } => request(id).is_some_and(|request| {
            matches!(&request.body, RequestBody::Approval { detail: Some(detail), .. }
                if !js_trim(detail).is_empty())
        }),
        ItemKind::UserInputRequest { request: id } => request(id).is_some_and(|request| {
            matches!(&request.body, RequestBody::Questions { questions } if !questions.is_empty())
        }),
        ItemKind::Notification { notification } => notification
            .detail
            .as_deref()
            .is_some_and(|detail| !js_trim(detail).is_empty()),
        ItemKind::SystemNotice { message } | ItemKind::Error { message, .. } => {
            !js_trim(message).is_empty()
        }
        ItemKind::ProposedPlan { plan: id } => {
            plan(id).is_some_and(|plan| !js_trim(&plan.markdown).is_empty())
        }
        ItemKind::TodoList { plan: id } => plan(id).is_some_and(|plan| !plan.steps.is_empty()),
        ItemKind::Subagent { task } => state.tasks.iter().any(|candidate| &candidate.id == task),
        _ => false,
    }
}

#[cfg(test)]
#[path = "item_detail_tests.rs"]
mod tests;
