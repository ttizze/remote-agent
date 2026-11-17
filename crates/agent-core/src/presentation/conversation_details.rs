//! Pure translation of the shared native details form into conversation-scoped operations.
use crate::{client::AgentError, state::Intent};
use agent_protocol::{automations as automation, pull_requests as pr, session::SessionRef};
use serde::Deserialize;

/// Both native clients send the same Encodable details action. The owner dispatches
/// the returned operations and keeps replies bound to its mounted conversation.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn conversation_details_intents(
    session: SessionRef,
    action_json: String,
) -> Result<Vec<Intent>, AgentError> {
    let action: Action = serde_json::from_str(&action_json)
        .map_err(|_| crate::client::error("会話の詳細の操作が正しくありません"))?;
    let action = match action {
        Action::ReadAutomations => automation::Action::List,
        Action::Refresh => {
            return Ok(vec![
                Intent::PullRequests(pr::Request {
                    session: session.clone(),
                    action: pr::Action::Read { refresh: true },
                }),
                Intent::Automations(automation::Request {
                    session,
                    action: automation::Action::List,
                }),
            ]);
        }
        Action::Link { url } => {
            return Ok(vec![Intent::PullRequests(pr::Request {
                session,
                action: pr::Action::Link { url },
            })]);
        }
        Action::Unlink { url } => {
            return Ok(vec![Intent::PullRequests(pr::Request {
                session,
                action: pr::Action::Unlink { url },
            })]);
        }
        Action::Save { definition: d } => automation::Action::Save {
            definition: Box::new(automation::Definition {
                id: d.id,
                session: session.clone(),
                title: d.title,
                prompt: d.prompt,
                model: d.model,
                effort: d.effort,
                service_tier: d.service_tier,
                schedule: d.schedule,
                enabled: d.enabled,
            }),
        },
        Action::RunNow { id } => automation::Action::RunNow { id },
        Action::Pause { id } => automation::Action::Pause { id },
        Action::Resume { id } => automation::Action::Resume { id },
        Action::Remove { id } => automation::Action::Remove { id },
        Action::Retry { id } => automation::Action::Retry { id },
    };
    Ok(vec![Intent::Automations(automation::Request {
        session,
        action,
    })])
}

// The form cannot choose the conversation. Insert the owner's session when constructing the operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Definition {
    id: uuid::Uuid,
    title: String,
    prompt: String,
    model: Option<agent_protocol::models::ModelRef>,
    effort: Option<String>,
    service_tier: Option<String>,
    schedule: automation::Schedule,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Action {
    Refresh,
    ReadAutomations,
    Link { url: String },
    Unlink { url: String },
    Save { definition: Definition },
    RunNow { id: uuid::Uuid },
    Pause { id: uuid::Uuid },
    Resume { id: uuid::Uuid },
    Remove { id: uuid::Uuid },
    Retry { id: uuid::Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_forms_preserve_each_provider_and_schedule_without_reinterpreting_values() {
        use agent_protocol::session::ProviderKind;
        let session = SessionRef::new(Some(ProviderKind::Codex), "selected".into()).unwrap();
        for (provider, expected_provider) in [
            ("codex", ProviderKind::Codex),
            ("claude", ProviderKind::Claude),
            ("deepseek", ProviderKind::DeepSeek),
        ] {
            for (schedule, expected) in [
                (
                    json!({"type":"once", "atMs":1700000000000_i64}),
                    automation::Schedule::Once {
                        at_ms: 1700000000000,
                    },
                ),
                (
                    json!({"type":"interval", "everyMs":60000}),
                    automation::Schedule::Interval { every_ms: 60000 },
                ),
                (
                    json!({"type":"fixedTime", "minuteOfDay":1380, "weekdays":[], "utcOffsetMinutes":-480}),
                    automation::Schedule::FixedTime {
                        minute_of_day: 1380,
                        weekdays: Vec::new(),
                        utc_offset_minutes: -480,
                    },
                ),
            ] {
                let action = json!({"kind":"save", "definition":{
                    "id":"d5560260-234f-4384-b87b-ff28d48a43da", "title":"Title", "prompt":"Prompt", "enabled":true,
                    "model":{"provider":provider,"id":"model"}, "schedule":schedule
                }});
                let operations =
                    conversation_details_intents(session.clone(), action.to_string()).unwrap();
                let [Intent::Automations(request)] = operations.as_slice() else {
                    panic!("one save operation is required")
                };
                let automation::Action::Save { definition } = &request.action else {
                    panic!("save action is required")
                };
                assert_eq!(request.session, session);
                assert_eq!(definition.session, session);
                assert_eq!(definition.schedule, expected);
                assert_eq!(
                    definition.model.as_ref().unwrap().provider,
                    expected_provider
                );
                assert_eq!(definition.model.as_ref().unwrap().id, "model");
                assert_eq!(definition.effort, None);
                assert_eq!(definition.service_tier, None);
                request.validate().unwrap();
            }
        }
    }

    #[test]
    fn malformed_native_actions_return_an_error_without_operations() {
        let session = SessionRef::new(None, "47ed573c-5c36-49e5-b276-c80c03ad60b6".into()).unwrap();
        for action in [
            "".into(),
            "{".into(),
            "null".into(),
            json!({"kind":"unknown"}).to_string(),
            json!({"kind":"pause"}).to_string(),
            json!({"kind":"pause","id":"not-a-uuid"}).to_string(),
            json!({"kind":"link"}).to_string(),
            json!({"kind":"save","definition":{}}).to_string(),
        ] {
            assert!(conversation_details_intents(session.clone(), action).is_err());
        }
    }
    #[test]
    fn saved_automation_uses_the_mounted_conversation_and_preserves_schedule_and_model() {
        let session = SessionRef::new(None, "47ed573c-5c36-49e5-b276-c80c03ad60b6".into()).unwrap();
        let action = json!({
            "kind": "save", "scope":"native-widget-scope", "definition": { "id": "d5560260-234f-4384-b87b-ff28d48a43da",
                "title": "Morning", "prompt": "Review changes", "enabled": false,
                "session":{"provider":"codex","id":"another-conversation"},
                "model": {"provider":"claude", "id":"review"}, "effort":"high", "serviceTier":null,
                "schedule":{"type":"fixedTime", "minuteOfDay":540, "weekdays":[1,5], "utcOffsetMinutes":540}
            }
        });
        let operations = conversation_details_intents(session.clone(), action.to_string()).unwrap();
        let [Intent::Automations(request)] = operations.as_slice() else {
            panic!()
        };
        request.validate().unwrap();
        let automation::Action::Save { definition } = &request.action else {
            panic!()
        };
        assert_eq!(definition.session, session);
        assert_eq!(
            definition.id.to_string(),
            "d5560260-234f-4384-b87b-ff28d48a43da"
        );
        assert_eq!(definition.model.as_ref().unwrap().id, "review");
        assert_eq!(definition.title, "Morning");
        assert_eq!(definition.prompt, "Review changes");
        assert_eq!(definition.effort.as_deref(), Some("high"));
        assert_eq!(definition.service_tier, None);
        assert_eq!(
            definition.schedule,
            automation::Schedule::FixedTime {
                minute_of_day: 540,
                weekdays: vec![1, 5],
                utc_offset_minutes: 540
            }
        );
        assert!(!definition.enabled);
    }

    #[test]
    fn details_actions_keep_the_selected_session_and_route_to_the_requested_operation() {
        let session = SessionRef::new(
            Some(agent_protocol::session::ProviderKind::Codex),
            "native-details".into(),
        )
        .unwrap();
        let id = "d5560260-234f-4384-b87b-ff28d48a43da";
        for kind in ["runNow", "pause", "resume", "remove", "retry"] {
            let action = json!({"kind":kind, "id":id});
            let operations =
                conversation_details_intents(session.clone(), action.to_string()).unwrap();
            let [Intent::Automations(request)] = operations.as_slice() else {
                panic!()
            };
            assert_eq!(request.session, session);
            assert_eq!(
                serde_json::to_value(&request.action).unwrap(),
                json!({"type":kind, "id":id})
            );
        }
        for kind in ["link", "unlink"] {
            let url = "https://github.com/acme/repo/pull/42";
            let action = json!({"kind":kind, "url":url});
            let operations =
                conversation_details_intents(session.clone(), action.to_string()).unwrap();
            let [Intent::PullRequests(request)] = operations.as_slice() else {
                panic!()
            };
            assert_eq!(request.session, session);
            assert_eq!(
                serde_json::to_value(&request.action).unwrap(),
                json!({"type":kind, "url":url})
            );
            request.validate().unwrap();
        }
        let operations = conversation_details_intents(
            session.clone(),
            json!({"kind":"readAutomations"}).to_string(),
        )
        .unwrap();
        let [Intent::Automations(request)] = operations.as_slice() else {
            panic!()
        };
        assert_eq!(request.session, session);
        assert!(matches!(request.action, automation::Action::List));
        let operations =
            conversation_details_intents(session.clone(), json!({"kind":"refresh"}).to_string())
                .unwrap();
        let [Intent::PullRequests(pr), Intent::Automations(automation)] = operations.as_slice()
        else {
            panic!()
        };
        assert_eq!(pr.session, session);
        assert_eq!(automation.session, session);
        assert!(matches!(pr.action, pr::Action::Read { refresh: true }));
        assert!(matches!(automation.action, automation::Action::List));
    }
}
