use super::*;
use agent_domain::context_references;

fn selection() -> TerminalContextSelection {
    TerminalContextSelection {
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 12,
        line_end: 13,
        text: "git status\nOn branch main".into(),
    }
}

#[test]
fn folds_producer_ids_consistently_in_records_and_references() {
    let record = terminal_context_record("old terminal:one", &selection());
    let reference = &context_references(&terminal_context_reference(&record))[0];
    assert_eq!(reference.context_id, record["contextId"].as_str().unwrap());
}

#[test]
fn formats_terminal_labels_with_line_ranges() {
    assert_eq!(
        format_terminal_context_label("Terminal 1", 12, 13),
        "Terminal 1 lines 12-13"
    );
    assert_eq!(
        format_terminal_context_label("Terminal 1", 9, 9),
        "Terminal 1 line 9"
    );
}

#[test]
fn formats_a_terminal_context_as_a_canonical_reference_link() {
    let record = terminal_context_record("context-1", &selection());
    assert_eq!(
        terminal_context_reference(&record),
        "[Terminal 1 lines 12-13](context://v1/terminal/terminal_context-1)"
    );
}

#[test]
fn a_selection_without_text_adds_nothing() {
    let blank = TerminalContextSelection {
        text: "\r\n\n".into(),
        ..selection()
    };
    assert_eq!(normalize_terminal_context_selection(&blank), None);
    let normalized = normalize_terminal_context_selection(&TerminalContextSelection {
        terminal_label: " Terminal 1 ".into(),
        line_start: 0,
        line_end: 0,
        text: "\nls\r\nREADME.md\n".into(),
        ..selection()
    })
    .unwrap();
    assert_eq!(normalized.text, "ls\nREADME.md");
    assert_eq!(normalized.terminal_label, "Terminal 1");
    assert_eq!((normalized.line_start, normalized.line_end), (1, 1));
}

#[test]
fn a_record_names_the_same_range_once() {
    let record = terminal_context_record("context-1", &selection());
    assert!(is_same_terminal_range(&record, &selection()));
    assert!(!is_same_terminal_range(
        &record,
        &TerminalContextSelection {
            line_end: 14,
            ..selection()
        }
    ));
}
