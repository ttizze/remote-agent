use super::*;
use agent_protocol::{
    requests::{Answer, ApprovalKind, Choice, ChoiceEffect, RequestBody, RequestTarget},
    session::RequestDelivery,
};

pub(super) fn body(permission: bool, description: &str, details: &str) -> RequestBody {
    let choices = [
        ("once", ChoiceEffect::Allow, "Use the requested resource"),
        (
            "session",
            ChoiceEffect::AllowLasting,
            "Use the requested resource",
        ),
        ("no", ChoiceEffect::Deny, "Leave access unchanged"),
    ]
    .into_iter()
    .map(|(id, effect, description)| Choice {
        id: id.into(),
        label: id.into(),
        description: description.into(),
        effect,
    })
    .collect();
    if permission {
        RequestBody::Permission {
            description: description.into(),
            details: details.into(),
            choices,
        }
    } else {
        RequestBody::Approval {
            kind: ApprovalKind::Command,
            description: description.into(),
            details: details.into(),
            choices,
        }
    }
}

#[test]
fn inline_approval_keeps_complete_provider_content_and_unique_scope_explanations() {
    for permission in [false, true] {
        for (delivery, status, can_respond) in [
            (RequestDelivery::Awaiting, "承認待ち", true),
            (RequestDelivery::Sending, "回答を送信中", false),
            (RequestDelivery::Sent, "回答を送信しました", false),
            (RequestDelivery::Unknown, "回答の配送結果が不明です", false),
        ] {
            let shown = request(
                UiLanguage::Japanese,
                &WireRequest {
                    id: "approval".into(),
                    target: RequestTarget::Session,
                    delivery,
                    body: body(permission, "Reason 日本語 {0}", "echo fixture\ncwd /repo"),
                },
            );
            assert_eq!(shown.can_respond, can_respond);
            let approval = shown.approval.unwrap();
            assert_eq!(
                approval.kind,
                if permission { "toolCall" } else { "command" }
            );
            assert_eq!(approval.detail, "Reason 日本語 {0}");
            assert_eq!(approval.details, "echo fixture\ncwd /repo");
            assert_eq!(approval.status, status);
            assert_eq!(
                approval.notes,
                ["Use the requested resource", "Leave access unchanged"]
            );
            assert_eq!(approval.actions.approve.unwrap().choice_id, "once");
            assert_eq!(approval.actions.decline.unwrap().choice_id, "no");
            assert_eq!(
                approval
                    .actions
                    .more
                    .iter()
                    .map(|action| action.choice_id.as_str())
                    .collect::<Vec<_>>(),
                ["session"]
            );
        }
    }
}

#[test]
fn approval_detail_falls_back_to_complete_command_then_the_kind_label() {
    for (reason, details, expected) in [
        ("", "echo fixture\ncwd /repo", "echo fixture\ncwd /repo"),
        (" \n", "", "Command approval"),
        ("same", "same", "same"),
    ] {
        let shown = request(
            UiLanguage::English,
            &WireRequest {
                id: "approval".into(),
                target: RequestTarget::Session,
                delivery: RequestDelivery::Awaiting,
                body: body(false, reason, details),
            },
        )
        .approval
        .unwrap();
        assert_eq!(shown.detail, expected);
        assert!(
            shown.details.is_empty(),
            "do not repeat the command when it is already the visible detail"
        );
    }
}

#[test]
fn offered_decisions_keep_the_wire_answer_kind_and_reject_foreign_choices() {
    for permission in [false, true] {
        let body = body(permission, "reason", "details");
        for id in ["once", "session", "no"] {
            let expected = if permission {
                Answer::Permission {
                    choice_id: id.into(),
                }
            } else {
                Answer::Approval {
                    choice_id: id.into(),
                }
            };
            assert_eq!(
                request_choice_answer(body.clone(), id.into()),
                Some(expected)
            );
        }
        for id in ["", "foreign", "Once"] {
            assert!(request_choice_answer(body.clone(), id.into()).is_none());
        }
    }
    assert!(
        request_choice_answer(RequestBody::Question { questions: vec![] }, "once".into()).is_none()
    );
}
