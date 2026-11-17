use super::*;
use agent_protocol::requests::*;

fn pending(body: RequestBody) -> resources::PendingRequest {
    resources::PendingRequest {
        target: SessionRef::new(None, "thread".into()).unwrap(),
        native: SessionRef::new(ProviderKind::Codex, "native".into()).unwrap(),
        attempt_id: Some(Uuid::new_v4()),
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
                preview: None,
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

#[derive(Default)]
struct Answers {
    values: Mutex<Vec<(serde_json::Value, Answer)>>,
    changed: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl crate::providers::AnswerSource for Answers {
    fn is_alive(&self) -> bool {
        true
    }
    async fn prepare(
        &self,
        id: &serde_json::Value,
        body: &RequestBody,
        answer: &Answer,
    ) -> Result<crate::providers::AnswerWrite, Failure> {
        validate_answer(body, answer).map_err(|error| Failure::new("invalid_answer", error))?;
        self.values
            .lock()
            .unwrap()
            .push((id.clone(), answer.clone()));
        self.changed.notify_one();
        Ok(Box::pin(async { Ok(()) }))
    }
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn native_requests_keep_their_real_scope_and_typed_answer_callback_without_an_application_run(
    #[case] turn_scoped: bool,
) {
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, history, own};
    use agent_protocol::{execution::SessionStatus, models};
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let native = SessionRef::new(ProviderKind::Codex, "native-request-owner".into()).unwrap();
    let response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(native.clone()),
            cwd: Some(root.path().to_str().unwrap().into()),
            status: SessionStatus::Idle,
            turns: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .insert(native.id.clone(), response.clone());
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    let _client = service.open_session();
    own(&service, &native, &response).await;
    Runtime::ensure_imported(&service, &native).await.unwrap();
    let resource = Arc::new(Answers::default());
    let origin = RequestOrigin {
        instance: Uuid::new_v4(),
        native_id: json!(17),
        provider: ProviderKind::Codex,
        source: resource.clone(),
    };
    let body = RequestBody::Elicitation {
        server: "native-mcp".into(),
        message: "Choose an integer".into(),
        input: ElicitationInput::Form {
            fields: vec![FormField {
                name: "count".into(),
                title: "Count".into(),
                description: "Between two and four".into(),
                required: true,
                input: FormInput::Number {
                    integer: true,
                    minimum: Some(2.0),
                    maximum: Some(4.0),
                    default: Some(3.0),
                },
            }],
        },
    };
    let request = Request {
        id: "sdk-request".into(),
        target: if turn_scoped {
            RequestTarget::Turn {
                turn_id: "real-native-turn".into(),
                item_id: Some("real-request-item".into()),
            }
        } else {
            RequestTarget::Session
        },
        delivery: agent_protocol::session::RequestDelivery::Awaiting,
        body: body.clone(),
    };
    let event = || AgentChange::Request {
        session: native.clone(),
        origin: origin.clone(),
        request: request.clone(),
    };
    Runtime::observe(&service, event(), Some(Uuid::new_v4()))
        .await
        .unwrap();
    let storage = &service.inner.conversation.storage;
    let before = storage.thread_stream(&native, None).unwrap();
    Runtime::observe(&service, event(), Some(Uuid::new_v4()))
        .await
        .unwrap();
    assert_eq!(
        before,
        storage.thread_stream(&native, None).unwrap(),
        "a repeated native callback does not allocate a second request or node"
    );
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert!(state.runs.is_empty());
    assert!(state.run_attempts.is_empty());
    assert!(state.effects.is_empty());
    assert_eq!(state.provider_turns.len(), usize::from(turn_scoped));
    assert_eq!(state.runtime_requests.len(), 1);
    let saved = state.runtime_requests.values().next().unwrap();
    let model::ResponseCapability::Live {
        provider_session_id,
        ..
    } = &saved.response_capability
    else {
        panic!("live native callback")
    };
    assert_eq!(
        state
            .selected_native_thread()
            .unwrap()
            .provider_session_id
            .as_deref(),
        Some(provider_session_id.as_str())
    );
    assert!(
        state
            .attached_provider_sessions
            .contains(provider_session_id)
    );

    let view = history(&service, &native).unwrap();
    let shown = &view.thread.requests[saved.id.as_str()];
    assert_eq!(shown.body, body);
    match &shown.target {
        RequestTarget::Session => assert!(!turn_scoped),
        RequestTarget::Turn { turn_id, .. } => {
            assert!(turn_scoped);
            assert_eq!(turn_id.as_str(), saved.provider_turn_id.as_deref().unwrap());
            assert_ne!(turn_id.as_str(), "real-native-turn");
        }
    }
    let answer = Answer::Elicitation {
        action: ElicitationAnswer::Accept {
            values: json!({"count":3}),
        },
    };
    Runtime::change(
        &service,
        &records::Change {
            session: native.clone(),
            operation: Uuid::new_v4(),
            command: Command::Answer {
                id: saved.id.clone().into(),
                answer: answer.clone(),
            },
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        resource.changed.notified(),
    )
    .await
    .unwrap();
    assert_eq!(*resource.values.lock().unwrap(), [(json!(17), answer)]);
    let state = storage.thread_state(&native.application_id(), &[]).unwrap();
    assert_eq!(
        state.runtime_requests[&saved.id].status,
        model::RequestStatus::Resolved
    );
    assert!(state.runs.is_empty());
    assert!(state.run_attempts.is_empty());
    assert!(agent.inputs.lock().unwrap().is_empty());
    let pending_resource = Arc::new(super::super::super::native_fixture::AnswerResource);
    let retained = Arc::downgrade(&pending_resource);
    let next_origin = RequestOrigin {
        instance: origin.instance,
        native_id: json!(18),
        provider: ProviderKind::Codex,
        source: pending_resource,
    };
    Runtime::observe(
        &service,
        AgentChange::Request {
            session: native.clone(),
            origin: next_origin,
            request: Request {
                id: "second-native-request".into(),
                target: request.target.clone(),
                delivery: request.delivery,
                body: RequestBody::ToolExecution {
                    tool: "inspect".into(),
                    namespace: None,
                    arguments: json!({}),
                },
            },
        },
        None,
    )
    .await
    .unwrap();
    assert!(retained.upgrade().is_some());
    let pending_state = storage.thread_state(&native.application_id(), &[]).unwrap();
    let pending_request = pending_state
        .runtime_requests
        .values()
        .find(|request| {
            request
                .native_request_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("18")
        })
        .unwrap();
    let expired_item = pending_state
        .request_item(&pending_request.id)
        .unwrap()
        .id
        .clone();
    Runtime::observe(&service, AgentChange::SourceClosed(origin.instance), None)
        .await
        .unwrap();
    assert!(
        retained.upgrade().is_none(),
        "closing a native source releases its callback without fabricating a run"
    );
    let closed = storage.thread_state(&native.application_id(), &[]).unwrap();
    let expired = closed
        .runtime_requests
        .values()
        .find(|request| {
            request
                .native_request_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("18")
        })
        .unwrap();
    assert_eq!(expired.status, model::RequestStatus::Expired);
    assert_eq!(
        closed.nodes[&expired.node_id].status,
        model::NodeStatus::Cancelled
    );
    assert_eq!(
        storage
            .thread_item(&native, &expired_item)
            .unwrap()
            .unwrap()
            .status,
        model::TurnItemStatus::Cancelled
    );
    let final_resource = Arc::new(super::super::super::native_fixture::AnswerResource);
    let final_retained = Arc::downgrade(&final_resource);
    let final_instance = Uuid::new_v4();
    Runtime::observe(
        &service,
        AgentChange::Request {
            session: native.clone(),
            origin: RequestOrigin {
                instance: final_instance,
                native_id: json!(19),
                provider: ProviderKind::Codex,
                source: final_resource,
            },
            request: Request {
                id: "last-native-request".into(),
                target: request.target.clone(),
                delivery: request.delivery,
                body: RequestBody::ToolExecution {
                    tool: "inspect".into(),
                    namespace: None,
                    arguments: json!({}),
                },
            },
        },
        None,
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native.clone(),
            change: SessionChange::Turn {
                turn: models::Turn {
                    id: "real-native-turn".into(),
                    status: agent_protocol::execution::TurnStatus::Completed,
                    items: None,
                    ..Default::default()
                },
                completed: true,
            },
        },
        None,
    )
    .await
    .unwrap();
    let ended = storage.thread_state(&native.application_id(), &[]).unwrap();
    let last = ended
        .runtime_requests
        .values()
        .find(|request| {
            request
                .native_request_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("19")
        })
        .unwrap();
    assert_eq!(
        last.status,
        if turn_scoped {
            model::RequestStatus::Expired
        } else {
            model::RequestStatus::Pending
        }
    );
    assert_eq!(final_retained.upgrade().is_none(), turn_scoped);
    Runtime::observe(
        &service,
        AgentChange::Resolved {
            instance: final_instance,
            native_id: json!(19),
        },
        None,
    )
    .await
    .unwrap();
    assert!(final_retained.upgrade().is_none());

    let before_rejected_answer = storage.thread_stream(&native, None).unwrap();
    let rejected = Runtime::change(
        &service,
        &records::Change {
            session: native.clone(),
            operation: Uuid::new_v4(),
            command: Command::Answer {
                id: saved.id.clone().into(),
                answer: Answer::Elicitation {
                    action: ElicitationAnswer::Cancel,
                },
            },
        },
    )
    .await
    .unwrap();
    // Command rejection is a persisted acknowledgement, not a transport error.
    assert_eq!(
        rejected.error.as_deref(),
        Some("request is no longer pending")
    );
    assert_eq!(rejected.accepted().unwrap_err().code, "command_rejected");
    assert_eq!(
        before_rejected_answer,
        storage.thread_stream(&native, None).unwrap()
    );
    assert_eq!(
        resource.values.lock().unwrap().len(),
        1,
        "a resolved callback must not receive another answer"
    );
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
#[tokio::test]
async fn a_native_question_keeps_its_password_field_hint_through_storage_and_callback_loss(
    #[case] secret: bool,
) {
    use crate::host_rpc::runtime::native_fixture::{AnswerResource, NativeAgent, history, own};
    use agent_protocol::models;
    let root = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let native = SessionRef::new(ProviderKind::Codex, "native-question-owner".into()).unwrap();
    let response = models::ThreadResponse {
        thread: models::Thread {
            id: Some(native.clone()),
            cwd: Some(root.path().to_str().unwrap().into()),
            turns: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    };
    agent
        .records
        .lock()
        .unwrap()
        .insert(native.id.clone(), response.clone());
    let service = HostRpcService::new([agent.backend()], root.path().join("projects.json"));
    own(&service, &native, &response).await;
    Runtime::ensure_imported(&service, &native).await.unwrap();
    let params = json!({"threadId":native.id,"turnId":"question-turn","itemId":"question-item","questions":[{"id":"private-field","header":"Input","question":"Enter a value","isSecret":secret,"options":[]}]});
    let adapted = crate::providers::requests::codex(
        "native-question".into(),
        "item/tool/requestUserInput",
        &params,
    )
    .unwrap();
    let instance = Uuid::new_v4();
    Runtime::observe(
        &service,
        AgentChange::Request {
            session: native.clone(),
            origin: RequestOrigin {
                instance,
                native_id: json!(27),
                provider: ProviderKind::Codex,
                source: Arc::new(AnswerResource),
            },
            request: adapted.request,
        },
        None,
    )
    .await
    .unwrap();
    for closed in [false, true] {
        if closed {
            Runtime::observe(&service, AgentChange::SourceClosed(instance), None)
                .await
                .unwrap();
        }
        let view = history(&service, &native).unwrap();
        assert_eq!(view.thread.requests.len(), 1);
        let RequestBody::Question { questions } =
            &view.thread.requests.values().next().unwrap().body
        else {
            panic!("question card")
        };
        assert_eq!(questions.len(), 1);
        assert_eq!(
            questions[0].secret, secret,
            "native clients use this hint to mask the input, including a question answered later by message"
        );
        let state = service
            .inner
            .conversation
            .storage
            .thread_state(&native.application_id(), &[])
            .unwrap();
        let request = state.runtime_requests.values().next().unwrap();
        let serialized =
            serde_json::to_value(&state.request_item(&request.id).unwrap().body).unwrap();
        assert_eq!(
            serialized["questions"][0].get("isSecret"),
            secret.then_some(&serde_json::Value::Bool(true)),
            "ordinary questions retain the pinned T3 fields; only a native password hint adds isSecret"
        );
        assert!(state.runs.is_empty());
        assert!(state.run_attempts.is_empty());
        assert!(state.effects.is_empty());
    }
}
