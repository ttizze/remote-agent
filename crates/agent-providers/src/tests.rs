use crate::*;
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
    let answers = Answers::from([(questions[0].id.clone(), vec!["Compact and continue".into()])]);
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
