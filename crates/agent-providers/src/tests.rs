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
        note: None,
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
    let start = protocol.command(command, context, &[]).unwrap().outbound;
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
        protocol.command(&start, &context, &[]).unwrap().outbound[0]["params"]["config"],
        expected
    );
    if let ProviderCommand::Start { native_thread, .. } = &mut start {
        *native_thread = Some("resumed".into());
    }
    let resume = protocol.command(&start, &context, &[]).unwrap().outbound;
    assert_eq!(resume[0]["method"], "thread/resume");
    assert_eq!(resume[0]["params"]["config"], expected);
    let fork = protocol
        .command(
            &ProviderCommand::Fork {
                native_thread: "resumed".into(),
                through_turn: Some("head".into()),
            },
            &context,
            &[],
        )
        .unwrap()
        .outbound;
    assert_eq!(fork[0]["params"]["config"], expected);
    assert_eq!(fork[0]["params"]["model"], "gpt-5.4");
    let revert = protocol.rollback("resumed", 1, &context).outbound;
    let resume = protocol.receive(&json!({"id":revert[0]["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"notLoaded"}}}})).unwrap();
    assert_eq!(resume.outbound[0]["params"]["config"], expected);
    assert_eq!(resume.outbound[0]["params"]["model"], "gpt-5.4");
    assert_eq!(resume.outbound[0]["params"]["cwd"], "/workspace");
}
#[test]
fn codex_rejected_resume_and_start_name_their_own_operation() {
    for (native_thread, method) in [(Some("saved"), "thread/resume"), (None, "thread/start")] {
        let mut start = codex_start();
        if let ProviderCommand::Start {
            native_thread: n, ..
        } = &mut start
        {
            *n = native_thread.map(Into::into);
        }
        let mut protocol = CodexProtocol::default();
        let sent = protocol
            .command(&start, &wire_context(), &[])
            .unwrap()
            .outbound;
        assert_eq!(sent[0]["method"], method);
        let rejected = protocol.receive(
            &json!({"id":sent[0]["id"],"error":{"code":-32600,"message":"thread not found"}}),
        );
        assert!(
            matches!(&rejected, Err(ProtocolError::Remote { operation, .. }) if operation == method),
            "{rejected:?}"
        );
    }
}
#[test]
fn codex_compact_on_a_fresh_process_resumes_the_saved_thread_first() {
    let mut protocol = CodexProtocol::default();
    let compact = ProviderCommand::Compact {
        native_thread: Some("saved".into()),
    };
    let resume = protocol
        .command(&compact, &wire_context(), &[])
        .unwrap()
        .outbound;
    assert_eq!(resume.len(), 1);
    assert_eq!(resume[0]["method"], "thread/resume");
    assert_eq!(resume[0]["params"]["threadId"], "saved");
    assert_eq!(resume[0]["params"]["excludeTurns"], true);
    let ready = protocol
        .receive(&json!({"id":resume[0]["id"],"result":{"thread":{"id":"saved"}}}))
        .unwrap();
    assert_eq!(
        ready.events,
        [ProviderEvent::SessionReady {
            native_thread: "saved".into()
        }]
    );
    assert_eq!(ready.outbound[0]["method"], "thread/compact/start");
    assert_eq!(ready.outbound[0]["params"]["threadId"], "saved");
    let again = protocol
        .command(&compact, &wire_context(), &[])
        .unwrap()
        .outbound;
    assert_eq!(again[0]["method"], "thread/compact/start");
}
// CodexAdapterV2.ts resumeThread: an archived session is unarchived and
// resumed again instead of being lost.
#[test]
fn codex_unarchives_an_archived_session_and_resumes_it_once() {
    let mut start = codex_start();
    if let ProviderCommand::Start { native_thread, .. } = &mut start {
        *native_thread = Some("saved".into());
    }
    let mut protocol = CodexProtocol::default();
    let resume = protocol
        .command(&start, &wire_context(), &[])
        .unwrap()
        .outbound;
    let archived = protocol
        .receive(&json!({"id":resume[0]["id"],"error":{"code":-32600,"message":"Session saved is archived. Run `codex unarchive saved` first."}}))
        .unwrap();
    assert_eq!(archived.outbound[0]["method"], "thread/unarchive");
    assert_eq!(archived.outbound[0]["params"], json!({"threadId":"saved"}));
    let again = protocol
        .receive(&json!({"id":archived.outbound[0]["id"],"result":{}}))
        .unwrap();
    assert_eq!(again.outbound[0]["method"], "thread/resume");
    assert_eq!(again.outbound[0]["params"], resume[0]["params"]);
    let ready = protocol
        .receive(&json!({"id":again.outbound[0]["id"],"result":{"thread":{"id":"saved"}}}))
        .unwrap();
    assert_eq!(
        ready.events,
        [ProviderEvent::SessionReady {
            native_thread: "saved".into()
        }]
    );
    assert_eq!(ready.outbound[0]["method"], "turn/start");

    let mut protocol = CodexProtocol::default();
    let resume = protocol
        .command(&start, &wire_context(), &[])
        .unwrap()
        .outbound;
    let unarchive = protocol
        .receive(&json!({"id":resume[0]["id"],"error":{"code":-32600,"message":"session saved is archived"}}))
        .unwrap()
        .outbound;
    let again = protocol
        .receive(&json!({"id":unarchive[0]["id"],"result":{}}))
        .unwrap()
        .outbound;
    let lost = protocol.receive(
        &json!({"id":again[0]["id"],"error":{"code":-32600,"message":"session saved is archived"}}),
    );
    assert!(
        matches!(&lost, Err(ProtocolError::Remote { operation, .. }) if operation == "thread/resume"),
        "{lost:?}"
    );
}
#[test]
fn only_the_reference_archive_messages_unarchive() {
    for (message, archived) in [
        ("Session 019a is archived", true),
        ("thread failed: session abc-1 is archived.", true),
        ("run codex unarchive first", true),
        ("thread not found", false),
        ("subsession abc is archived", false),
        ("session abc is archivedx", false),
        ("session  is archived", false),
        ("codex unarchived", false),
    ] {
        let mut start = codex_start();
        if let ProviderCommand::Start { native_thread, .. } = &mut start {
            *native_thread = Some("saved".into());
        }
        let mut protocol = CodexProtocol::default();
        let resume = protocol
            .command(&start, &wire_context(), &[])
            .unwrap()
            .outbound;
        let answer = protocol
            .receive(&json!({"id":resume[0]["id"],"error":{"code":-32600,"message":message}}));
        assert_eq!(answer.is_ok(), archived, "{message}");
    }
}
// ProviderTurnStartService.ts: a compaction without a native thread ensures one first.
#[test]
fn codex_compact_without_a_native_thread_starts_one_first() {
    let mut protocol = CodexProtocol::default();
    let start = protocol
        .command(
            &ProviderCommand::Compact {
                native_thread: None,
            },
            &wire_context(),
            &[],
        )
        .unwrap()
        .outbound;
    assert_eq!(start[0]["method"], "thread/start");
    let ready = protocol
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"fresh"}}}))
        .unwrap();
    assert_eq!(
        ready.events,
        [ProviderEvent::SessionReady {
            native_thread: "fresh".into()
        }]
    );
    assert_eq!(ready.outbound[0]["method"], "thread/compact/start");
    assert_eq!(ready.outbound[0]["params"]["threadId"], "fresh");
}
#[test]
fn codex_reports_a_root_turn_in_flight_until_it_completes() {
    let mut protocol = CodexProtocol::default();
    assert!(!protocol.turn_in_flight(""));
    let start = protocol
        .command(&codex_start(), &wire_context(), &[])
        .unwrap()
        .outbound;
    assert!(protocol.turn_in_flight(""));
    let turn = protocol
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
        .unwrap()
        .outbound;
    assert!(protocol.turn_in_flight(""));
    protocol
        .receive(&json!({"id":turn[0]["id"],"result":{"turn":{"id":"turn-1"}}}))
        .unwrap();
    assert!(protocol.turn_in_flight(""));
    protocol
        .receive(&json!({"method":"turn/started","params":{"threadId":"native-child","turn":{"id":"child-turn"}}}))
        .unwrap();
    protocol
        .receive(&json!({"method":"turn/completed","params":{"threadId":"native","turn":{"id":"turn-1","status":"interrupted"}}}))
        .unwrap();
    assert!(!protocol.turn_in_flight(""));
}
#[test]
fn codex_stop_before_thread_ready_cancels_prompt_and_the_next_prompt_can_start() {
    use serde_json::json;
    let mut protocol = CodexProtocol::default();
    let start = protocol
        .command(&codex_start(), &wire_context(), &[])
        .unwrap()
        .outbound;
    assert_eq!(start[0]["method"], "thread/start");
    assert!(
        protocol
            .command(
                &ProviderCommand::Interrupt {
                    native_thread: None,
                    native_turn: None
                },
                &wire_context(),
                &[]
            )
            .unwrap()
            .outbound
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
    let next = protocol
        .command(&next_command, &wire_context(), &[])
        .unwrap()
        .outbound;
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
    let start = protocol
        .command(&codex_start(), &wire_context(), &[])
        .unwrap()
        .outbound;
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
                &wire_context(),
                &[]
            )
            .unwrap()
            .outbound
            .is_empty()
    );
    let started = protocol.receive(&json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"turn-root"}}})).unwrap();
    assert_eq!(started.outbound[0]["method"], "turn/interrupt");
    let reply = protocol
        .receive(&json!({"id":ready.outbound[0]["id"],"result":{"turn":{"id":"turn-root"}}}))
        .unwrap();
    assert!(reply.outbound.is_empty());
    protocol.receive(&json!({"method":"item/started","params":{"threadId":"root","item":{"id":"tool","type":"commandExecution","command":"sleep 30","processId":"123","status":"inProgress"}}})).unwrap();
    let stop = protocol
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("root".into()),
                native_turn: Some("turn-root".into()),
            },
            &wire_context(),
            &[],
        )
        .unwrap()
        .outbound;
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
    protocol
        .receive(&json!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"turn-child"}}}))
        .unwrap();
    let child = protocol
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("child".into()),
                native_turn: Some("turn-child".into()),
            },
            &wire_context(),
            &[],
        )
        .unwrap()
        .outbound;
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
        let start = protocol
            .command(&command, &wire_context(), &[])
            .unwrap()
            .outbound;
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
        assert_eq!(
            text,
            if error.is_some() {
                format!("{}\n\nUser message:\nhello", render_history(&history))
            } else {
                "hello".into()
            }
        );
        assert_eq!(
            result.events.contains(&ProviderEvent::ContextInjected),
            error.is_none()
        );
    }
}

// T3 ProviderTurnStartService.ts: context, then the restart note, then "User message:".
#[test]
fn inline_history_and_restart_notes_precede_the_labelled_user_message() {
    let history = select_history(&[], "Recover source history", 0, 16_000);
    let mut command = codex_start();
    if let ProviderCommand::Start { context, note, .. } = &mut command {
        *context = Some(history.clone());
        *note = Some("Note: work was cancelled".into());
    }
    let mut claude = ClaudeProtocol::default();
    let sent = claude.command(&command, "prompt", &[]).unwrap().outbound[0].clone();
    assert_eq!(
        sent["message"]["content"],
        format!(
            "{}\n\nNote: work was cancelled\n\nUser message:\nhello",
            render_history(&history)
        )
    );
    let mut codex = CodexProtocol::default();
    let start = codex
        .command(&command, &wire_context(), &[])
        .unwrap()
        .outbound;
    let ready = codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
        .unwrap();
    let injected = codex
        .receive(&json!({"id":ready.outbound[0]["id"],"result":{}}))
        .unwrap();
    assert_eq!(
        injected.outbound[0]["params"]["input"][0]["text"],
        "Note: work was cancelled\n\nUser message:\nhello"
    );
}
// T3 CodexAdapterV2.ts toCodexInput: start and steer send text, then image data URLs.
#[test]
fn codex_start_and_steer_send_prepared_images_after_the_text() {
    let image = Attachment {
        kind: AttachmentKind::Image,
        source: None,
        id: "shot".into(),
        name: "shot.png".into(),
        mime_type: "image/png".into(),
        path: "/attachments/shot.png".into(),
        size: 3,
    };
    let prepared = [PreparedImage {
        attachment_id: "shot".into(),
        mime_type: "image/png".into(),
        base64: "AAEC".into(),
    }];
    let mut command = codex_start();
    if let ProviderCommand::Start {
        attachments, text, ..
    } = &mut command
    {
        attachments.push(image.clone());
        *text = "€review this".into();
    }
    let mut codex = CodexProtocol::default();
    let start = codex
        .command(&command, &wire_context(), &prepared)
        .unwrap()
        .outbound;
    let ready = codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
        .unwrap();
    let input = &ready.outbound[0]["params"]["input"];
    assert_eq!(
        input[1],
        json!({"type":"image","url":"data:image/png;base64,AAEC"})
    );
    assert!(
        input[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("$review this\n\n")
    );
    codex
        .receive(
            &json!({"method":"turn/started","params":{"threadId":"native","turn":{"id":"turn"}}}),
        )
        .unwrap();
    let steer = codex
        .command(
            &ProviderCommand::Steer {
                message: MessageId::new("steer").unwrap(),
                text: "and this".into(),
                attachments: vec![image],
            },
            &wire_context(),
            &prepared,
        )
        .unwrap()
        .outbound;
    assert_eq!(steer[0]["method"], "turn/steer");
    assert_eq!(
        steer[0]["params"]["input"][1],
        json!({"type":"image","url":"data:image/png;base64,AAEC"})
    );
    assert!(
        codex
            .command(
                &ProviderCommand::Steer {
                    message: MessageId::new("missing").unwrap(),
                    text: "x".into(),
                    attachments: vec![Attachment {
                        id: "other".into(),
                        ..prepared_attachment()
                    }],
                },
                &wire_context(),
                &prepared,
            )
            .is_err()
    );
}
#[test]
fn a_cancelled_codex_question_replies_with_no_answers() {
    let mut codex = CodexProtocol::default();
    codex
        .receive(&json!({"id":7,"method":"item/tool/requestUserInput","params":{"threadId":"t","questions":[{"id":"q","header":"Q","question":"Which?"}]}}))
        .unwrap();
    let reply = codex
        .command(
            &ProviderCommand::Respond {
                native_key: "7".into(),
                decision: Some(ApprovalDecision::Cancel),
                answers: None,
                input: None,
            },
            &wire_context(),
            &[],
        )
        .unwrap()
        .outbound;
    assert_eq!(reply[0], json!({"id":7,"result":{"answers":{}}}));
}
fn prepared_attachment() -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        source: None,
        id: "shot".into(),
        name: "shot.png".into(),
        mime_type: "image/png".into(),
        path: "/attachments/shot.png".into(),
        size: 3,
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
fn rollback_finds_the_revert_boundary_across_pages_of_newest_first_turns() {
    // T3 CodexThreadRevert.test.ts "finds the revert boundary across pages".
    let mut protocol = CodexProtocol::default();
    let read = protocol
        .rollback("thread", 3, &wire_context())
        .outbound
        .remove(0);
    assert_eq!(read["method"], "thread/read");
    assert_eq!(
        read["params"],
        json!({"threadId":"thread","includeTurns":false})
    );
    let resume = protocol.receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"notLoaded"}}}})).unwrap().outbound.remove(0);
    assert_eq!(resume["method"], "thread/resume");
    assert_eq!(
        resume["params"],
        json!({"threadId":"thread","excludeTurns":true,"cwd":"/workspace","config":{"tools.update_plan.enabled":true}})
    );
    let page = protocol
        .receive(&json!({"id":resume["id"],"result":{"thread":{"id":"thread"}}}))
        .unwrap()
        .outbound
        .remove(0);
    assert_eq!(
        page["params"],
        json!({"threadId":"thread","cursor":null,"limit":3,"sortDirection":"desc","itemsView":"summary"})
    );
    let next = protocol.receive(&json!({"id":page["id"],"result":{"data":[{"id":"newest"},{"id":"middle"}],"nextCursor":"older"}})).unwrap().outbound.remove(0);
    assert_eq!(
        next["params"],
        json!({"threadId":"thread","cursor":"older","limit":1,"sortDirection":"desc","itemsView":"summary"})
    );
    let revert = protocol
        .receive(&json!({"id":next["id"],"result":{"data":[{"id":"boundary"}],"nextCursor":null}}))
        .unwrap()
        .outbound
        .remove(0);
    assert_eq!(revert["method"], "thread/revert");
    assert_eq!(
        revert["params"],
        json!({"threadId":"thread","beforeTurnId":"boundary"})
    );
    assert_eq!(
        protocol
            .receive(
                &json!({"id":revert["id"],"error":{"code":-32603,"message":"boundary reached"}})
            )
            .unwrap_err(),
        ProtocolError::Remote {
            request: Some(revert["id"].to_string()),
            operation: "thread/revert".into(),
            message: "boundary reached".into(),
            turn_completed: false,
        }
    );
    // An empty history reads the thread instead of reverting.
    let read = protocol
        .rollback("empty", 2, &wire_context())
        .outbound
        .remove(0);
    let page = protocol.receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"idle"}}}})).unwrap().outbound.remove(0);
    let reread = protocol
        .receive(&json!({"id":page["id"],"result":{"data":[],"nextCursor":null}}))
        .unwrap()
        .outbound
        .remove(0);
    assert_eq!(reread["method"], "thread/read");
    assert_eq!(
        protocol
            .receive(&json!({"id":reread["id"],"result":{"thread":{"id":"empty"}}}))
            .unwrap()
            .completion,
        Some(Completion::RolledBack {
            native_thread: "empty".into()
        })
    );
}

#[test]
fn rollback_rejects_repeated_cursors_instead_of_reverting_incomplete_history() {
    // T3 CodexThreadRevert.test.ts "rejects repeated cursors".
    let mut protocol = CodexProtocol::default();
    let read = protocol
        .rollback("thread", 3, &wire_context())
        .outbound
        .remove(0);
    let page = protocol
        .receive(&json!({"id":read["id"],"result":{"thread":{"historyMode":"paginated"}}}))
        .unwrap()
        .outbound
        .remove(0);
    let next = protocol
        .receive(&json!({"id":page["id"],"result":{"data":[{"id":"newest"}],"nextCursor":"again"}}))
        .unwrap()
        .outbound
        .remove(0);
    assert_eq!(next["params"]["limit"], 2);
    let error = protocol
        .receive(&json!({"id":next["id"],"result":{"data":[],"nextCursor":"again"}}))
        .unwrap_err();
    assert_eq!(
        error,
        ProtocolError::Invalid("Thread history pagination repeated a cursor.".into())
    );
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
        let create = protocol
            .command(&start, &wire_context(), &[])
            .unwrap()
            .outbound
            .remove(0);
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
                &[],
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
fn codex_ready() -> CodexProtocol {
    let mut codex = CodexProtocol::default();
    let start = codex
        .command(&codex_start(), &wire_context(), &[])
        .unwrap()
        .outbound;
    let ready = codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"root"}}}))
        .unwrap();
    codex
        .receive(&json!({"id":ready.outbound[0]["id"],"result":{"turn":{"id":"turn"}}}))
        .unwrap();
    codex
        .receive(
            &json!({"method":"turn/started","params":{"threadId":"root","turn":{"id":"turn"}}}),
        )
        .unwrap();
    codex
}
fn notify(codex: &mut CodexProtocol, method: &str, params: Value) -> Translation {
    codex
        .receive(&json!({"method":method,"params":params}))
        .unwrap()
}
#[test]
fn stop_while_the_thread_is_starting_suppresses_its_prompt_even_after_thread_started() {
    let mut codex = CodexProtocol::default();
    let start = codex
        .command(&codex_start(), &wire_context(), &[])
        .unwrap()
        .outbound;
    notify(
        &mut codex,
        "thread/started",
        json!({"thread":{"id":"root"}}),
    );
    let stop = codex
        .command(
            &ProviderCommand::Interrupt {
                native_thread: None,
                native_turn: None,
            },
            &wire_context(),
            &[],
        )
        .unwrap();
    assert!(stop.outbound.is_empty());
    let reply = codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"root"}}}))
        .unwrap();
    assert!(reply.outbound.is_empty());
}
// T3 CodexAdapterV2.test.ts:3805 and :4004.
#[test]
fn commands_running_at_turn_end_report_later_and_stop_without_a_turn_interrupt() {
    const COMMAND: &str = "sleep 20 && echo CODEX_BG_WAKE_DONE";
    let mut codex = codex_ready();
    notify(
        &mut codex,
        "item/started",
        json!({"threadId":"root","turnId":"turn","item":{"type":"commandExecution","id":"call-bg","command":COMMAND,"processId":"4242","status":"inProgress"}}),
    );
    let done = notify(
        &mut codex,
        "turn/completed",
        json!({"threadId":"root","turn":{"id":"turn","status":"completed"}}),
    );
    assert!(matches!(
        &done.events[..],
        [ProviderEvent::BackgroundTask { key, status: None, description, .. }, ProviderEvent::TurnFinished { .. }]
            if key == "call-bg" && description == COMMAND
    ));
    let late = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"commandExecution","id":"call-bg","command":COMMAND,"processId":"4242","status":"completed","exitCode":0,"aggregatedOutput":"CODEX_BG_WAKE_DONE\n"}}),
    );
    let detail = format!(
        "Background command completed (exit 0): {COMMAND}\n\nOutput tail:\nCODEX_BG_WAKE_DONE"
    );
    assert!(
        matches!(&late.events[0], ProviderEvent::ItemFinished { key, status: ItemStatus::Completed, .. } if key == "call-bg")
    );
    assert_eq!(
        late.events[1],
        ProviderEvent::BackgroundTask {
            key: "call-bg".into(),
            tool: "call-bg".into(),
            kind: BackgroundKind::Command,
            description: COMMAND.into(),
            status: Some(ItemStatus::Completed),
            summary: Some(detail.clone()),
            exit_code: Some(0),
        }
    );
    assert_eq!(
        late.events[2],
        ProviderEvent::Wake {
            text: detail,
            detail: Some(COMMAND.into()),
        }
    );

    let mut codex = codex_ready();
    notify(
        &mut codex,
        "item/started",
        json!({"threadId":"root","turnId":"turn","item":{"type":"commandExecution","id":"call-bg","command":COMMAND,"processId":"4242","status":"inProgress"}}),
    );
    notify(
        &mut codex,
        "turn/completed",
        json!({"threadId":"root","turn":{"id":"turn","status":"completed"}}),
    );
    let stop = codex
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("root".into()),
                native_turn: Some("turn".into()),
            },
            &wire_context(),
            &[],
        )
        .unwrap();
    assert_eq!(
        stop.outbound,
        [
            json!({"id":3,"method":"thread/backgroundTerminals/terminate","params":{"threadId":"root","processId":"4242"}})
        ]
    );
    let listed = codex
        .receive(&json!({"id":3,"result":{"terminated":false}}))
        .unwrap();
    assert_eq!(
        listed.outbound,
        [json!({"id":4,"method":"thread/backgroundTerminals/list","params":{"threadId":"root"}})]
    );
    assert!(matches!(
        codex.receive(&json!({"id":4,"result":{"data":[{"processId":"4242"}],"nextCursor":null}})),
        Err(ProtocolError::Remote { message, .. }) if message == "Codex background terminal 4242 remained active after termination."
    ));
    let stop = codex
        .command(
            &ProviderCommand::Interrupt {
                native_thread: Some("root".into()),
                native_turn: Some("turn".into()),
            },
            &wire_context(),
            &[],
        )
        .unwrap();
    let ended = codex
        .receive(&json!({"id":stop.outbound[0]["id"],"result":{"terminated":true}}))
        .unwrap();
    assert!(matches!(
        &ended.events[0],
        ProviderEvent::ItemFinished {
            status: ItemStatus::Interrupted,
            ..
        }
    ));
    assert!(matches!(
        &ended.events[1],
        ProviderEvent::BackgroundTask {
            status: Some(ItemStatus::Cancelled),
            ..
        }
    ));
    let late = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"commandExecution","id":"call-bg","command":COMMAND,"status":"failed","exitCode":143}}),
    );
    assert!(
        !late
            .events
            .iter()
            .any(|event| matches!(event, ProviderEvent::Wake { .. }))
    );
}
// T3 CodexAdapterV2.test.ts:2242.
#[test]
fn asynchronous_codex_questions_become_message_requests_without_prose() {
    let mut codex = codex_ready();
    let started = notify(
        &mut codex,
        "item/started",
        json!({"threadId":"root","turnId":"turn","item":{"type":"agentMessage","id":"async-question-item","text":"","delivery":"async"}}),
    );
    assert!(started.events.is_empty());
    let done = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"agentMessage","id":"async-question-item","text":"Which branch?","delivery":"async","questions":[{"title":"Which branch?","options":["main","dev"]}]}}),
    );
    let [
        ProviderEvent::RequestOpened {
            key,
            body: RequestBody::Questions { questions },
            capability: ResponseCapability::Message,
            ..
        },
    ] = &done.events[..]
    else {
        panic!("{:?}", done.events)
    };
    assert_eq!(key, "async:async-question-item");
    assert_eq!(questions[0].question, "Which branch?");
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|o| o.label.as_str())
            .collect::<Vec<_>>(),
        ["main", "dev"]
    );
}
// T3 CodexAdapterV2.ts codexItemStatus and CodexAdapterV2.test.ts:6670.
#[test]
fn codex_item_and_subagent_states_use_the_reference_mapping() {
    let mut codex = codex_ready();
    let declined = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"fileChange","id":"edit","changes":[],"status":"declined"}}),
    );
    assert!(matches!(
        &declined.events[0],
        ProviderEvent::ItemFinished {
            status: ItemStatus::Cancelled,
            ..
        }
    ));
    notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["a","b","c","d","e","f"],"agentsStates":{}}}),
    );
    let states = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"collabAgentToolCall","id":"list","tool":"listAgents","receiverThreadIds":["a","b","c","d","e","f","unknown"],"agentsStates":{
            "a":{"status":"errored","message":null},"b":{"status":"shutdown","message":null},
            "c":{"status":"notFound","message":null},"d":{"status":"interrupted","message":null},
            "e":{"status":"completed","message":"done"},"f":{"status":"running","message":null},
            "unknown":{"status":"completed","message":null}}}}),
    );
    let finished = states
        .events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::SubagentFinished { key, status, .. } => Some((key.as_str(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        finished,
        [
            ("a", ItemStatus::Failed),
            ("b", ItemStatus::Cancelled),
            ("c", ItemStatus::Failed),
            ("d", ItemStatus::Interrupted),
            ("e", ItemStatus::Completed)
        ]
    );
}
// T3 CodexAdapterV2.ts:2685: only a spawn assigns a child's parent.
#[test]
fn collaboration_calls_do_not_reparent_existing_children() {
    let mut codex = codex_ready();
    notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"collabAgentToolCall","id":"s1","tool":"spawnAgent","receiverThreadIds":["a"],"agentsStates":{}}}),
    );
    notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"a","turnId":"t-a","item":{"type":"collabAgentToolCall","id":"s2","tool":"spawnAgent","receiverThreadIds":["b"],"agentsStates":{}}}),
    );
    notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"collabAgentToolCall","id":"w","tool":"wait","receiverThreadIds":["b"],"agentsStates":{}}}),
    );
    let output = notify(
        &mut codex,
        "item/agentMessage/delta",
        json!({"threadId":"b","turnId":"t-b","itemId":"m","delta":"hi"}),
    );
    let ProviderEvent::Child { key, event } = &output.events[0] else {
        panic!()
    };
    assert_eq!(key, "a");
    assert!(matches!(event.as_ref(), ProviderEvent::Child { key, .. } if key == "b"));
}
// T3 CodexAdapterV2.test.ts:5719 and :2837.
#[test]
fn codex_failures_and_retries_keep_their_reference_classification_and_lifecycle() {
    let mut codex = codex_ready();
    let failed = notify(
        &mut codex,
        "turn/completed",
        json!({"threadId":"root","turn":{"id":"turn","status":"failed","error":{"message":"quota exhausted","codexErrorInfo":"usageLimitExceeded"}}}),
    );
    assert!(matches!(
        &failed.events[0],
        ProviderEvent::ItemFinished { key, kind: ProviderItem::Error { message, code: Some(code), class: Some(class), .. }, status: ItemStatus::Failed, .. }
            if key == "terminal-failure:turn" && message == "quota exhausted" && code == "usageLimitExceeded" && class == "usage_limit"
    ));
    let mut codex = codex_ready();
    let retry = notify(
        &mut codex,
        "error",
        json!({"threadId":"root","turnId":"turn","willRetry":true,"error":{"message":"Reconnecting... 2/5","additionalDetails":"The response stream disconnected.","codexErrorInfo":{"responseStreamDisconnected":{"httpStatusCode":529}}}}),
    );
    assert_eq!(
        retry.events,
        [ProviderEvent::ItemStarted {
            key: "terminal-failure:turn".into(),
            kind: ProviderItem::Error {
                message: "The response stream disconnected.".into(),
                retry: Some(RetryProgress {
                    attempt: 2,
                    max_attempts: Some(5),
                    delay_ms: None
                }),
                code: Some("responseStreamDisconnected".into()),
                class: Some("transport_error".into()),
                retryable: Some(true),
            },
        }]
    );
    let resumed = notify(
        &mut codex,
        "item/started",
        json!({"threadId":"root","turnId":"turn","item":{"type":"commandExecution","id":"pwd","command":"pwd","status":"inProgress"}}),
    );
    assert!(matches!(
        &resumed.events[0],
        ProviderEvent::ItemFinished { key, status: ItemStatus::Completed, .. } if key == "terminal-failure:turn"
    ));
}
// T3 CodexAdapterV2.test.ts final-answer cases (3348-3687).
#[test]
fn final_answers_drop_repeats_and_late_empty_completions() {
    let answers = |messages: &[(&str, Option<&str>)]| {
        let mut codex = codex_ready();
        let mut texts = vec![];
        for (index, (text, phase)) in messages.iter().enumerate() {
            let mut item = json!({"type":"agentMessage","id":format!("m{index}"),"text":""});
            if let Some(phase) = phase {
                item["phase"] = json!(phase);
            }
            notify(
                &mut codex,
                "item/started",
                json!({"threadId":"root","turnId":"turn","item":item.clone()}),
            );
            item["text"] = json!(text);
            for event in notify(
                &mut codex,
                "item/completed",
                json!({"threadId":"root","turnId":"turn","item":item}),
            )
            .events
            {
                if let ProviderEvent::ItemFinished {
                    text: Some(text), ..
                } = event
                {
                    texts.push(text);
                }
            }
        }
        texts
    };
    let fa = Some("final_answer");
    assert_eq!(answers(&[("OK", fa), ("", fa)]), ["OK"]);
    assert_eq!(answers(&[("OK", fa), ("OK", fa)]), ["OK"]);
    assert_eq!(answers(&[("", fa)]), [""]);
    assert_eq!(answers(&[("", fa), ("", fa)]), [""]);
    assert_eq!(
        answers(&[("Working on it.", Some("commentary")), ("", fa)]),
        ["Working on it.", ""]
    );
    assert_eq!(answers(&[("OK", None), ("", None)]), ["OK"]);
    assert_eq!(answers(&[("", fa), ("OK", fa)]), ["", "OK"]);
}
// T3 CodexAdapterV2.test.ts:6455.
#[test]
fn rerouted_child_models_update_the_child() {
    let mut codex = codex_ready();
    notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"root","turnId":"turn","item":{"type":"collabAgentToolCall","id":"s","tool":"spawnAgent","receiverThreadIds":["child"],"agentsStates":{}}}),
    );
    for (method, params) in [
        (
            "model/rerouted",
            json!({"threadId":"child","toModel":"gpt-5.6-sol"}),
        ),
        (
            "thread/settings/updated",
            json!({"threadId":"child","threadSettings":{"model":"gpt-5.6-sol"}}),
        ),
    ] {
        let output = notify(&mut codex, method, params);
        assert_eq!(
            output.events,
            [ProviderEvent::Child {
                key: "child".into(),
                event: Box::new(ProviderEvent::ModelObserved {
                    model: "gpt-5.6-sol".into()
                })
            }]
        );
    }
    assert!(
        notify(
            &mut codex,
            "model/rerouted",
            json!({"threadId":"root","toModel":"other"})
        )
        .events
        .is_empty()
    );
}
#[test]
fn native_rollback_and_fork_report_completion() {
    let mut codex = CodexProtocol::default();
    let fork = codex
        .command(
            &ProviderCommand::Fork {
                native_thread: "root".into(),
                through_turn: None,
            },
            &wire_context(),
            &[],
        )
        .unwrap();
    assert_eq!(
        codex
            .receive(&json!({"id":fork.outbound[0]["id"],"result":{"thread":{"id":"forked"}}}))
            .unwrap()
            .completion,
        Some(Completion::Forked {
            native_thread: "forked".into()
        })
    );
    // T3 rollbackThread: no turn to discard sends nothing.
    let reached = codex.rollback("root", 0, &wire_context());
    assert!(reached.outbound.is_empty());
    assert_eq!(
        reached.completion,
        Some(Completion::RolledBack {
            native_thread: "root".into()
        })
    );
    let read = codex.rollback("root", 1, &wire_context());
    let page = codex
        .receive(&json!({"id":read.outbound[0]["id"],"result":{"thread":{"historyMode":"paginated","status":{"type":"idle"}}}}))
        .unwrap();
    let revert = codex
        .receive(
            &json!({"id":page.outbound[0]["id"],"result":{"data":[{"id":"t2"}],"nextCursor":"older"}}),
        )
        .unwrap();
    assert_eq!(
        revert.outbound[0]["params"],
        json!({"threadId":"root","beforeTurnId":"t2"})
    );
    assert_eq!(
        codex
            .receive(&json!({"id":revert.outbound[0]["id"],"result":{"thread":{"id":"root"}}}))
            .unwrap()
            .completion,
        Some(Completion::RolledBack {
            native_thread: "root".into()
        })
    );
}
fn inner(output: Translation) -> Vec<ProviderEvent> {
    output
        .events
        .into_iter()
        .flat_map(|event| match event {
            ProviderEvent::NativeOutput { events, .. } => events,
            event => vec![event],
        })
        .collect()
}
fn claude_receive(claude: &mut ClaudeProtocol, frame: Value) -> Vec<ProviderEvent> {
    inner(claude.receive(&frame).unwrap())
}
// T3 ClaudeAdapterV2.test.ts:3326 and :5351.
#[test]
fn claude_rosters_replace_background_work_and_foreground_tasks_stay_foreground() {
    let mut claude = ClaudeProtocol::default();
    let foreground = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"task_started","task_id":"fg","task_type":"local_bash","is_backgrounded":false,"description":"Foreground"}),
    );
    assert!(foreground.is_empty());
    let started = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"task_started","task_id":"bg","task_type":"local_bash","is_backgrounded":true,"description":"Sleep"}),
    );
    assert!(
        matches!(&started[0], ProviderEvent::BackgroundTask { key, status: None, .. } if key == "bg")
    );
    let roster = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"other","task_type":"local_bash","description":" Monitor 0 "},{"task_id":"agent","task_type":"local_agent","description":"ignored"}]}),
    );
    assert_eq!(
        roster,
        [ProviderEvent::BackgroundRoster {
            tasks: vec![BackgroundEntry {
                key: "other".into(),
                tool: "other".into(),
                kind: BackgroundKind::Command,
                description: "Monitor 0".into(),
            }]
        }]
    );
    let done = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"task_notification","task_id":"bg","status":"completed","summary":"done"}),
    );
    assert!(
        matches!(&done[0], ProviderEvent::BackgroundTask { description, status: Some(ItemStatus::Completed), .. } if description == "Sleep")
    );
}
// T3 ClaudeAdapterV2.ts:1976 and :2059.
#[test]
fn claude_server_tools_and_typed_results_are_tool_activity() {
    let mut claude = ClaudeProtocol::default();
    let events = claude_receive(
        &mut claude,
        json!({"type":"assistant","message":{"id":"m","content":[
            {"type":"mcp_tool_use","id":"mcp-1","name":"search","server_name":"docs","input":{"q":"x"}},
            {"type":"mcp_tool_result","tool_use_id":"mcp-1","is_error":true,"content":[{"type":"text","text":"denied"}]},
            {"type":"server_tool_use","id":"ws-1","name":"web_search","input":{"query":"rust"}},
            {"type":"web_search_tool_result","tool_use_id":"ws-1","content":{"type":"web_search_tool_result_error","error_code":"unavailable"}}
        ]}}),
    );
    let finished = events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::ItemFinished {
                key, status, kind, ..
            } => Some((key.as_str(), *status, kind)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2);
    assert_eq!(
        (finished[0].0, finished[0].1),
        ("mcp-1", ItemStatus::Failed)
    );
    assert_eq!((finished[1].0, finished[1].1), ("ws-1", ItemStatus::Failed));
    assert!(matches!(finished[1].2, ProviderItem::WebSearch { query, .. } if query == "rust"));
    let unknown = claude_receive(
        &mut claude,
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"lost","content":"output"}]}}),
    );
    assert!(
        matches!(&unknown[0], ProviderEvent::ItemStarted { key, kind: ProviderItem::Tool { name, .. } } if key == "lost" && name == "tool")
    );
    assert!(
        matches!(&unknown[1], ProviderEvent::ItemFinished { key, text: Some(text), .. } if key == "lost" && text == "output")
    );
}
// T3 ClaudeAdapterV2.test.ts:4328.
#[test]
fn claude_bash_output_joins_stdout_and_stderr() {
    let mut claude = ClaudeProtocol::default();
    claude_receive(
        &mut claude,
        json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"bash","name":"Bash","input":{"command":"git status"}}]}}),
    );
    let events = claude_receive(
        &mut claude,
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"bash","content":"On branch main"}]},"tool_use_result":{"stdout":"On branch main","stderr":"warning: dirty","interrupted":false,"isImage":false}}),
    );
    assert!(
        matches!(&events[0], ProviderEvent::ItemFinished { text: Some(text), .. } if text == "On branch main\nwarning: dirty")
    );
}
// T3 ClaudeAdapterV2.test.ts:3130 and :3204.
#[test]
fn claude_api_retries_update_one_item_until_recovery_or_failure() {
    let mut claude = ClaudeProtocol::default();
    let retry = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"retry_delay_ms":1500,"error_status":529,"error":"overloaded"}),
    );
    assert_eq!(
        retry,
        [ProviderEvent::ItemStarted {
            key: "terminal-failure".into(),
            kind: ProviderItem::Error {
                message: "Claude API overloaded.".into(),
                retry: Some(RetryProgress {
                    attempt: 2,
                    max_attempts: Some(10),
                    delay_ms: Some(1500)
                }),
                code: Some("api_error_529".into()),
                class: Some("provider_error".into()),
                retryable: Some(true),
            }
        }]
    );
    let recovered = claude_receive(
        &mut claude,
        json!({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"ok"}]}}),
    );
    assert!(
        matches!(&recovered[0], ProviderEvent::ItemFinished { key, status: ItemStatus::Completed, .. } if key == "terminal-failure")
    );
    claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"api_retry","attempt":10,"max_retries":10,"retry_delay_ms":38010,"error_status":529,"error":"overloaded"}),
    );
    let failed = claude_receive(
        &mut claude,
        json!({"type":"result","subtype":"success","is_error":true,"api_error_status":529,"result":"overloaded","num_turns":1}),
    );
    assert!(failed.iter().any(|event| matches!(event,
        ProviderEvent::ItemFinished { key, kind: ProviderItem::Error { retry: Some(RetryProgress { attempt: 10, .. }), code: Some(code), .. }, status: ItemStatus::Failed, .. }
            if key == "terminal-failure" && code == "api_error_529")));
}
// T3 ClaudeAdapterV2.ts:6298: a success result marked as an error is neither an answer nor a failure.
#[test]
fn claude_success_results_marked_as_errors_add_no_answer_or_failure() {
    let mut claude = ClaudeProtocol::default();
    let events = claude_receive(
        &mut claude,
        json!({"type":"result","subtype":"success","is_error":true,"result":"Provider failure details.","num_turns":1}),
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderEvent::ItemFinished { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            ..
        }
    )));
}
// T3 ClaudeAdapterV2.test.ts:2691 and ClaudeAdapterV2.ts:2097.
#[test]
fn claude_refusal_fallbacks_and_mcp_names_use_the_reference_fields() {
    let mut claude = ClaudeProtocol::default();
    let notice = claude_receive(
        &mut claude,
        json!({"type":"system","subtype":"model_refusal_fallback","uuid":"u","content":"Safeguards flagged this message. Switched to Opus 4.8.","original_model":"a","fallback_model":"b"}),
    );
    assert!(
        matches!(&notice[0], ProviderEvent::ItemFinished { kind: ProviderItem::Notice { message }, .. } if message == "Safeguards flagged this message. Switched to Opus 4.8.")
    );
    let events = claude_receive(
        &mut claude,
        json!({"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"mcp","name":"mcp__x__read","input":{}}]},"tool_use_meta":[{"id":"mcp","display_name":"Read\n  issue","server_display_name":" GitHub \t App "}]}),
    );
    let ProviderEvent::ItemStarted {
        kind: ProviderItem::Tool { presentation, .. },
        ..
    } = &events[0]
    else {
        panic!("{events:?}")
    };
    assert_eq!(presentation.title.as_deref(), Some("Read issue"));
    assert_eq!(
        presentation.source.as_ref().unwrap().0["name"],
        "GitHub App"
    );
}
// T3 ClaudeAdapterV2.test.ts:2376 and :2441.
#[test]
fn claude_rate_limits_announce_rejected_windows_unless_overage_is_allowed() {
    let mut claude = ClaudeProtocol::default();
    for info in [
        json!({"status":"allowed_warning"}),
        json!({"status":"rejected","overageStatus":"allowed"}),
        json!({"status":"rejected","overageStatus":"allowed_warning"}),
        json!({"status":"rejected","isUsingOverage":true}),
        json!({"status":"rejected","overageInUse":true}),
    ] {
        assert!(
            claude_receive(
                &mut claude,
                json!({"type":"rate_limit_event","rate_limit_info":info})
            )
            .is_empty()
        );
    }
    let notice = claude_receive(
        &mut claude,
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":7200}}),
    );
    assert_eq!(
        notice,
        [ProviderEvent::ItemFinished {
            key: "usage-limit:five_hour:7200".into(),
            kind: ProviderItem::UsageLimit {
                limit: Some("five_hour".into()),
                resets_at: Some(7200)
            },
            text: None,
            status: ItemStatus::Completed,
        }]
    );
    assert_eq!(
        usage_limit_notice(Some("five_hour"), Some(7200), 0),
        "Claude usage limit reached. This turn is paused until the 5-hour limit resets in 2h."
    );
    assert_eq!(
        usage_limit_notice(Some("seven_day"), Some(5400), 0),
        "Claude usage limit reached. This turn is paused until the 7-day limit resets in 1h 30m."
    );
    assert_eq!(
        usage_limit_notice(None, None, 0),
        "Claude usage limit reached. This turn is paused until the limit resets."
    );
    let failed = claude_receive(
        &mut claude,
        json!({"type":"result","subtype":"error_during_execution","errors":[],"num_turns":1}),
    );
    assert!(failed.iter().any(|event| matches!(event,
        ProviderEvent::ItemFinished { kind: ProviderItem::Error { class: Some(class), .. }, .. } if class == "usage_limit")));
}
// T3 ClaudeSkillDispatch.ts and ClaudeAdapterV2.ts:7175.
#[test]
fn claude_prompts_run_known_skills_and_request_ultrathink_effort() {
    let mut claude = ClaudeProtocol::default();
    claude.set_skills(vec!["review".into()]);
    let mut start = codex_start();
    if let ProviderCommand::Start {
        selection, text, ..
    } = &mut start
    {
        selection.driver = Driver::Claude;
        selection.model = "claude-sonnet-4-6".into();
        selection
            .options
            .insert("effort".into(), "ultrathink".into());
        *text = "please $review this patch".into();
    }
    let sent = claude.command(&start, "prompt", &[]).unwrap().outbound[0].clone();
    assert_eq!(
        sent["message"]["content"],
        json!([{"type":"text","text":"Ultrathink:\nplease"},{"type":"text","text":"/review this patch"}])
    );
    let model = claude
        .command(
            &ProviderCommand::SetModel {
                selection: ModelSelection {
                    instance: "claude".into(),
                    driver: Driver::Claude,
                    model: "claude-fable-5".into(),
                    options: [("contextWindow".to_string(), "1m".to_string())].into(),
                },
            },
            "",
            &[],
        )
        .unwrap();
    assert_eq!(model.outbound[0]["request"]["model"], "claude-fable-5[1m]");
}

fn routed(route: &str) -> WireContext {
    WireContext {
        route: route.into(),
        ..wire_context()
    }
}
/// Starts a turn of `route` on a shared translator; its native thread is `native`.
fn shared_start(codex: &mut CodexProtocol, route: &str, native: &str, turn: &str) {
    let start = codex
        .command(&codex_start(), &routed(route), &[])
        .unwrap()
        .outbound;
    let ready = codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":native}}}))
        .unwrap();
    assert_eq!(ready.route.as_deref(), Some(route));
    codex
        .receive(&json!({"id":ready.outbound[0]["id"],"result":{"turn":{"id":turn}}}))
        .unwrap();
}
// T3 CodexAdapterV2 serves every provider thread of a session from one
// app-server: native threads of different app threads share the id space and
// each notification belongs to the route of its native thread.
#[test]
fn one_app_server_routes_each_native_thread_to_its_own_route() {
    let mut codex = CodexProtocol::default();
    shared_start(&mut codex, "thread-a", "native-a", "turn-a");
    shared_start(&mut codex, "thread-b", "native-b", "turn-b");
    let delta = notify(
        &mut codex,
        "item/agentMessage/delta",
        json!({"threadId":"native-b","turnId":"turn-b","itemId":"m","delta":"hi"}),
    );
    assert_eq!(delta.route.as_deref(), Some("thread-b"));
    assert!(matches!(
        &delta.events[..],
        [ProviderEvent::TextDelta { .. }]
    ));
    let spawn = notify(
        &mut codex,
        "item/started",
        json!({"threadId":"native-a","turnId":"turn-a","item":{"type":"collabAgentToolCall","id":"spawn","tool":"spawnAgent","receiverThreadIds":["child-a"],"prompt":"look"}}),
    );
    assert_eq!(spawn.route.as_deref(), Some("thread-a"));
    let child = notify(
        &mut codex,
        "item/agentMessage/delta",
        json!({"threadId":"child-a","turnId":"child-turn","itemId":"c","delta":"x"}),
    );
    assert_eq!(child.route.as_deref(), Some("thread-a"));
    assert!(matches!(&child.events[..], [ProviderEvent::Child { key, .. }] if key == "child-a"));
    let unknown = notify(
        &mut codex,
        "item/agentMessage/delta",
        json!({"threadId":"elsewhere","turnId":"t","itemId":"u","delta":"x"}),
    );
    assert_eq!(unknown.route, None);
    let interrupt = codex
        .command(
            &ProviderCommand::Interrupt {
                native_thread: None,
                native_turn: None,
            },
            &routed("thread-b"),
            &[],
        )
        .unwrap();
    assert_eq!(interrupt.outbound[0]["method"], "turn/interrupt");
    assert_eq!(
        interrupt.outbound[0]["params"],
        json!({"threadId":"native-b","turnId":"turn-b"})
    );
    assert!(codex.turn_in_flight("thread-a") && codex.turn_in_flight("thread-b"));
    notify(
        &mut codex,
        "turn/completed",
        json!({"threadId":"native-b","turn":{"id":"turn-b","status":"interrupted"}}),
    );
    assert!(codex.turn_in_flight("thread-a") && !codex.turn_in_flight("thread-b"));
}
// T3 ProviderTurnStartService: a fork's thread is loaded for the child's first
// turn, a thread loaded for another selection or policy is resumed again, and
// a stop while starting suppresses only that route's prompt.
#[test]
fn a_shared_translator_loads_threads_per_route() {
    let mut codex = CodexProtocol::default();
    shared_start(&mut codex, "source", "native-source", "turn-source");
    notify(
        &mut codex,
        "turn/completed",
        json!({"threadId":"native-source","turn":{"id":"turn-source","status":"completed"}}),
    );
    let fork = codex
        .command(
            &ProviderCommand::Fork {
                native_thread: "native-source".into(),
                through_turn: Some("turn-source".into()),
            },
            &routed("source"),
            &[],
        )
        .unwrap();
    let forked = codex
        .receive(&json!({"id":fork.outbound[0]["id"],"result":{"thread":{"id":"native-fork"}}}))
        .unwrap();
    assert_eq!(
        forked.completion,
        Some(Completion::Forked {
            native_thread: "native-fork".into()
        })
    );
    let mut child_start = codex_start();
    if let ProviderCommand::Start { native_thread, .. } = &mut child_start {
        *native_thread = Some("native-fork".into());
    }
    let child = codex.command(&child_start, &routed("fork"), &[]).unwrap();
    assert_eq!(child.outbound[0]["method"], "turn/start");
    assert_eq!(child.outbound[0]["params"]["threadId"], "native-fork");
    let mut source_start = codex_start();
    if let ProviderCommand::Start {
        native_thread,
        runtime_mode,
        ..
    } = &mut source_start
    {
        *native_thread = Some("native-source".into());
        *runtime_mode = RuntimeMode::FullAccess;
    }
    let resumed = codex
        .command(&source_start, &routed("source"), &[])
        .unwrap();
    assert_eq!(resumed.outbound[0]["method"], "thread/resume");
    let other = codex
        .command(&codex_start(), &routed("other"), &[])
        .unwrap();
    assert_eq!(other.outbound[0]["method"], "thread/start");
    codex
        .command(
            &ProviderCommand::Interrupt {
                native_thread: None,
                native_turn: None,
            },
            &routed("other"),
            &[],
        )
        .unwrap();
    let source_ready = codex
        .receive(
            &json!({"id":resumed.outbound[0]["id"],"result":{"thread":{"id":"native-source"}}}),
        )
        .unwrap();
    assert_eq!(source_ready.outbound[0]["method"], "turn/start");
    let other_ready = codex
        .receive(&json!({"id":other.outbound[0]["id"],"result":{"thread":{"id":"native-other"}}}))
        .unwrap();
    assert!(other_ready.outbound.is_empty());
}
// T3 restoreAdditionalContext: compaction drops client developer messages, so
// the thread's additional context is injected again.
#[test]
fn a_compaction_restores_the_threads_additional_context() {
    let mut codex = CodexProtocol::default();
    let context = WireContext {
        additional_context: Some(Json(
            json!({"orchestration":{"kind":"application","value":"use the tools"}}),
        )),
        ..routed("thread")
    };
    let start = codex
        .command(&codex_start(), &context, &[])
        .unwrap()
        .outbound;
    codex
        .receive(&json!({"id":start[0]["id"],"result":{"thread":{"id":"native"}}}))
        .unwrap();
    let compacted = notify(
        &mut codex,
        "item/completed",
        json!({"threadId":"native","turnId":"turn","item":{"type":"contextCompaction","id":"compact"}}),
    );
    assert_eq!(compacted.outbound.len(), 1);
    assert_eq!(compacted.outbound[0]["method"], "thread/inject_items");
    assert_eq!(
        compacted.outbound[0]["params"],
        json!({"threadId":"native","items":[{"type":"message","role":"developer","content":[{"type":"input_text","text":"<orchestration>use the tools</orchestration>"}]}]})
    );
}
