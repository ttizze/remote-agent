use crate::*;
use serde_json::json;
#[test]
fn sdk_version_matches_the_reference_lock() {
    assert_eq!(CLAUDE_SDK_VERSION, "0.3.276");
}
#[test]
fn resume_dialog_control_has_the_original_question_choices_and_result() {
    use serde_json::json;
    let mut control = ClaudeControl::default();
    let output = control.receive(&json!({"type":"control_request","request_id":"dialog-resume-1","request":{"subtype":"request_user_dialog","dialog_kind":"resume_return","payload":{"sessionAgeMinutes":90,"estimatedTokens":120000}}})).unwrap().unwrap();
    let ProviderEvent::RequestOpened {
        body: RequestBody::Questions { questions },
        ..
    } = &output.events[0]
    else {
        panic!()
    };
    assert_eq!(
        questions[0].question,
        "This session is 1h 30m old and uses 120,000 tokens. Compact it before continuing?"
    );
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Compact and continue",
            "Keep full history",
            "Don't ask again"
        ]
    );
    let answers = Answers::from([(
        questions[0].id.clone(),
        Answer::Choices(vec!["Compact and continue".into()]),
    )]);
    assert_eq!(
        control
            .respond("dialog-resume-1", None, Some(&answers))
            .unwrap()["response"]["response"],
        json!({"behavior":"completed","result":"compact"})
    );
}
#[test]
fn absolute_rollback_requires_the_recorded_native_head() {
    use serde_json::json;
    let turns = vec![
        json!({"id":"one"}),
        json!({"id":"two"}),
        json!({"id":"three"}),
    ];
    assert_eq!(absolute_revert_count(&turns, Some("two")).unwrap(), 1);
    assert_eq!(absolute_revert_count(&turns, Some("three")).unwrap(), 0);
    assert_eq!(absolute_revert_count(&turns, None).unwrap(), 3);
    assert_eq!(
        absolute_revert_count(&turns, Some("missing")),
        Err(ProtocolError::MissingBoundary("missing".into()))
    );
}
#[tokio::test]
async fn json_lines_continue_after_invalid_frames_and_preserve_large_unicode_output() {
    use serde_json::json;
    let text = "日本語\n".repeat(100000);
    let mut input = Vec::new();
    write_frame(&mut input, &json!({"text":text}))
        .await
        .unwrap();
    let bytes = [b"\nnot json\n".as_slice(), input.as_slice()].concat();
    let mut reader = tokio::io::BufReader::new(bytes.as_slice());
    assert!(read_frame(&mut reader).await.is_err());
    assert_eq!(
        read_frame(&mut reader).await.unwrap().unwrap()["text"],
        text
    );
    assert!(read_frame(&mut reader).await.unwrap().is_none());
}

fn codex_start() -> ProviderCommand {
    ProviderCommand::Start {
        selection: ModelSelection {
            instance: "codex".into(),
            driver: Driver::Codex,
            model: "gpt-5.4".into(),
            options: std::collections::BTreeMap::new(),
        },
        runtime_mode: RuntimeMode::Auto,
        interaction_mode: InteractionMode::Default,
        text: "hello".into(),
        attachments: vec![],
        native_thread: None,
        resume_at: None,
        context: None,
    }
}
fn wire_context() -> WireContext {
    WireContext {
        cwd: "/workspace".into(),
        client_name: "agent-client".into(),
        client_version: "1".into(),
    }
}
#[test]
fn codex_stop_before_thread_ready_cancels_prompt_and_the_next_prompt_can_start() {
    use serde_json::json;
    let mut protocol = CodexProtocol::default();
    let start = protocol.command(&codex_start(), &wire_context()).unwrap();
    assert_eq!(start[0]["method"], "thread/start");
    assert!(
        protocol
            .command(
                &ProviderCommand::Interrupt {
                    native_thread: None,
                    native_turn: None
                },
                &wire_context()
            )
            .unwrap()
            .is_empty()
    );
    let ready = protocol
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"root"}}}))
        .unwrap();
    assert!(ready.outbound.is_empty());
    let mut next_command = codex_start();
    if let ProviderCommand::Start { native_thread, .. } = &mut next_command {
        *native_thread = Some("root".into());
    }
    let next = protocol.command(&next_command, &wire_context()).unwrap();
    assert_eq!(next[0]["method"], "turn/start");
    assert_eq!(next[0]["params"]["approvalsReviewer"], "auto_review");
    assert_eq!(next[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(
        next[0]["params"]["input"],
        json!([{"type":"text","text":"hello"}])
    );
}
#[test]
fn codex_stop_before_turn_ready_interrupts_once_and_terminates_native_processes() {
    use serde_json::json;
    let mut protocol = CodexProtocol::default();
    let start = protocol.command(&codex_start(), &wire_context()).unwrap();
    let ready = protocol
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"root"}}}))
        .unwrap();
    assert!(
        protocol
            .command(
                &ProviderCommand::Interrupt {
                    native_thread: Some("root".into()),
                    native_turn: None
                },
                &wire_context()
            )
            .unwrap()
            .is_empty()
    );
    let started = protocol.receive(&json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"turn-root"}}})).unwrap();
    assert_eq!(started.outbound[0]["method"], "turn/interrupt");
    let reply = protocol
        .receive(&json!({"id":ready.outbound[0]["id"],"result":{"turn":{"id":"turn-root"}}}))
        .unwrap();
    assert!(reply.outbound.is_empty());
    protocol.receive(&json!({"method":"item/started","params":{"threadId":"root","item":{"id":"tool","type":"commandExecution","command":"sleep 30","processId":"123"}}})).unwrap();
    let stop = protocol
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("root".into()),
                native_turn: Some("turn-root".into()),
            },
            &wire_context(),
        )
        .unwrap();
    assert_eq!(
        stop.iter()
            .map(|f| f["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["turn/interrupt", "thread/backgroundTerminals/terminate"]
    );
    assert_eq!(
        stop[1]["params"],
        json!({"threadId":"root","processId":"123"})
    );
    let child = protocol
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("child".into()),
                native_turn: Some("turn-child".into()),
            },
            &wire_context(),
        )
        .unwrap();
    assert_eq!(child.len(), 1);
    assert_eq!(
        child[0]["params"],
        json!({"threadId":"child","turnId":"turn-child"})
    );
}

#[test]
fn initialize_recovers_pending_requests_once_and_preserves_reply_correlation() {
    use serde_json::json;
    let mut control = ClaudeControl::default();
    let init = control.initialize();
    let permission = json!({"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"tool-1"}});
    let dialog = json!({"type":"control_request","request_id":"dialog-1","request":{"subtype":"request_user_dialog","dialog_kind":"resume_return","payload":{"sessionAgeMinutes":20,"estimatedTokens":2000}}});
    let output = control.receive(&json!({"type":"control_response","response":{"subtype":"success","request_id":init["request_id"],"response":{"pending_permission_requests":[permission.clone()],"pending_user_dialog_requests":[dialog.clone()]}}})).unwrap().unwrap();
    assert_eq!(output.events.len(), 2);
    assert_eq!(output.replies[0].operation, "initialize");
    assert!(
        control
            .receive(&permission)
            .unwrap()
            .unwrap()
            .events
            .is_empty()
    );
    assert!(control.receive(&dialog).unwrap().unwrap().events.is_empty());
    let unknown = control.receive(&json!({"type":"control_request","request_id":"unknown-dialog","request":{"subtype":"request_user_dialog","dialog_kind":"future_dialog"}})).unwrap().unwrap();
    assert!(unknown.events.is_empty());
    assert!(unknown.outbound.is_empty());
}

#[test]
fn native_history_injection_preserves_roles_and_only_explicit_unsupported_uses_inline() {
    let history = select_history(
        &[HistoricalMessage {
            role: Role::Assistant,
            text: "日本語\n  Keep indentation.".into(),
            thread: "source".into(),
            run: Some("run".into()),
            item: "item".into(),
            provider_thread: Some("source-native".into()),
            status: "completed".into(),
            kind: "assistant_message".into(),
            run_status: None,
        }],
        "Recover source history",
        0,
        16_000,
    );
    for error in [None, Some(-32601), Some(-32000)] {
        let mut protocol = CodexProtocol::default();
        let mut command = codex_start();
        if let ProviderCommand::Start { context, .. } = &mut command {
            *context = Some(history.clone());
        }
        let start = protocol.command(&command, &wire_context()).unwrap();
        let ready = protocol
            .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
            .unwrap();
        let inject = &ready.outbound[0];
        assert_eq!(inject["method"], "thread/inject_items");
        assert_eq!(inject["params"]["items"][0]["role"], "user");
        assert_eq!(inject["params"]["items"][1]["role"], "assistant");
        assert!(
            inject["params"]["items"][1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("日本語\n  Keep indentation.")
        );
        let response = match error {
            Some(code) => {
                json!({"id":inject["id"],"error":{"code":code,"message":"unsupported or lost"}})
            }
            None => json!({"id":inject["id"],"result":{}}),
        };
        let result = protocol.receive(&response);
        if error == Some(-32000) {
            assert!(
                matches!(result,Err(ProtocolError::Remote {operation,..}) if operation=="thread/inject_items")
            );
            continue;
        }
        let result = result.unwrap();
        assert_eq!(result.outbound[0]["method"], "turn/start");
        let text = result.outbound[0]["params"]["input"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text.contains("Keep indentation."), error.is_some());
        assert_eq!(
            result.events.contains(&ProviderEvent::ContextInjected),
            error.is_none()
        );
    }
}

#[test]
fn assistant_context_usage_includes_cache_reads_and_creation_in_the_reference_window() {
    let mut protocol = ClaudeProtocol::default();
    let mut command = codex_start();
    if let ProviderCommand::Start { selection, .. } = &mut command {
        selection.driver = Driver::Claude;
        selection.model = "claude-sonnet-4-6".into();
    }
    protocol.command(&command, "prompt", &[]).unwrap();
    let output=protocol.receive(&json!({"type":"assistant","uuid":"assistant","message":{"id":"message","model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":42_000,"cache_creation_input_tokens":2000,"cache_read_input_tokens":5000,"output_tokens":1000}}})).unwrap();
    let events = match &output.events[0] {
        ProviderEvent::NativeOutput { events, .. } => events,
        _ => &output.events,
    };
    assert!(events.contains(&ProviderEvent::ContextUsage(ContextUsage {
        used_tokens: 50_000,
        max_tokens: Some(200_000),
        auto_compact_threshold: None
    })));
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderEvent::Usage(TokenUsage {
            input: 49_000,
            cached_input: 5000,
            output: 1000,
            total: 50_000,
            max: Some(200_000),
            ..
        })
    )));
}

#[test]
fn mcp_metadata_trims_names_limits_utf16_and_accepts_only_web_icons() {
    for (title, server, icon, expected_title, expected_icon) in [
        (
            "  Pull issue  ".to_string(),
            " GitHub ".to_string(),
            "https://example.com".to_string(),
            Some("Pull issue"),
            Some("https://example.com/"),
        ),
        (
            "Tool".into(),
            "Server".into(),
            "file:///tmp/icon".into(),
            Some("Tool"),
            None,
        ),
        (
            "x".repeat(161),
            "Server".into(),
            "https://example.com".into(),
            None,
            None,
        ),
        (
            "🧪".repeat(81),
            "Server".into(),
            "https://example.com".into(),
            None,
            None,
        ),
        (
            "Tool".into(),
            " ".into(),
            "https://example.com".into(),
            Some("Tool"),
            None,
        ),
    ] {
        let mut protocol = ClaudeProtocol::default();
        let output=protocol.receive(&json!({"type":"assistant","uuid":"assistant","tool_use_meta":[{"id":" tool ","display_name":title,"server_display_name":server,"icon_url":icon}],"message":{"id":"message","content":[{"type":"tool_use","id":"tool","name":"mcp__server__tool","input":{}}]}})).unwrap();
        let events = match &output.events[0] {
            ProviderEvent::NativeOutput { events, .. } => events,
            _ => &output.events,
        };
        let presentation = events
            .iter()
            .find_map(|event| match event {
                ProviderEvent::ItemStarted {
                    kind: ProviderItem::Tool { presentation, .. },
                    ..
                } => Some(presentation),
                _ => None,
            })
            .unwrap();
        assert_eq!(presentation.title.as_deref(), expected_title);
        assert_eq!(
            presentation
                .source
                .as_ref()
                .and_then(|source| source.0["icon"]["logoUrl"].as_str()),
            expected_icon
        );
        if title.starts_with("  ") {
            assert_eq!(presentation.source.as_ref().unwrap().0["name"], "GitHub");
        }
    }
}
