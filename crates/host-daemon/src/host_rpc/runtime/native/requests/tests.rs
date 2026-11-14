use super::*;
use agent_protocol::requests::*;

fn pending(body: RequestBody) -> resources::PendingRequest {
    resources::PendingRequest {
        target: SessionRef::new(None, "thread".into()).unwrap(),
        attempt: Attempt {
            id: Uuid::new_v4(),
            turn: "turn".into(),
            provider: ProviderKind::Codex,
            source: "native".into(),
        },
        origin: RequestOrigin {
            instance: Uuid::new_v4(),
            native_id: json!(1),
            provider: ProviderKind::Codex,
            source: Arc::new(super::super::super::native_fixture::AnswerResource),
        },
        body,
    }
}
#[test]
fn tool_result_is_reconstructed_from_the_canonical_response_payload() {
    let pending = pending(RequestBody::ToolExecution {
        tool: "test".into(),
        namespace: None,
        arguments: json!({}),
    });
    // The native callback accepts the typed ToolContent representation.
    let content = vec![ToolContent::Text {
        text: "done".into(),
    }];
    let answers = std::collections::BTreeMap::from([(
        "result".into(),
        json!({"success":true,"content":content}),
    )]);
    assert_eq!(
        native_answer(&pending, None, Some(&answers)).unwrap(),
        Answer::ToolExecution {
            success: true,
            content
        }
    );
}
#[test]
fn skip_is_delivered_and_choices_use_native_ids_only_after_label_validation() {
    let pending = pending(RequestBody::Question {
        questions: vec![Question {
            id: "question".into(),
            header: "Header".into(),
            prompt: "Choose".into(),
            secret: false,
            allow_free_text: true,
            multiple: true,
            choices: vec![QuestionChoice {
                id: "opaque-native-choice".into(),
                label: "First".into(),
                description: String::new(),
            }],
        }],
    });
    assert_eq!(
        native_answer(&pending, None, Some(&Default::default())).unwrap(),
        Answer::SkipQuestions
    );
    let answers = std::collections::BTreeMap::from([("question".into(), json!(["First"]))]);
    let answer = native_answer(&pending, None, Some(&answers)).unwrap();
    validate_answer(&pending.body, &answer).unwrap();
    assert!(
        matches!(answer,Answer::Questions {answers} if answers["question"]==QuestionAnswer::MultipleChoices {choice_ids:vec!["opaque-native-choice".into()]})
    );
    let invalid = std::collections::BTreeMap::from([("question".into(), json!(["missing"]))]);
    assert!(native_answer(&pending, None, Some(&invalid)).is_err());
    let text =
        std::collections::BTreeMap::from([("question".into(), json!("opaque-native-choice"))]);
    assert!(
        matches!(native_answer(&pending,None,Some(&text)).unwrap(),Answer::Questions {answers} if answers["question"]==QuestionAnswer::FreeText {text:"opaque-native-choice".into()})
    );
}
