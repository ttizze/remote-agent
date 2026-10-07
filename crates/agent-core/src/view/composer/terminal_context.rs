//! Terminal output the user adds to the composer: the selection a terminal
//! reports, its label and the context record it becomes.
use super::chips::{format_context_reference, kind_scoped_context_id};
use agent_domain::sanitize_context_label;
use serde_json::{Value, json};

/// Lines selected in a thread terminal. Lines count from 1 at the oldest
/// line the terminal keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalContextSelection {
    pub terminal_id: String,
    pub terminal_label: String,
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

/// Unix line endings, without blank lines around the selection.
pub fn normalize_terminal_context_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_matches('\n').to_owned()
}

/// `Terminal 1 line 4` or `Terminal 1 lines 12-13`.
pub fn format_terminal_context_label(
    terminal_label: &str,
    line_start: u32,
    line_end: u32,
) -> String {
    if line_start == line_end {
        format!("{terminal_label} line {line_start}")
    } else {
        format!("{terminal_label} lines {line_start}-{line_end}")
    }
}

/// The selection with its text normalized and its range ordered; `None`
/// when it has no text or no terminal.
pub fn normalize_terminal_context_selection(
    selection: &TerminalContextSelection,
) -> Option<TerminalContextSelection> {
    let text = normalize_terminal_context_text(&selection.text);
    let terminal_id = selection.terminal_id.trim();
    let terminal_label = selection.terminal_label.trim();
    if text.is_empty() || terminal_id.is_empty() || terminal_label.is_empty() {
        return None;
    }
    let line_start = selection.line_start.max(1);
    Some(TerminalContextSelection {
        terminal_id: terminal_id.into(),
        terminal_label: terminal_label.into(),
        line_start,
        line_end: selection.line_end.max(line_start),
        text,
    })
}

/// The context record of a normalized selection, under the producer id `id`.
pub fn terminal_context_record(id: &str, selection: &TerminalContextSelection) -> Value {
    json!({
        "version": 1,
        "contextId": kind_scoped_context_id("terminal", id),
        "kind": "terminal",
        "label": sanitize_context_label(
            &format_terminal_context_label(
                &selection.terminal_label,
                selection.line_start,
                selection.line_end,
            ),
            "terminal",
        ),
        "terminalId": selection.terminal_id,
        "terminalLabel": selection.terminal_label,
        "lineStart": selection.line_start,
        "lineEnd": selection.line_end,
        "text": normalize_terminal_context_text(&selection.text),
    })
}

/// The inline link that stands for a terminal record in the prompt.
pub fn terminal_context_reference(record: &Value) -> String {
    let field = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or_default();
    format_context_reference("terminal", field("contextId"), field("label"))
}

/// Whether `record` holds the same lines of the same terminal; the draft keeps
/// one record per range.
pub fn is_same_terminal_range(record: &Value, selection: &TerminalContextSelection) -> bool {
    record.get("kind").and_then(Value::as_str) == Some("terminal")
        && record.get("terminalId").and_then(Value::as_str) == Some(&selection.terminal_id)
        && record.get("lineStart").and_then(Value::as_u64) == Some(selection.line_start.into())
        && record.get("lineEnd").and_then(Value::as_u64) == Some(selection.line_end.into())
}

#[cfg(test)]
mod tests;
