//! A terminal's visible output attached to a thread's draft: the frozen
//! viewport's lines, the chosen range and the context record it becomes.
use serde_json::{Value, json};

/// The longest terminal text a context record carries, in UTF-16 units.
pub const TERMINAL_CONTEXT_MAX_CHARS: usize = 64_000;

/// The lines of a captured viewport; line numbers count from its top, not
/// from the terminal's scrollback.
pub fn visible_terminal_lines(output: &str) -> Vec<String> {
    output
        .trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutputSelection {
    pub text: String,
    /// The range exceeds the context limit.
    pub too_large: bool,
    pub can_attach: bool,
}

/// The text of lines `start..=end` (zero-based) and whether it can be attached.
pub fn terminal_output_selection(lines: &[String], start: u32, end: u32) -> TerminalOutputSelection {
    let (start, end) = (start as usize, end as usize);
    let text = if start <= end && end < lines.len() {
        lines[start..=end].join("\n")
    } else {
        String::new()
    };
    let too_large = text.encode_utf16().count() > TERMINAL_CONTEXT_MAX_CHARS;
    TerminalOutputSelection {
        can_attach: !text.trim().is_empty() && !too_large,
        too_large,
        text,
    }
}

/// The record of attached output; lines count from one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOutputContext {
    pub context_id: String,
    pub terminal_id: String,
    /// The terminal's name, such as "Terminal 2".
    pub terminal_label: String,
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}
impl TerminalOutputContext {
    /// "Terminal 2 · visible lines 3–7".
    pub fn label(&self) -> String {
        format!(
            "{} · visible lines {}–{}",
            self.terminal_label, self.line_start, self.line_end
        )
    }
    pub fn record(&self) -> Value {
        json!({
            "version": 1,
            "kind": "terminal",
            "contextId": self.context_id,
            "label": self.label(),
            "terminalId": self.terminal_id,
            "terminalLabel": format!("{} (visible output)", self.terminal_label),
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
            "text": self.text,
        })
    }
    /// The link the draft text carries for the record.
    pub fn reference(&self) -> String {
        crate::view::composer::chips::format_context_reference(
            "terminal",
            &self.context_id,
            &self.label(),
        )
    }
}

fn js_whitespace(c: char) -> bool {
    c == '\u{feff}' || c.is_whitespace()
}

/// Appends a context link to a draft, separated by spaces and followed by
/// one so typing continues after it.
pub fn append_context_reference(text: &str, reference: &str) -> String {
    let lead = !text.is_empty()
        && !text.ends_with(js_whitespace)
        && !reference.starts_with(js_whitespace);
    let trail = !reference.ends_with(js_whitespace);
    format!(
        "{text}{}{reference}{}",
        if lead { " " } else { "" },
        if trail { " " } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        visible_terminal_lines(text)
    }

    #[test]
    fn numbers_the_viewport_lines_without_its_trailing_blank_lines() {
        assert_eq!(lines("$ ls\na b\n\n\n"), ["$ ls", "a b"]);
        assert_eq!(lines(""), [""]);
    }

    #[test]
    fn a_range_attaches_its_lines_unless_blank_or_over_the_limit() {
        let viewport = lines("one\n\ntwo\nthree");
        let selection = terminal_output_selection(&viewport, 2, 3);
        assert_eq!(selection.text, "two\nthree");
        assert!(selection.can_attach);
        assert!(!terminal_output_selection(&viewport, 1, 1).can_attach);
        let huge = vec!["x".repeat(TERMINAL_CONTEXT_MAX_CHARS + 1)];
        let selection = terminal_output_selection(&huge, 0, 0);
        assert!(selection.too_large);
        assert!(!selection.can_attach);
    }

    #[test]
    fn the_record_names_the_terminal_and_its_visible_lines() {
        let context = TerminalOutputContext {
            context_id: "ctx-1".into(),
            terminal_id: "term-2".into(),
            terminal_label: "Terminal 2".into(),
            line_start: 3,
            line_end: 7,
            text: "out".into(),
        };
        let record = context.record();
        assert_eq!(record["label"], "Terminal 2 · visible lines 3–7");
        assert_eq!(record["terminalLabel"], "Terminal 2 (visible output)");
        assert_eq!(record["lineStart"], 3);
        assert_eq!(
            context.reference(),
            "[Terminal 2 · visible lines 3–7](context://v1/terminal/ctx-1)"
        );
    }

    #[test]
    fn appends_the_link_with_a_separating_and_a_trailing_space() {
        assert_eq!(append_context_reference("", "[a](x)"), "[a](x) ");
        assert_eq!(append_context_reference("look", "[a](x)"), "look [a](x) ");
        assert_eq!(append_context_reference("look ", "[a](x)"), "look [a](x) ");
    }
}
