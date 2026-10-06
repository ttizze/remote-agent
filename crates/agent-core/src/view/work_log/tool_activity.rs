//! What a tool call did (ran, read, changed, searched) and its read and search
//! headings, from the structured tool input.
use super::ItemType;
use crate::js_text::{JS_SPACE, js_trim};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

type Record = Map<String, Value>;

fn as_record(value: Option<&Value>) -> Option<&Record> {
    value.and_then(Value::as_object)
}

fn as_trimmed_string(value: Option<&Value>) -> Option<&str> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then_some(trimmed)
}

/// A Claude `Skill` call: the skill it loads and the arguments it passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocation {
    pub name: String,
    pub args: Option<String>,
}

pub fn claude_skill_invocation(tool_name: Option<&str>, input: &Value) -> Option<SkillInvocation> {
    if tool_name != Some("Skill") {
        return None;
    }
    let record = input.as_object();
    let name = as_trimmed_string(record.and_then(|record| record.get("skill")))?;
    Some(SkillInvocation {
        name: name.into(),
        args: as_trimmed_string(record.and_then(|record| record.get("args"))).map(Into::into),
    })
}

/// The heading a dynamic tool derives from its input: CUA's `title`, or the
/// skill a Claude `Skill` call loads.
pub fn dynamic_tool_title(tool_name: Option<&str>, input: &Value) -> Option<String> {
    if tool_name == Some("cua_repl.js") {
        return as_trimmed_string(input.as_object().and_then(|input| input.get("title")))
            .map(Into::into);
    }
    claude_skill_invocation(tool_name, input).map(|skill| format!("Skill: {}", skill.name))
}

fn record_has_keys(value: Option<&Record>) -> bool {
    value.is_some_and(|record| !record.is_empty())
}

const PATH_KEYS: [&str; 8] = [
    "path",
    "filePath",
    "file_path",
    "relativePath",
    "filename",
    "fileName",
    "newPath",
    "oldPath",
];
const NESTED_PATH_KEYS: [&str; 7] = [
    "locations",
    "item",
    "input",
    "result",
    "rawInput",
    "data",
    "changes",
];
const MAX_PATHS: usize = 8;

fn collect_paths(value: &Value, paths: &mut Vec<String>, seen: &mut HashSet<String>, depth: u32) {
    if depth > 4 || paths.len() >= MAX_PATHS {
        return;
    }
    if let Value::Array(entries) = value {
        for entry in entries {
            collect_paths(entry, paths, seen, depth + 1);
            if paths.len() >= MAX_PATHS {
                return;
            }
        }
        return;
    }
    let Some(record) = value.as_object() else {
        return;
    };
    for key in PATH_KEYS {
        let Some(candidate) = as_trimmed_string(record.get(key)) else {
            continue;
        };
        if !seen.insert(candidate.into()) {
            continue;
        }
        paths.push(candidate.into());
        if paths.len() >= MAX_PATHS {
            return;
        }
    }
    for key in NESTED_PATH_KEYS {
        let Some(nested) = record.get(key) else {
            continue;
        };
        collect_paths(nested, paths, seen, depth + 1);
        if paths.len() >= MAX_PATHS {
            return;
        }
    }
}

/// Structured paths from tool input, never file-body text.
pub fn collect_tool_file_paths(data: &Value) -> Vec<String> {
    let mut paths = vec![];
    collect_paths(data, &mut paths, &mut HashSet::new(), 0);
    paths
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolActivityAction {
    Command,
    Read,
    FileChange,
    Search,
    Other,
}

static SERVER_PREFIXED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"__|[./]").expect("server prefix pattern compiles"));
static NAME_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"_|-|{JS_SPACE}")).expect("name separator pattern compiles")
});

fn tool_name_token(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    // Server-prefixed MCP names (`github.read_file`, `mcp__db__find`) are not local reads or searches.
    if trimmed.is_empty() || SERVER_PREFIXED.is_match(trimmed) {
        return None;
    }
    Some(NAME_SEPARATORS.replace_all(trimmed, "").to_lowercase())
}

/// Classifies a tool call from its item type, request kind and structured
/// data (`kind`, `toolName`, `item.tool`), never from its title.
pub fn classify_tool_activity(
    item_type: Option<ItemType>,
    request_kind: Option<&str>,
    data: Option<&Value>,
) -> ToolActivityAction {
    let data = as_record(data);
    let field = |key: &str| data.and_then(|data| data.get(key));
    let request_kind = request_kind
        .map(js_trim)
        .filter(|kind| !kind.is_empty())
        .map(str::to_lowercase);
    let request_kind = request_kind.as_deref();
    let kind = as_trimmed_string(field("kind")).map(str::to_lowercase);
    let kind = kind.as_deref();
    let tool_name =
        tool_name_token(as_trimmed_string(field("toolName")).or_else(|| {
            as_trimmed_string(as_record(field("item")).and_then(|item| item.get("tool")))
        }));
    let tool_name = tool_name.as_deref();

    match item_type {
        Some(ItemType::CommandExecution) => return ToolActivityAction::Command,
        Some(ItemType::FileChange) => return ToolActivityAction::FileChange,
        Some(ItemType::WebSearch) => return ToolActivityAction::Search,
        _ => {}
    }
    if request_kind == Some("command") || kind == Some("execute") {
        return ToolActivityAction::Command;
    }
    if request_kind == Some("file-change")
        || matches!(kind, Some("edit" | "move" | "delete" | "write"))
    {
        return ToolActivityAction::FileChange;
    }
    if kind == Some("search") || matches!(tool_name, Some("find" | "grep" | "glob" | "rg" | "ls")) {
        return ToolActivityAction::Search;
    }
    if request_kind == Some("file-read") || kind == Some("read") {
        return ToolActivityAction::Read;
    }
    if matches!(tool_name, Some("terminal" | "bash" | "shell")) {
        return ToolActivityAction::Command;
    }
    if matches!(tool_name, Some("read" | "readfile")) {
        return ToolActivityAction::Read;
    }
    ToolActivityAction::Other
}

const SEARCH_QUERY_KEYS: [&str; 6] = ["pattern", "query", "searchTerm", "regex", "grep", "needle"];
const SEARCH_GLOB_KEYS: [&str; 6] = [
    "glob",
    "globPattern",
    "glob_pattern",
    "include",
    "filePattern",
    "file_pattern",
];
const SEARCH_TARGET_KEYS: [&str; 6] = [
    "path",
    "target_directory",
    "targetDirectory",
    "directory",
    "cwd",
    "root",
];

fn first_input_string<'a>(record: Option<&'a Record>, keys: &[&str]) -> Option<&'a str> {
    let record = record?;
    keys.iter()
        .find_map(|key| as_trimmed_string(record.get(*key)))
}

fn search_input_record(data: Option<&Record>) -> Option<&Record> {
    let field = |key: &str| data.and_then(|data| data.get(key));
    [
        as_record(field("rawInput")),
        as_record(field("input")),
        as_record(field("item")).and_then(|item| as_record(item.get("input"))),
    ]
    .into_iter()
    .find(|record| record_has_keys(*record))
    .flatten()
    .or(data)
}

fn search_target_name(value: Option<&str>) -> Option<&str> {
    value?
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty() && *part != ".")
}

/// Cursor-style row: "Searched files *.{ts,tsx} in web".
pub fn format_search_tool_label(data: Option<&Value>) -> Option<String> {
    let input = search_input_record(as_record(data));
    let query = first_input_string(input, &SEARCH_QUERY_KEYS);
    let glob = first_input_string(input, &SEARCH_GLOB_KEYS);
    let target = search_target_name(first_input_string(input, &SEARCH_TARGET_KEYS));
    match (query, glob, target) {
        (Some(query), _, Some(target)) => Some(format!("Searched {query} in {target}")),
        (_, Some(glob), Some(target)) => Some(format!("Searched files {glob} in {target}")),
        (_, Some(glob), None) => Some(format!("Searched files {glob}")),
        (Some(query), None, None) => Some(format!("Searched {query}")),
        (None, None, Some(target)) => Some(format!("Searched in {target}")),
        (None, None, None) => None,
    }
}

/// Work-log heading for a file read: verb plus the structured path, never the path alone.
pub fn format_read_tool_label(path: &str, extra_count: usize) -> String {
    let trimmed = js_trim(path);
    let suffix = if extra_count > 0 {
        format!(" +{extra_count} more")
    } else {
        String::new()
    };
    if trimmed.is_empty() {
        format!("Read file{suffix}")
    } else {
        format!("Read {trimmed}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classify(request_kind: Option<&str>, data: Value) -> ToolActivityAction {
        classify_tool_activity(None, request_kind, Some(&data))
    }

    #[test]
    fn classifies_from_kind_and_tool_name_without_sniffing_titles() {
        assert_eq!(
            classify(None, json!({"kind": "read"})),
            ToolActivityAction::Read
        );
        assert_eq!(
            classify(None, json!({"toolName": "Grep"})),
            ToolActivityAction::Search
        );
        assert_eq!(
            classify(None, json!({"toolName": "Read"})),
            ToolActivityAction::Read
        );
        for tool_name in ["github.read_file", "mongodb.find", "mcp__github__read_file"] {
            assert_eq!(
                classify(None, json!({"toolName": tool_name})),
                ToolActivityAction::Other
            );
        }
        assert_eq!(classify(None, json!({})), ToolActivityAction::Other);
    }

    #[test]
    fn classifies_claude_search_tools_ahead_of_their_broad_file_read_request_kind() {
        for tool_name in ["Glob", "Grep", "LS"] {
            assert_eq!(
                classify(Some("file-read"), json!({"toolName": tool_name})),
                ToolActivityAction::Search
            );
        }
        assert_eq!(
            classify(Some("file-read"), json!({"toolName": "Read"})),
            ToolActivityAction::Read
        );
    }

    #[test]
    fn formats_read_and_search_labels_from_structured_input() {
        let search = |data: Value| format_search_tool_label(Some(&data));
        assert_eq!(format_read_tool_label("src/env.ts", 0), "Read src/env.ts");
        assert_eq!(
            format_read_tool_label("src/env.ts", 2),
            "Read src/env.ts +2 more"
        );
        assert_eq!(format_read_tool_label("", 0), "Read file");
        assert_eq!(
            search(json!({"input": {"pattern": "TODO", "path": "apps/web"}})).as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            search(json!({"input": {"glob": "*.ts", "path": "/tmp/app-new"}})).as_deref(),
            Some("Searched files *.ts in app-new")
        );
        assert_eq!(
            search(json!({"rawInput": {}, "input": {"pattern": "TODO", "path": "apps/web"}}))
                .as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            search(json!({"input": {"globPattern": "*.tsx", "path": "apps/web"}})).as_deref(),
            Some("Searched files *.tsx in web")
        );
        assert_eq!(
            search(json!({"input": {"pattern": "TODO", "glob": "*.ts", "path": "apps/web"}}))
                .as_deref(),
            Some("Searched TODO in web")
        );
    }

    #[test]
    fn keeps_bare_filenames_from_explicit_path_fields() {
        assert_eq!(
            collect_tool_file_paths(&json!({"input": {"file_path": "README"}})),
            ["README"]
        );
    }

    #[test]
    fn titles_claude_skill_calls_with_the_skill_they_load() {
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({"skill": "full-send"})).as_deref(),
            Some("Skill: full-send")
        );
        assert_eq!(
            claude_skill_invocation(
                Some("Skill"),
                &json!({"skill": "claude-api", "args": " pricing "})
            ),
            Some(SkillInvocation {
                name: "claude-api".into(),
                args: Some("pricing".into()),
            })
        );
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({"skill": " "})),
            None
        );
        assert_eq!(
            dynamic_tool_title(Some("Read"), &json!({"skill": "full-send"})),
            None
        );
    }
}
