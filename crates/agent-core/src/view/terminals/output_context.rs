//! Captured terminal output attached to a conversation.
/// The most text, in UTF-16 units, one terminal context record carries.
pub const TERMINAL_CONTEXT_TEXT_MAX_CHARS: usize = 64_000;

/// A terminal's visible output as the attach sheet lists it: trailing
/// newlines dropped, one entry per line.
pub fn visible_output_lines(text: &str) -> Vec<String> {
    text.trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// The lines chosen in the attach sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct VisibleOutputSelection {
    pub text: String,
    /// Over one record's limit: the sheet asks for fewer lines.
    pub too_large: bool,
    pub can_attach: bool,
}

/// Lines `start` through `end` (0-based, inclusive) of the visible output.
pub fn visible_output_selection(lines: &[String], start: u32, end: u32) -> VisibleOutputSelection {
    let (start, end) = (start as usize, end as usize);
    let text = lines
        .get(start..=end.min(lines.len().saturating_sub(1)))
        .unwrap_or_default()
        .join("\n");
    let too_large = crate::js_text::utf16_len(&text) > TERMINAL_CONTEXT_TEXT_MAX_CHARS;
    VisibleOutputSelection {
        can_attach: !crate::js_text::js_trim(&text).is_empty() && !too_large,
        text,
        too_large,
    }
}

/// Visible terminal lines attached to the composer as context.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutputContext {
    pub terminal_id: String,
    pub terminal_label: String,
    /// 1-based, relative to the frozen viewport rather than the scrollback.
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

impl TerminalOutputContext {
    /// The message context record and the composer link to it.
    pub fn record(&self, context_id: &str) -> (serde_json::Value, String) {
        let label = format!(
            "{} · visible lines {}–{}",
            self.terminal_label, self.line_start, self.line_end
        );
        let reference =
            crate::view::composer::chips::format_context_reference("terminal", context_id, &label);
        let record = serde_json::json!({
            "version": 1,
            "kind": "terminal",
            "contextId": context_id,
            "label": label,
            "terminalId": self.terminal_id,
            "terminalLabel": format!("{} (visible output)", self.terminal_label),
            "lineStart": self.line_start,
            "lineEnd": self.line_end,
            "text": self.text,
        });
        (record, reference)
    }
}

/// `text` with `reference` added at its end, spaced from the text before it
/// and followed by a space for typing on.
pub fn append_context_reference(text: &str, reference: &str) -> String {
    let lead = if !text.is_empty()
        && !text.ends_with(crate::js_text::is_js_space)
        && !reference.starts_with(crate::js_text::is_js_space)
    {
        " "
    } else {
        ""
    };
    let trail = if reference.ends_with(crate::js_text::is_js_space) {
        ""
    } else {
        " "
    };
    format!("{text}{lead}{reference}{trail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lines(text: &str) -> Vec<String> {
        visible_output_lines(text)
    }

    #[test]
    fn the_attach_sheet_lists_visible_lines_without_trailing_newlines() {
        assert_eq!(lines("a\n\nb\n\n\n"), ["a", "", "b"]);
        assert_eq!(lines(""), [""]);
    }

    #[test]
    fn a_selection_attaches_its_lines_unless_blank_or_over_the_limit() {
        let output = lines("$ cargo test\nok\n   \n");
        let chosen = visible_output_selection(&output, 0, 1);
        assert_eq!(chosen.text, "$ cargo test\nok");
        assert!(chosen.can_attach && !chosen.too_large);
        assert!(!visible_output_selection(&output, 2, 2).can_attach);
        let huge = vec!["x".repeat(TERMINAL_CONTEXT_TEXT_MAX_CHARS + 1)];
        let over = visible_output_selection(&huge, 0, 0);
        assert!(over.too_large && !over.can_attach);
    }

    #[test]
    fn attached_output_names_its_terminal_and_visible_line_range() {
        let context = TerminalOutputContext {
            terminal_id: "term-1".into(),
            terminal_label: "Terminal 1".into(),
            line_start: 3,
            line_end: 7,
            text: "error: boom".into(),
        };
        let (record, reference) = context.record("ctx-1");
        assert_eq!(
            reference,
            "[Terminal 1 · visible lines 3–7](context://v1/terminal/ctx-1)"
        );
        assert_eq!(record["terminalLabel"], "Terminal 1 (visible output)");
        assert_eq!(record["label"], "Terminal 1 · visible lines 3–7");
        assert_eq!(
            (record["lineStart"].as_u64(), record["lineEnd"].as_u64()),
            (Some(3), Some(7))
        );
        let context = agent_domain::MessageContext {
            version: 1,
            records: vec![agent_domain::Json(record)],
        };
        assert_eq!(
            context.normalized().map(|valid| valid.records.len()),
            Some(1)
        );
    }

    #[test]
    fn appended_references_are_spaced_from_the_draft() {
        assert_eq!(append_context_reference("", "[t](x)"), "[t](x) ");
        assert_eq!(append_context_reference("look", "[t](x)"), "look [t](x) ");
        assert_eq!(append_context_reference("look ", "[t](x)"), "look [t](x) ");
    }
}
