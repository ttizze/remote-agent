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
    let mut line = Vec::new();
    assert!(read_frame(&mut reader, &mut line).await.is_err());
    assert_eq!(
        read_frame(&mut reader, &mut line).await.unwrap().unwrap()["text"],
        text
    );
    assert!(read_frame(&mut reader, &mut line).await.unwrap().is_none());
}
#[tokio::test]
async fn a_cancelled_read_keeps_the_partial_frame_for_the_next_read() {
    use tokio::io::AsyncWriteExt;
    let (mut writer, reader) = tokio::io::duplex(64);
    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = Vec::new();
    writer.write_all(br#"{"text":"split "#).await.unwrap();
    let cancelled = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        read_frame(&mut reader, &mut line),
    )
    .await;
    assert!(cancelled.is_err());
    writer.write_all(b"frame\"}\n").await.unwrap();
    assert_eq!(
        read_frame(&mut reader, &mut line).await.unwrap().unwrap(),
        serde_json::json!({"text":"split frame"})
    );
    drop(writer);
    assert!(read_frame(&mut reader, &mut line).await.unwrap().is_none());
}
#[tokio::test]
async fn an_adopted_child_speaks_json_lines() {
    let child = tokio::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut process = StdioProcess::from_child(child).unwrap();
    process.send(&serde_json::json!({"id":1})).await.unwrap();
    assert_eq!(
        process.next_frame().await.unwrap().unwrap(),
        serde_json::json!({"id":1})
    );
    assert!(process.close().await.unwrap().success());
}

fn codex_start() -> ProviderCommand {
    ProviderCommand::Start {
        resume_interrupted_turn: false,
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
        ..WireContext::default()
    }
}
fn codex_turn_params(command: &ProviderCommand, context: &WireContext) -> Value {
    let mut protocol = CodexProtocol::default();
    let start = protocol.command(command, context).unwrap();
    protocol
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
        .unwrap()
        .outbound[0]["params"]
        .clone()
}
#[test]
fn codex_runtime_policy_keeps_reference_defaults_and_explicit_overrides() {
    for (mode, approval, reviewer, sandbox) in [
        (
            RuntimeMode::ApprovalRequired,
            "untrusted",
            "user",
            "readOnly",
        ),
        (
            RuntimeMode::AutoAcceptEdits,
            "on-request",
            "user",
            "workspaceWrite",
        ),
        (
            RuntimeMode::Auto,
            "on-request",
            "auto_review",
            "workspaceWrite",
        ),
        (RuntimeMode::FullAccess, "never", "user", "dangerFullAccess"),
    ] {
        let mut command = codex_start();
        if let ProviderCommand::Start { runtime_mode, .. } = &mut command {
            *runtime_mode = mode;
        }
        let params = codex_turn_params(&command, &wire_context());
        assert_eq!(params["approvalPolicy"], approval);
        assert_eq!(params["approvalsReviewer"], reviewer);
        assert_eq!(params["sandboxPolicy"]["type"], sandbox);
        let context = WireContext {
            approval_policy: Some(Json(json!("on-request"))),
            sandbox_policy: Some(Json(
                json!({"type":"readOnly","access":{"type":"restricted","includePlatformDefaults":false,"readableRoots":[]},"networkAccess":false}),
            )),
            ..wire_context()
        };
        let params = codex_turn_params(&command, &context);
        assert_eq!(params["approvalPolicy"], "on-request");
        assert_eq!(params["sandboxPolicy"], context.sandbox_policy.unwrap().0);
        assert_eq!(params["approvalsReviewer"], reviewer);
    }
}
#[test]
fn codex_turn_selection_is_explicit_and_managed_sessions_omit_service_tiers() {
    let mut command = codex_start();
    if let ProviderCommand::Start {
        selection,
        interaction_mode,
        ..
    } = &mut command
    {
        selection.model = "gpt-5.4".into();
        selection
            .options
            .insert("reasoningEffort".into(), "xhigh".into());
        selection
            .options
            .insert("serviceTier".into(), "priority".into());
        *interaction_mode = InteractionMode::Plan;
    }
    let params = codex_turn_params(&command, &wire_context());
    assert_eq!(params["model"], "gpt-5.4");
    assert_eq!(params["effort"], "xhigh");
    assert_eq!(params["serviceTier"], "priority");
    assert_eq!(
        params["collaborationMode"]["settings"]["reasoning_effort"],
        "xhigh"
    );
    assert_eq!(params["collaborationMode"]["mode"], "plan");
    assert!(
        params["collaborationMode"]["settings"]
            .get("developer_instructions")
            .is_none()
    );
    let params = codex_turn_params(
        &command,
        &WireContext {
            omit_service_tier: true,
            ..wire_context()
        },
    );
    assert!(params.get("serviceTier").is_none());
    if let ProviderCommand::Start {
        interaction_mode, ..
    } = &mut command
    {
        *interaction_mode = InteractionMode::Default;
    }
    assert!(
        codex_turn_params(&command, &wire_context())
            .get("collaborationMode")
            .is_none()
    );
    let context = WireContext {
        developer_instructions: Some(
            "Use `delegate_task` with a structured object, never as JSON text.".into(),
        ),
        additional_context: Some(Json(
            json!({"t3_code_orchestration":{"value":"Use `delegate_task` with a structured object, never as JSON text."}}),
        )),
        ..wire_context()
    };
    let params = codex_turn_params(&command, &context);
    assert_eq!(params["collaborationMode"]["mode"], "default");
    assert_eq!(
        params["collaborationMode"]["settings"]["developer_instructions"],
        context.developer_instructions.unwrap()
    );
    assert_eq!(
        params["additionalContext"],
        context.additional_context.unwrap().0
    );
}
#[test]
fn codex_restart_continuation_resumes_natively_without_an_extra_user_prompt() {
    let mut command = codex_start();
    if let ProviderCommand::Start {
        resume_interrupted_turn,
        text,
        ..
    } = &mut command
    {
        *resume_interrupted_turn = true;
        *text = "Continue where you left off.".into();
    }
    assert_eq!(
        codex_turn_params(&command, &wire_context())["input"],
        json!([])
    );
    let output = ClaudeProtocol::default()
        .command(&command, "continuation", &[])
        .unwrap();
    assert_eq!(
        output.outbound[0]["message"]["content"],
        "Continue where you left off."
    );
}
#[test]
fn codex_thread_configuration_is_shared_by_start_resume_fork_and_rollback_resume() {
    let context = WireContext {
        thread_model: Some("gpt-5.4".into()),
        thread_config: std::collections::BTreeMap::from([(
            "mcp_servers".into(),
            Json(json!({"runtime":{"url":"http://127.0.0.1:43123/mcp"}})),
        )]),
        ..wire_context()
    };
    let mut start = codex_start();
    let mut protocol = CodexProtocol::default();
    let expected = json!({"tools.update_plan.enabled":true,"mcp_servers":{"runtime":{"url":"http://127.0.0.1:43123/mcp"}}});
    assert_eq!(
        protocol.command(&start, &context).unwrap()[0]["params"]["config"],
        expected
    );
    if let ProviderCommand::Start { native_thread, .. } = &mut start {
        *native_thread = Some("resumed".into());
    }
    let resume = protocol.command(&start, &context).unwrap();
    assert_eq!(resume[0]["method"], "thread/resume");
    assert_eq!(resume[0]["params"]["config"], expected);
    let fork = protocol
        .command(
            &ProviderCommand::Fork {
                native_thread: "resumed".into(),
                through_turn: Some("head".into()),
            },
            &context,
        )
        .unwrap();
    assert_eq!(fork[0]["params"]["config"], expected);
    assert_eq!(fork[0]["params"]["model"], "gpt-5.4");
    let revert = protocol
        .command(
            &ProviderCommand::Rollback {
                native_thread: "resumed".into(),
                absolute_head: Some("head".into()),
            },
            &context,
        )
        .unwrap();
    let resume = protocol.receive(&json!({"id":revert[0]["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"notLoaded"}}}})).unwrap();
    assert_eq!(resume.outbound[0]["params"]["config"], expected);
    assert_eq!(resume.outbound[0]["params"]["model"], "gpt-5.4");
    assert_eq!(resume.outbound[0]["params"]["cwd"], "/workspace");
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
    let init = control.initialize("runtime instructions");
    assert_eq!(
        init["request"]["appendSystemPrompt"],
        "runtime instructions"
    );
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

#[test]
fn rollback_resolves_an_absolute_boundary_across_pages_and_is_safe_to_repeat() {
    let mut protocol = CodexProtocol::default();
    let command = ProviderCommand::Rollback {
        native_thread: "thread".into(),
        absolute_head: Some("kept".into()),
    };
    let read = protocol
        .command(&command, &wire_context())
        .unwrap()
        .remove(0);
    assert_eq!(
        read["params"],
        json!({"threadId":"thread","includeTurns":false})
    );
    let page = protocol.receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"notLoaded"}}}})).unwrap().outbound.remove(0);
    assert_eq!(page["method"], "thread/resume");
    assert_eq!(
        page["params"],
        json!({"threadId":"thread","excludeTurns":true,"cwd":"/workspace","config":{"tools.update_plan.enabled":true}})
    );
    let page = protocol
        .receive(&json!({"id":page["id"],"result":{"thread":{"id":"thread"}}}))
        .unwrap()
        .outbound
        .remove(0);
    assert_eq!(
        page["params"],
        json!({"threadId":"thread","cursor":null,"limit":100,"sortDirection":"desc","itemsView":"summary"})
    );
    let next = protocol.receive(&json!({"id":page["id"],"result":{"data":[{"id":"newest"},{"id":"middle"}],"nextCursor":"older"}})).unwrap().outbound.remove(0);
    assert_eq!(next["params"]["cursor"], "older");
    let revert = protocol.receive(&json!({"id":next["id"],"result":{"data":[{"id":"boundary"},{"id":"kept"}],"nextCursor":null}})).unwrap().outbound.remove(0);
    assert_eq!(
        revert["params"],
        json!({"threadId":"thread","beforeTurnId":"boundary"})
    );
    protocol
        .receive(&json!({"id":revert["id"],"result":{"thread":{"id":"thread"}}}))
        .unwrap();
    let read = protocol
        .command(&command, &wire_context())
        .unwrap()
        .remove(0);
    let page = protocol.receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"idle"}}}})).unwrap().outbound.remove(0);
    assert!(
        protocol
            .receive(&json!({"id":page["id"],"result":{"data":[{"id":"kept"}],"nextCursor":null}}))
            .unwrap()
            .outbound
            .is_empty()
    );
}

#[test]
fn rollback_rejects_repeated_cursors_and_missing_heads_without_reverting_partial_history() {
    for missing in [false, true] {
        let mut protocol = CodexProtocol::default();
        let read = protocol
            .command(
                &ProviderCommand::Rollback {
                    native_thread: "thread".into(),
                    absolute_head: Some("kept".into()),
                },
                &wire_context(),
            )
            .unwrap()
            .remove(0);
        let page = protocol
            .receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated"}}}))
            .unwrap()
            .outbound
            .remove(0);
        let next = protocol
            .receive(
                &json!({"id":page["id"],"result":{"data":[{"id":"newest"}],"nextCursor":"again"}}),
            )
            .unwrap()
            .outbound
            .remove(0);
        let error = protocol.receive(&json!({"id":next["id"],"result":{"data":[],"nextCursor":if missing {Value::Null} else {json!("again")}}})).unwrap_err();
        assert_eq!(
            error,
            if missing {
                ProtocolError::MissingBoundary("kept".into())
            } else {
                ProtocolError::Invalid("Thread history pagination repeated a cursor.".into())
            }
        );
    }
}

#[test]
fn codex_reasoning_omits_empty_native_items_and_keeps_summary_and_content_parts_separate() {
    let mut protocol = CodexProtocol::default();
    assert!(protocol.receive(&json!({"method":"item/started","params":{"turnId":"turn","item":{"type":"reasoning","id":"reason","summary":[],"content":[]}}})).unwrap().events.is_empty());
    assert!(protocol.receive(&json!({"method":"item/completed","params":{"turnId":"turn","item":{"type":"reasoning","id":"empty","summary":[""],"content":[]}}})).unwrap().events.is_empty());
    let mut keys = vec![];
    for (method, index_key, index, text) in [
        (
            "item/reasoning/summaryTextDelta",
            "summaryIndex",
            0,
            "Summary one",
        ),
        (
            "item/reasoning/summaryTextDelta",
            "summaryIndex",
            1,
            "Summary two",
        ),
        (
            "item/reasoning/textDelta",
            "contentIndex",
            0,
            "Raw reasoning",
        ),
    ] {
        let mut frame =
            json!({"method":method,"params":{"turnId":"turn","itemId":"reason","delta":text}});
        frame["params"][index_key] = json!(index);
        let output = protocol.receive(&frame).unwrap();
        let ProviderEvent::TextDelta {
            key, text: actual, ..
        } = &output.events[0]
        else {
            panic!()
        };
        assert_eq!(actual, text);
        keys.push(key.clone());
    }
    assert_eq!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    let output=protocol.receive(&json!({"method":"item/completed","params":{"turnId":"turn","item":{"type":"reasoning","id":"reason","summary":["Summary one","Summary two"],"content":["Raw reasoning"]}}})).unwrap();
    assert_eq!(output.events.len(), 3);
    for event in output.events {
        let ProviderEvent::ItemFinished { key, status, .. } = event else {
            panic!()
        };
        assert!(keys.contains(&key));
        assert_eq!(status, ItemStatus::Completed);
    }
}

#[test]
fn stop_during_history_injection_suppresses_both_acceptance_and_unsupported_fallback_input() {
    for code in [None, Some(-32601)] {
        let mut protocol = CodexProtocol::default();
        let mut start = codex_start();
        if let ProviderCommand::Start { context, .. } = &mut start {
            *context = Some(HistoricalContext {
                messages: vec![],
                context: "History coverage".into(),
                omitted_items: 0,
                omitted_item_ids: vec![],
            });
        }
        let create = protocol.command(&start, &wire_context()).unwrap().remove(0);
        let inject = protocol
            .receive(&json!({"id":create["id"],"result":{"thread":{"id":"native"}}}))
            .unwrap()
            .outbound
            .remove(0);
        protocol
            .command(
                &ProviderCommand::Interrupt {
                    native_thread: Some("native".into()),
                    native_turn: None,
                },
                &wire_context(),
            )
            .unwrap();
        let reply = if let Some(code) = code {
            json!({"id":inject["id"],"error":{"code":code,"message":"not supported"}})
        } else {
            json!({"id":inject["id"],"result":{}})
        };
        assert!(protocol.receive(&reply).unwrap().outbound.is_empty());
    }
}
