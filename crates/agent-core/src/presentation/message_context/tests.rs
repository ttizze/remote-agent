use super::*;
use serde_json::json;

fn context(value: serde_json::Value) -> MessageContext {
    serde_json::from_value(value).unwrap()
}

#[test]
fn reference_identity_and_literal_label_cannot_escape_their_markdown_link() {
    let value = context(
        json!({"id":"a/b)#日本語", "label":"[raw] <b>", "content":{"file":{"path":"/repo/raw.rs"}}}),
    );
    let reference = context_reference(&value);
    assert_eq!(
        context_href(&value),
        "bex-context://v1/file/a%2Fb%29%23%E6%97%A5%E6%9C%AC%E8%AA%9E"
    );
    assert!(reference.starts_with("[\\[raw\\] \\<b\\>]"));
    let chip = context_chips(vec![value]).remove(0);
    assert_eq!(chip.label, "[raw] <b>");
    assert!(
        matches!(chip.action.unwrap().target, ContextTarget::File { path } if path == "/repo/raw.rs")
    );
}

#[test]
fn thread_targets_keep_the_recorded_namespace_and_unresolved_context_has_no_action() {
    let values = [
        context(
            json!({"id":"thread", "label":"Related", "content":{"thread":{"host":"other-host", "storageScope":"old-account", "session":{"provider":"codex","id":"same-id"}, "title":"Related"}}}),
        ),
        context(json!({"id":"unknown", "label":"Missing", "content":{"summary":{"kind":"file"}}})),
        context(
            json!({"id":"machine", "label":"Machine", "content":{"environment":{"host":"other-host"}}}),
        ),
    ];
    let chips = context_chips(values.to_vec());
    assert!(
        matches!(chips[0].action.as_ref().unwrap().target, ContextTarget::Thread { ref host, ref storage, ref session }
        if host == "other-host" && storage == "old-account" && session.id == "same-id")
    );
    assert_eq!(chips[1].kind, "unavailable");
    assert!(chips[1].action.is_none());
    assert!(
        chips[2].action.is_none(),
        "the source machine chip is informational"
    );
}

#[test]
fn pull_request_actions_only_use_the_existing_http_browser_contract() {
    for (url, allowed) in [
        ("https://example.test/repo/pull/7", true),
        ("javascript:alert(1)", false),
        ("file:///tmp/no", false),
    ] {
        let value = context(
            json!({"id":"pr", "label":"PR", "content":{"pullRequest":{"pullRequest":{
                "number":7, "title":"Fix", "url":url, "headBranch":"feature", "baseBranch":"main", "state":"open", "draft":false
            }}}}),
        );
        let chip = context_chips(vec![value]).remove(0);
        assert_eq!(chip.action.is_some(), allowed, "{url}");
    }
}

#[test]
fn citation_comment_is_prose_while_the_source_card_is_excluded_from_find() {
    let value = context(
        json!({"id":"quote", "label":"Source", "content":{"citation":{"citation":{
            "source":{"host":"other", "storageScope":"account", "session":{"provider":"codex", "id":"thread"}, "turnId":"turn", "itemId":"answer"},
            "range":{"start":2,"end":7}, "text":"raw <quote>", "prefix":"a", "suffix":"b", "comment":"Please explain **this**"
        }}}}),
    );
    let text = context_reference(&value);
    assert!(text.ends_with("\n\nPlease explain \\*\\*this\\*\\*"));
    let chip = context_chips(vec![value]).remove(0);
    assert!(!chip.searchable);
    let ContextTarget::Citation { citation } = chip.action.unwrap().target else {
        panic!()
    };
    assert_eq!(citation.text, "raw <quote>");
}

#[test]
fn citation_preview_collapses_js_spaces_without_changing_the_saved_quote_or_source() {
    let value = context(
        json!({"id":"quote", "label":"Source", "content":{"citation":{"citation":{
            "source":{"host":"host", "storageScope":"account", "session":{"provider":"codex", "id":"thread"}, "turnId":"turn", "itemId":"answer"},
            "range":{"start":0,"end":8}, "text":"\u{0085}\u{00a0}chosen\u{2028}",
            "prefix":"\u{feff} before \n", "suffix":"\tafter \u{0085}\u{3000}", "comment":"Discuss **exactly**"
        }}}}),
    );
    let chip = context_chips(vec![value]).remove(0);
    let preview = chip.citation_preview.unwrap();
    assert_eq!(preview.prefix, "… before ");
    assert_eq!(preview.text, "\u{0085} chosen ");
    assert_eq!(preview.suffix, " after \u{0085} …");
    assert!(preview.highlighted);
    let ContextTarget::Citation { citation } = chip.action.unwrap().target else {
        panic!()
    };
    assert_eq!(citation.text, "\u{0085}\u{00a0}chosen\u{2028}");
    assert_eq!(citation.prefix, "\u{feff} before \n");
    assert_eq!(citation.suffix, "\tafter \u{0085}\u{3000}");
    assert_eq!(citation.comment, "Discuss **exactly**");
    assert_eq!(
        citation.range,
        agent_protocol::context::TextRange { start: 0, end: 8 }
    );
}

#[test]
fn only_partial_response_previews_have_a_mark_and_context_ellipses() {
    for (prefix, suffix, marked) in [("", "", false), ("a", "", true), ("", "b", true)] {
        let preview = CitationPreview::new(" whole\nresponse ", prefix, suffix);
        assert_eq!(preview.text, " whole response ");
        assert_eq!(preview.highlighted, marked);
        assert_eq!(preview.prefix, if prefix.is_empty() { "" } else { "…a" });
        assert_eq!(preview.suffix, if suffix.is_empty() { "" } else { "b…" });
    }
}

#[test]
fn terminal_preview_retains_exact_bytes_including_indentation_and_final_newline() {
    let value = context(
        json!({"id":"terminal", "label":"Build", "content":{"terminal":{
            "terminalId":"shell", "terminalLabel":"Shell", "lineStart":8,"lineEnd":8,"text":"<b>日本語</b>\n  indent\n"
        }}}),
    );
    let chip = context_chips(vec![value]).remove(0);
    assert_eq!(chip.sections[1].text, "Line 8");
    assert_eq!(chip.sections[2].text, "<b>日本語</b>\n  indent\n");
    assert!(chip.action.is_none());
}

#[test]
fn element_details_show_the_captured_page_selector_and_source_location_without_inventing_empty_code()
 {
    let value = context(
        json!({"id":"element", "label":"Save", "content":{"element":{"element":{
            "pageUrl":"https://example.test/settings", "pageTitle":"  Settings  ", "tagName":"button", "selector":"#save",
            "html":"<button>Save</button>", "componentName":null, "styles":"   ",
            "source":{"functionName":null, "fileName":"src/settings.tsx", "line":17, "column":3}
        }}}}),
    );
    let chip = context_chips(vec![value]).remove(0);
    assert_eq!(
        chip.sections
            .iter()
            .map(|section| section.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Settings",
            "https://example.test/settings",
            "#save",
            "src/settings.tsx:17",
            "<button>Save</button>"
        ]
    );
    assert!(
        chip.action.is_none(),
        "captured page/source text grants no resource action"
    );
}
