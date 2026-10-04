use agent_protocol::{protocol, requests::*, session::RequestDelivery};
use proptest::prelude::*;
use serde_json::{Value, json};

fn body(value: Value) -> RequestBody {
    serde_json::from_value(value).unwrap()
}

#[test]
fn choices_are_ids_and_answers_must_match_the_request_family() {
    let choice = json!({"id":"opaque","label":"Allow","description":"turn permission"});
    let approval = body(
        json!({"approval":{"kind":"command","description":"run","details":"pwd","choices":[choice]}}),
    );
    let permission =
        body(json!({"permission":{"description":"network","details":"host","choices":[choice]}}));
    let allow = Answer::Approval {
        choice_id: "opaque".into(),
    };
    let grant = Answer::Permission {
        choice_id: "opaque".into(),
    };
    assert!(validate_answer(&approval, &allow).is_ok());
    assert!(validate_answer(&permission, &grant).is_ok());
    assert!(validate_answer(&approval, &grant).is_err());
    assert!(validate_answer(&permission, &allow).is_err());
    for choice_id in ["", "Allow", "0", "native-accept"] {
        assert!(
            validate_answer(
                &approval,
                &Answer::Approval {
                    choice_id: choice_id.into()
                }
            )
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<Answer>(json!({"raw":{"value":{"decision":"accept"}}})).is_err()
    );
}

#[test]
fn questions_keep_free_text_single_and_multiple_choices_distinct() {
    let questions = body(json!({"question":{"questions":[
        {"id":"single","header":"one","prompt":"choose one","secret":false,"allowFreeText":false,"multiple":false,"choices":[{"id":"s","label":"One","description":""}]},
        {"id":"multiple","header":"many","prompt":"choose many","secret":false,"allowFreeText":true,"multiple":true,"choices":[{"id":"a","label":"Alpha","description":""},{"id":"b","label":"Beta","description":""}]},
        {"id":"text","header":"secret","prompt":"enter text","secret":true,"allowFreeText":true,"multiple":false,"choices":[]}
    ]}}));
    let answers = [
        (
            "single".into(),
            QuestionAnswer::SingleChoice {
                choice_id: "s".into(),
            },
        ),
        (
            "multiple".into(),
            QuestionAnswer::MultipleChoices {
                choice_ids: vec!["a".into(), "b".into()],
            },
        ),
        (
            "text".into(),
            QuestionAnswer::FreeText {
                text: "answer".into(),
            },
        ),
    ]
    .into();
    let valid = Answer::Questions { answers };
    assert!(validate_answer(&questions, &valid).is_ok());
    for (id, invalid) in [
        ("single", QuestionAnswer::FreeText { text: "One".into() }),
        (
            "single",
            QuestionAnswer::MultipleChoices {
                choice_ids: vec!["s".into()],
            },
        ),
        (
            "multiple",
            QuestionAnswer::SingleChoice {
                choice_id: "a".into(),
            },
        ),
        (
            "multiple",
            QuestionAnswer::MultipleChoices {
                choice_ids: vec!["a".into(), "a".into()],
            },
        ),
        (
            "multiple",
            QuestionAnswer::MultipleChoices { choice_ids: vec![] },
        ),
        (
            "multiple",
            QuestionAnswer::MultipleChoices {
                choice_ids: vec!["Alpha".into()],
            },
        ),
        ("text", QuestionAnswer::FreeText { text: " \n".into() }),
    ] {
        let Answer::Questions { mut answers } = valid.clone() else {
            unreachable!()
        };
        answers.insert(id.into(), invalid);
        assert!(validate_answer(&questions, &Answer::Questions { answers }).is_err());
    }
    let Answer::Questions { mut answers } = valid else {
        unreachable!()
    };
    answers.remove("text");
    assert!(validate_answer(&questions, &Answer::Questions { answers }).is_err());
}

#[test]
fn form_and_url_answers_cannot_bypass_field_constraints() {
    let fields: Vec<FormField> = serde_json::from_value(json!([
        {"name":"name","title":"name","description":"","required":true,"input":{"string":{"minLength":1,"maxLength":3,"format":null,"default":null}}},
        {"name":"count","title":"count","description":"","required":true,"input":{"number":{"integer":true,"minimum":1,"maximum":5,"default":null}}},
        {"name":"flag","title":"flag","description":"","required":false,"input":{"boolean":{"default":null}}},
        {"name":"one","title":"one","description":"","required":false,"input":{"choice":{"choices":[{"value":"value","title":"Title"}],"default":null}}},
        {"name":"many","title":"many","description":"","required":false,"input":{"multiple":{"choices":[{"value":"a","title":"A"},{"value":"b","title":"B"}],"minItems":1,"maxItems":2,"default":[]}}}
    ])).unwrap();
    let form = RequestBody::Elicitation {
        server: "mcp".into(),
        message: "input".into(),
        input: ElicitationInput::Form { fields },
    };
    let valid = json!({"name":"日本語","count":3,"flag":false,"one":"value","many":["a","b"]});
    assert!(
        validate_answer(
            &form,
            &Answer::Elicitation {
                action: ElicitationAnswer::Accept {
                    values: valid.clone()
                }
            }
        )
        .is_ok()
    );
    for (name, value) in [
        ("name", json!("")),
        ("name", json!("日本語日本")),
        ("name", json!(1)),
        ("count", json!(0)),
        ("count", json!(6)),
        ("count", json!(1.5)),
        ("count", json!("3")),
        ("flag", json!(0)),
        ("one", json!("Title")),
        ("many", json!([])),
        ("many", json!(["a", "a"])),
        ("many", json!(["A"])),
        ("many", json!(["a", "b", "a"])),
        ("extra", json!(true)),
    ] {
        let mut values = valid.clone();
        values[name] = value;
        assert!(
            validate_answer(
                &form,
                &Answer::Elicitation {
                    action: ElicitationAnswer::Accept { values }
                }
            )
            .is_err(),
            "{name}"
        );
    }
    for values in [json!({"name":"ok"}), Value::Null, json!([])] {
        assert!(
            validate_answer(
                &form,
                &Answer::Elicitation {
                    action: ElicitationAnswer::Accept { values }
                }
            )
            .is_err()
        );
    }
    let url = RequestBody::Elicitation {
        server: "mcp".into(),
        message: "login".into(),
        input: ElicitationInput::Url {
            url: "https://example.com/".into(),
        },
    };
    for action in [
        ElicitationAnswer::Accept {
            values: Value::Null,
        },
        ElicitationAnswer::Decline,
        ElicitationAnswer::Cancel,
    ] {
        assert!(
            validate_answer(
                &url,
                &Answer::Elicitation {
                    action: action.clone()
                }
            )
            .is_ok()
        );
        if !matches!(action, ElicitationAnswer::Accept { .. }) {
            assert!(validate_answer(&form, &Answer::Elicitation { action }).is_ok());
        }
    }
    assert!(
        validate_answer(
            &url,
            &Answer::Elicitation {
                action: ElicitationAnswer::Accept { values: json!({}) }
            }
        )
        .is_err()
    );
}

#[test]
fn requests_and_specialized_tool_answers_round_trip_through_postcard() {
    let request = agent_protocol::requests::Request {
        id: "request".into(),
        target: RequestTarget::Turn {
            turn_id: "turn".into(),
            item_id: Some("item".into()),
        },
        delivery: RequestDelivery::Sent,
        body: RequestBody::ToolExecution {
            tool: "tool".into(),
            namespace: Some("namespace".into()),
            arguments: json!({"number":1,"array":[false,null,"text"]}),
        },
    };
    assert_eq!(
        protocol::decode::<agent_protocol::requests::Request>(&protocol::encode(&request).unwrap())
            .unwrap(),
        request
    );
    for success in [true, false] {
        let answer = Answer::ToolExecution {
            success,
            content: vec![
                ToolContent::Text {
                    text: "result".into(),
                },
                ToolContent::Image {
                    data_url: "data:image/png;base64,aGVsbG8=".into(),
                },
            ],
        };
        assert!(validate_answer(&request.body, &answer).is_ok());
        assert_eq!(
            protocol::decode::<Answer>(&protocol::encode(&answer).unwrap()).unwrap(),
            answer
        );
    }
    for data_url in [
        "https://example.com/image.png",
        "data:text/plain;base64,eA==",
        "data:image/png,raw",
    ] {
        assert!(
            validate_answer(
                &request.body,
                &Answer::ToolExecution {
                    success: true,
                    content: vec![ToolContent::Image {
                        data_url: data_url.into()
                    }]
                }
            )
            .is_err()
        );
    }
}

#[test]
fn string_formats_validate_dates_and_uris() {
    for (format, valid, invalid) in [
        (
            StringFormat::Email,
            "person@example.com",
            "person example.com",
        ),
        (StringFormat::Uri, "https://example.com/path", "not a uri"),
        (StringFormat::Date, "2024-02-29", "2023-02-29"),
        (
            StringFormat::DateTime,
            "2026-09-30T12:34:56+09:00",
            "2026-09-30",
        ),
    ] {
        let field = FormInput::String {
            min_length: None,
            max_length: None,
            format: Some(format),
            default: None,
        };
        assert!(field.accepts(&json!(valid)));
        assert!(!field.accepts(&json!(invalid)));
    }
}

#[test]
fn fractional_numbers_and_selection_bounds_are_independent_constraints() {
    let number = FormInput::Number {
        integer: false,
        minimum: Some(1.),
        maximum: Some(5.),
        default: None,
    };
    assert!(number.accepts(&json!(1.5)));
    assert!(!number.accepts(&json!(0.5)));
    assert!(!number.accepts(&json!(5.5)));
    let selection = FormInput::Multiple {
        choices: ["a", "b", "c"]
            .map(|value| FormChoice {
                value: value.into(),
                title: value.into(),
            })
            .into(),
        min_items: Some(2),
        max_items: Some(2),
        default: vec![],
    };
    assert!(selection.accepts(&json!(["a", "b"])));
    assert!(!selection.accepts(&json!(["a"])));
    assert!(!selection.accepts(&json!(["a", "b", "c"])));
}

proptest! {
    #[test]
    fn integer_form_bounds_remain_inclusive(value in -100i32..200) {
        let field = FormInput::Number { integer: true, minimum: Some(1.), maximum: Some(100.), default: None };
        prop_assert_eq!(field.accepts(&json!(value)), (1..=100).contains(&value));
        prop_assert!(!field.accepts(&json!(f64::from(value) + 0.5)));
    }
    #[test]
    fn unicode_form_length_counts_characters(value in prop::collection::vec(any::<char>(), 0..10)) {
        let field = FormInput::String { min_length: Some(2), max_length: Some(5), format: None, default: None };
        let text: String = value.iter().collect();
        prop_assert_eq!(field.accepts(&json!(text)), (2..=5).contains(&value.len()));
    }
}
