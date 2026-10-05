// T3 AgentSessionScanner.test.ts, `parseAgentSessionTranscript`.
use super::*;
use serde_json::json;

const NOW: &str = "2026-08-24T12:00:00.000Z";

fn ms(value: &str) -> i64 {
    Timestamp::parse(value).unwrap().millis()
}

fn meta(source: Driver, fallback: &str, last_active: &str) -> TranscriptMeta {
    TranscriptMeta {
        source,
        instance: match source {
            Driver::Codex => "codex".into(),
            Driver::Claude => "claude".into(),
        },
        fallback_session: fallback.into(),
        last_active_ms: ms(last_active),
    }
}

fn lines(records: &[Value]) -> String {
    records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn texts(thread: &SessionThread) -> Vec<&str> {
    thread.messages.iter().map(|m| m.text.as_str()).collect()
}

pub(crate) fn record_limit_transcript(cwd: &str, overflow: bool) -> String {
    let mut records = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "record-limit-session", "cwd": cwd } }),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "First prompt" } }),
    ]);
    records.push('\n');
    records.push_str(&"{}\n".repeat(99_998));
    if overflow {
        records.push('\n');
        records.push_str(
            &json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "Overflow prompt" } })
                .to_string(),
        );
        records.push('\n');
    }
    records
}

#[test]
fn handles_the_exact_record_limit_and_an_interior_blank() {
    for overflow in [false, true] {
        let thread = parse_session_transcript(
            &meta(Driver::Codex, "unused", NOW),
            &record_limit_transcript("/project", overflow),
        );
        if overflow {
            assert_eq!(thread, None);
        } else {
            assert_eq!(texts(&thread.unwrap()), ["First prompt"]);
        }
    }
}

#[test]
fn keeps_claude_text_and_titles_while_dropping_malformed_and_tool_records() {
    let contents = [
        "not valid json".to_owned(),
        lines(&[
            json!({ "type": "ai-title", "aiTitle": "Fix authentication" }),
            json!({ "type": "user", "sessionId": "claude-session", "isMeta": true,
                    "message": { "role": "user", "content": "Injected skill instructions" } }),
            json!({ "type": "user", "sessionId": "claude-session", "isCompactSummary": true,
                    "message": { "role": "user", "content": "Injected compaction summary" } }),
            json!({ "type": "user", "sessionId": "claude-session", "timestamp": "2026-08-24T10:00:00.000Z",
                    "message": { "role": "user", "content": [{ "type": "text", "text": "Fix authentication" }] } }),
            json!({ "type": "user", "sessionId": "claude-session",
                    "message": { "role": "user", "content": [{ "type": "tool_result", "text": "hidden" }] } }),
            json!({ "type": "assistant", "sessionId": "claude-session",
                    "message": { "role": "assistant", "model": "claude-sonnet-5",
                                 "content": [{ "type": "text", "text": "Updated the login flow" }] } }),
            json!({ "type": "assistant", "sessionId": "claude-session",
                    "message": { "role": "assistant", "model": "<synthetic>",
                                 "content": [{ "type": "text", "text": "The provider request failed" }] } }),
        ]),
    ]
    .join("\n");
    let thread =
        parse_session_transcript(&meta(Driver::Claude, "fallback", NOW), &contents).unwrap();
    assert_eq!(thread.session, "claude-session");
    assert_eq!(thread.title, "Fix authentication");
    assert_eq!(thread.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(
        thread
            .messages
            .iter()
            .map(|m| (m.role, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (Role::User, "Fix authentication"),
            (Role::Assistant, "Updated the login flow"),
            (Role::Assistant, "The provider request failed"),
        ]
    );
}

#[test]
fn drops_injected_codex_instructions_while_keeping_the_visible_user_event() {
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
                "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" },
                "content": [{ "type": "input_text", "text": "<user_instructions>\nInternal setup instructions\n</user_instructions>" }] } }),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "Fix the actual bug" } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
                "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" },
                "content": [{ "type": "input_text", "text": "Fix the actual bug" }] } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Fixed" }] } }),
    ]);
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &contents).unwrap();
    assert_eq!(texts(&thread), ["Fix the actual bug", "Fixed"]);
}

fn assistant(timestamp: String, text: String) -> Value {
    json!({ "type": "response_item", "timestamp": timestamp,
            "payload": { "type": "message", "role": "assistant",
                         "content": [{ "type": "output_text", "text": text }] } })
}

#[test]
fn keeps_the_canonical_first_prompt_after_long_codex_transcripts_are_capped() {
    let canonical_prompt = "\n  Keep the canonical prompt  \n";
    let canonical_timestamp = "2026-08-24T10:01:00.000Z";
    let mut records = vec![
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        json!({ "type": "response_item", "timestamp": "2026-08-24T10:00:00.000Z",
                "payload": { "type": "message", "role": "user",
                             "content": [{ "type": "input_text", "text": "Keep the canonical prompt" }] } }),
        json!({ "type": "event_msg", "timestamp": canonical_timestamp,
                "payload": { "type": "user_message", "message": canonical_prompt } }),
    ];
    records.extend((0..200).map(|index| {
        assistant(
            format!("2026-08-24T11:{:02}:00.000Z", index % 60),
            format!("Assistant message {index}"),
        )
    }));
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &lines(&records)).unwrap();
    assert_eq!(thread.messages.len(), 200);
    assert_eq!(thread.messages[0].role, Role::User);
    assert_eq!(thread.messages[0].text, canonical_prompt);
    assert_eq!(thread.messages[0].created_at.as_str(), canonical_timestamp);
}

#[test]
fn restores_the_canonical_first_prompt_when_a_later_user_message_remains() {
    let canonical_prompt = "\n  Keep the canonical prompt  \n";
    let canonical_timestamp = "2026-08-24T10:01:00.000Z";
    let mut records = vec![
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        json!({ "type": "response_item", "timestamp": "2026-08-24T10:00:00.000Z",
                "payload": { "type": "message", "role": "user",
                             "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" },
                             "content": [{ "type": "input_text", "text": "Keep the canonical prompt" }] } }),
        json!({ "type": "event_msg", "timestamp": canonical_timestamp,
                "payload": { "type": "user_message", "message": canonical_prompt } }),
    ];
    records.extend((0..198).map(|index| {
        assistant(
            format!("2026-08-24T11:{:02}:00.000Z", index % 60),
            format!("Assistant message {index}"),
        )
    }));
    records.push(json!({ "type": "event_msg", "timestamp": "2026-08-24T11:58:30.000Z",
                         "payload": { "type": "user_message", "message": "Keep this later prompt" } }));
    records.push(assistant(
        "2026-08-24T11:59:00.000Z".into(),
        "Keep this latest response".into(),
    ));
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &lines(&records)).unwrap();
    assert_eq!(thread.messages.len(), 200);
    assert_eq!(thread.messages[0].role, Role::User);
    assert_eq!(thread.messages[0].text, canonical_prompt);
    assert_eq!(thread.messages[0].created_at.as_str(), canonical_timestamp);
    assert_eq!(
        thread
            .messages
            .iter()
            .filter(|m| m.text.trim() == canonical_prompt.trim())
            .count(),
        1
    );
    assert!(
        thread
            .messages
            .iter()
            .any(|m| m.text == "Keep this later prompt")
    );
    assert_eq!(
        thread.messages.last().unwrap().text,
        "Keep this latest response"
    );
}

fn response_user(turn: Value, text: &str) -> Value {
    json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
            "internal_chat_message_metadata_passthrough": turn,
            "content": [{ "type": "input_text", "text": text }] } })
}

#[test]
fn keeps_mixed_format_response_users_when_turn_ids_repeat_after_an_assistant() {
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        response_user(json!({ "turn_id": "turn-older" }), "Keep this older prompt"),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "Keep this newer prompt" } }),
        response_user(json!({ "turn_id": "turn-newer" }), "Keep this newer prompt"),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Ask again when needed" }] } }),
        response_user(json!({ "turn_id": "turn-newer" }), "Keep this newer prompt"),
    ]);
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &contents).unwrap();
    assert_eq!(
        texts(&thread),
        [
            "Keep this older prompt",
            "Keep this newer prompt",
            "Ask again when needed",
            "Keep this newer prompt",
        ]
    );
}

#[test]
fn preserves_response_user_text_when_codex_turn_metadata_is_ambiguous() {
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        response_user(json!(["unexpected"]), "Keep this legacy prompt"),
        response_user(
            json!({ "turn_id": "   " }),
            "Keep this prompt with a blank turn ID",
        ),
    ]);
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &contents).unwrap();
    assert_eq!(
        texts(&thread),
        [
            "Keep this legacy prompt",
            "Keep this prompt with a blank turn ID"
        ]
    );
}

#[test]
fn uses_the_first_valid_codex_session_id_when_a_fork_copies_ancestor_metadata() {
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "fork-session", "forked_from_id": "parent-session" } }),
        json!({ "type": "session_meta", "payload": { "id": "parent-session" } }),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "Continue in the fork" } }),
    ]);
    let thread =
        parse_session_transcript(&meta(Driver::Codex, "fallback", NOW), &contents).unwrap();
    assert_eq!(thread.session, "fork-session");
}

#[test]
fn skips_codex_transcripts_without_a_resumable_session_id() {
    let contents = json!({ "type": "event_msg",
        "payload": { "type": "user_message", "message": "This transcript has no session metadata" } })
    .to_string();
    let meta = meta(
        Driver::Codex,
        "rollout-2026-08-24T12-00-00-not-a-session-id",
        NOW,
    );
    assert_eq!(parse_session_transcript(&meta, &contents), None);
}

#[test]
fn uses_the_canonical_codex_event_when_its_turn_has_generated_response_context() {
    let turn = || json!({ "turn_id": "turn-1" });
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        response_user(
            turn(),
            "<environment_context>\n<cwd>/tmp/project</cwd>\n<shell>zsh</shell>\n</environment_context>",
        ),
        response_user(
            turn(),
            "# AGENTS.md instructions for /tmp/project\n\n<INSTRUCTIONS>\nPrivate project rules\n</INSTRUCTIONS>",
        ),
        json!({ "type": "event_msg", "payload": { "type": "user_message",
                "message": "Do something here so it looks like a real project." } }),
        response_user(turn(), "Do something here so it looks like a real project."),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Created the project." }] } }),
    ]);
    let thread = parse_session_transcript(
        &meta(Driver::Codex, "fallback", "2026-08-25T08:00:00.000Z"),
        &contents,
    )
    .unwrap();
    assert_eq!(
        thread.title,
        "Do something here so it looks like a real project."
    );
    assert_eq!(
        texts(&thread),
        [
            "Do something here so it looks like a real project.",
            "Created the project."
        ]
    );
}

#[test]
fn preserves_context_markup_in_response_only_codex_messages() {
    let context = "<environment_context>\n<cwd>/tmp/project</cwd>\n</environment_context>";
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": context }] } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "Initialize Git and add a README." }] } }),
    ]);
    let thread = parse_session_transcript(
        &meta(Driver::Codex, "fallback", "2026-08-25T08:00:00.000Z"),
        &contents,
    )
    .unwrap();
    assert_eq!(thread.title, "<environment_context>");
    assert_eq!(
        texts(&thread),
        [context, "Initialize Git and add a README."]
    );
}

fn single_event(prompt: &str) -> SessionThread {
    let contents = lines(&[
        json!({ "type": "session_meta", "payload": { "id": "codex-session" } }),
        json!({ "type": "event_msg", "payload": { "type": "user_message", "message": prompt } }),
    ]);
    parse_session_transcript(
        &meta(Driver::Codex, "fallback", "2026-08-25T08:00:00.000Z"),
        &contents,
    )
    .unwrap()
}

#[test]
fn preserves_a_canonical_codex_event_that_starts_with_context_markup() {
    let prompt = "<environment_context>\n<cwd>/tmp/project</cwd>\n</environment_context>\n\nCreate a useful project.";
    let thread = single_event(prompt);
    assert_eq!(thread.title, "<environment_context>");
    assert_eq!(texts(&thread), [prompt]);
}

#[test]
fn preserves_a_codex_request_heading_in_a_canonical_event() {
    let prompt = "\n  ## My request for Codex:\n\nFix the visible bug";
    let thread = single_event(prompt);
    assert_eq!(thread.title, "## My request for Codex:");
    assert_eq!(texts(&thread), [prompt]);
}

#[test]
fn keeps_context_markup_quoted_inside_visible_codex_user_text() {
    let quoted = "Do not remove this example:\n<environment_context>\n<cwd>/tmp/example</cwd>\n</environment_context>";
    assert_eq!(texts(&single_event(quoted)), [quoted]);
}

#[test]
fn skips_sessions_without_a_visible_user_message() {
    let contents =
        json!({ "type": "assistant", "message": { "role": "assistant", "content": "Done" } })
            .to_string();
    assert_eq!(
        parse_session_transcript(&meta(Driver::Claude, "claude-session", NOW), &contents),
        None
    );
}

#[test]
fn keeps_the_first_prompt_when_later_assistant_output_exceeds_the_message_limit() {
    let mut records = vec![json!({ "type": "user", "sessionId": "claude-session",
                                   "message": { "role": "user", "content": "Keep this prompt" } })];
    records.extend((0..250).map(|index| {
        json!({ "type": "assistant",
                "message": { "role": "assistant", "content": format!("Assistant update {index}") } })
    }));
    let thread =
        parse_session_transcript(&meta(Driver::Claude, "fallback", NOW), &lines(&records)).unwrap();
    assert_eq!(thread.messages.len(), 200);
    assert_eq!(thread.messages[0].text, "Keep this prompt");
    assert_eq!(thread.messages.last().unwrap().text, "Assistant update 249");
}
