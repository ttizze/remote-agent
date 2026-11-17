use super::*;

#[test]
fn workspace_save_requires_both_a_connection_and_an_actual_workspace() {
    assert!(plan_write_enabled(true, "/workspace".into()));
    assert!(!plan_write_enabled(false, "/workspace".into()));
    assert!(!plan_write_enabled(true, String::new()));
    assert!(!plan_write_enabled(false, String::new()));
}

#[test]
fn titles_and_body_boundaries_match_the_pinned_ecmascript_helpers() {
    for (markdown, title, body) in [
        ("#No space", None, "#No space"),
        ("    # Indented", None, "    # Indented"),
        ("####### Deep", None, "####### Deep"),
        ("# A\n## Summary\ntext", Some("A"), "text"),
        ("# A\r\n## Summary\r\ntext", Some("A"), "text"),
        (
            "# A\n## Summary\u{2028}\ntext",
            Some("A"),
            "## Summary\u{2028}\ntext",
        ),
        ("#\nTitle", Some("Title"), "#\nTitle"),
        ("\u{85}# Wrong", None, "\u{85}# Wrong"),
        ("\u{feff}# Good", Some("Good"), ""),
    ] {
        assert_eq!(plan_title(markdown).as_deref(), title, "{markdown:?}");
        assert_eq!(plan_display(markdown.into()).body, body, "{markdown:?}");
    }
}

#[test]
fn collapsing_uses_utf16_length_and_visible_preview_lines() {
    assert!(plan_display("😀".repeat(450)).preview.is_none());
    assert!(plan_display("😀".repeat(451)).preview.is_some());
    assert!(plan_display("line\n".repeat(19)).preview.is_none());
    let long =
        plan_display("# A\n\n1\n\n2\n\n3\n\n4\n\n5\n\n6\n\n7\n\n8\n\n9\n\n10\n\n11\n".into());
    assert_eq!(
        long.preview.as_deref(),
        Some("1\n\n2\n\n3\n\n4\n\n5\n\n6\n\n7\n\n8\n\n9\n\n10\n\n...")
    );
    assert_eq!(
        long.contents,
        "# A\n\n1\n\n2\n\n3\n\n4\n\n5\n\n6\n\n7\n\n8\n\n9\n\n10\n\n11\n"
    );
}

#[test]
fn exports_use_the_same_title_and_ecmascript_trailing_space_rules() {
    for markdown in ["#No space", "    # Indented", "####### Deep"] {
        assert_eq!(plan_export(markdown).0, "plan.md");
    }
    assert_eq!(plan_export("# ship.it").0, "shipit.md");
    assert_eq!(plan_export("# A  /  B").0, "a-b.md");
    assert_eq!(
        plan_export("# Ship (it)!\n\n- one\n\n"),
        ("ship-it.md".into(), "# Ship (it)!\n\n- one\n".into())
    );
    assert_eq!(
        plan_export("# 計画\n本文"),
        ("plan.md".into(), "# 計画\n本文\n".into())
    );
    assert_eq!(
        plan_export("no heading"),
        ("plan.md".into(), "no heading\n".into())
    );
    assert_eq!(
        plan_export("text\u{85}"),
        ("plan.md".into(), "text\u{85}\n".into())
    );
    assert_eq!(
        plan_export("text\u{feff}"),
        ("plan.md".into(), "text\n".into())
    );
}
