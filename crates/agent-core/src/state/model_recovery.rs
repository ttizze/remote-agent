use super::*;
use crate::models::{ItemBody, ItemContent, MessagePart, TurnStatus};

impl Snapshot {
    /// A provider session cannot move between backends. Recover an initial
    /// failed input as an editable local draft, without sending it automatically.
    pub(crate) fn recoverable_model_draft(
        &self,
        session: &crate::session::SessionRef,
    ) -> Option<Draft> {
        if crate::presentation::list::is_coordinator_session(session)
            || self.activity.active.get(session) == Some(&true)
            || self
                .pending_submissions
                .values()
                .any(|pending| pending.draft_key == DraftKey::from(session))
        {
            return None;
        }
        let thread = self.conversations.get(session)?;
        if thread.history_has_more == Some(true)
            || thread.status == crate::models::SessionStatus::Running
            || !thread.requests.is_empty()
        {
            return None;
        }
        let turns = thread.turns.as_ref()?;
        let [turn] = turns.as_slice() else {
            return None;
        };
        if turn.status != TurnStatus::Failed || turn.items_summary {
            return None;
        }
        let mut recovered = None;
        for item in turn.items.as_ref()? {
            let ItemContent::Inline { body } = &item.body else {
                return None;
            };
            match body.as_ref() {
                ItemBody::UserMessage { text, content } if recovered.is_none() => {
                    let mut draft = Draft {
                        text: text.clone().unwrap_or_default(),
                        ..Default::default()
                    };
                    for part in content {
                        match part {
                            MessagePart::Text { text: part } if text.is_none() => {
                                draft.text.push_str(part)
                            }
                            MessagePart::Text { .. } => {}
                            MessagePart::Image { source } => {
                                // Remote/data images cannot be submitted as local files.
                                if source.starts_with("data:") || source.contains("://") {
                                    return None;
                                }
                                draft.attachments.push(Attachment {
                                    path: source.clone(),
                                    name: source
                                        .rsplit(['/', '\\'])
                                        .next()
                                        .unwrap_or(source)
                                        .to_owned(),
                                    is_image: true,
                                });
                            }
                            MessagePart::Attachment { name, path } => {
                                draft.attachments.push(Attachment {
                                    path: path.clone(),
                                    name: name.clone(),
                                    is_image: false,
                                })
                            }
                            MessagePart::Invocation { .. } => return None,
                        }
                    }
                    recovered = Some(draft);
                }
                ItemBody::AssistantText { text, .. } if text.is_empty() => {}
                ItemBody::Reasoning { content, summary }
                    if content.iter().chain(summary).all(String::is_empty) => {}
                _ => return None,
            }
        }
        recovered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};
    use serde_json::json;

    fn fixture() -> (Snapshot, SessionRef) {
        let id = SessionRef {
            provider: ProviderKind::DeepSeek,
            id: "failed-start".into(),
        };
        let thread = serde_json::from_value(json!({
            "id":id, "historyHasMore":false, "turns":[{
                "id":"first", "status":"failed", "items":[{
                    "id":"input", "body":{"inline":{"body":{"userMessage":{
                        "text":null, "content":[{"text":{"text":"Please fix this"}},
                        {"image":{"source":"C:\\images\\screen.png"}},
                        {"attachment":{"name":"notes.txt","path":"C:\\notes.txt"}}]
                    }}}}
                }]
            }]
        }))
        .unwrap();
        let mut snapshot = Snapshot::default();
        snapshot.navigation = Arc::new(Navigation {
            thread_id: Some(id.clone()),
            draft_key: (&id).into(),
            cwd: "C:\\project".into(),
        });
        Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(thread));
        (snapshot, id)
    }

    #[test]
    fn switching_initial_failed_provider_recovers_input_without_sending_or_changing_history() {
        let (mut snapshot, id) = fixture();
        let key = DraftKey::from(&id);
        Arc::make_mut(&mut snapshot.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "Additional detail".into(),
                ..Default::default()
            }),
        );
        assert!(snapshot.can_change_provider(key.clone()));
        let model = crate::models::ModelRef {
            provider: ProviderKind::Codex,
            id: "replacement".into(),
        };
        let (next, effects) = reduce_intent(
            &snapshot,
            Intent::SelectModel {
                thread_id: key.clone(),
                model: model.clone(),
            },
        );
        assert!(effects.is_empty());
        assert!(next.navigation.thread_id.is_none());
        assert_eq!(next.navigation.cwd, snapshot.navigation.cwd);
        let draft = &next.drafts[&next.navigation.draft_key];
        assert!(draft.text.starts_with("Please fix this"));
        assert!(draft.text.ends_with("Additional detail"));
        assert_eq!(draft.attachments.len(), 2);
        assert_eq!(draft.attachments[0].path, "C:\\images\\screen.png");
        assert!(draft.attachments[0].is_image);
        assert_eq!(draft.attachments[1].name, "notes.txt");
        assert_eq!(draft.model.as_ref(), Some(&model));
        assert_eq!(next.conversations, snapshot.conversations);
        assert_eq!(next.drafts[&key], snapshot.drafts[&key]);
        assert_eq!(next.model_defaults, snapshot.model_defaults);
    }

    #[test]
    fn pending_or_active_work_and_unknown_history_do_not_offer_recovery() {
        let (snapshot, id) = fixture();
        for change in [
            "active",
            "pending",
            "missing-turns",
            "missing-items",
            "remote-image",
        ] {
            let mut next = snapshot.clone();
            match change {
                "active" => {
                    Arc::make_mut(&mut next.activity)
                        .active
                        .insert(id.clone(), true);
                }
                "pending" => {
                    Arc::make_mut(&mut next.pending_submissions).insert(
                        "pending".into(),
                        Arc::new(PendingSubmission {
                            sequence: 1,
                            draft_key: (&id).into(),
                            draft: Arc::default(),
                            turn_id: None,
                            after_item_id: None,
                            accepted: false,
                            delivery_unknown: true,
                        }),
                    );
                }
                change => {
                    let thread =
                        Arc::make_mut(Arc::make_mut(&mut next.conversations).get_mut(&id).unwrap());
                    if change == "missing-turns" {
                        thread.turns = None;
                    } else {
                        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]);
                        if change == "missing-items" {
                            turn.items = None;
                        } else {
                            let item = Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]);
                            if let ItemBody::UserMessage { content, .. } = item.body_mut() {
                                content.push(MessagePart::Image {
                                    source: "https://example.invalid/image.png".into(),
                                });
                            }
                        }
                    }
                }
            }
            assert!(!next.can_change_provider((&id).into()), "{change}");
        }
    }

    #[test]
    fn provider_stays_locked_after_execution_or_with_incomplete_history() {
        let (snapshot, id) = fixture();
        for change in [
            "completed",
            "running",
            "history",
            "summary",
            "assistant",
            "tool",
            "deferred",
            "earlier",
        ] {
            let mut next = snapshot.clone();
            let thread =
                Arc::make_mut(Arc::make_mut(&mut next.conversations).get_mut(&id).unwrap());
            match change {
                "history" => thread.history_has_more = Some(true),
                "earlier" => thread
                    .turns
                    .as_mut()
                    .unwrap()
                    .push(Arc::new(crate::models::Turn::default())),
                change => {
                    let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]);
                    match change {
                        "completed" => turn.status = TurnStatus::Completed,
                        "running" => turn.status = TurnStatus::Running,
                        "summary" => turn.items_summary = true,
                        "deferred" => {
                            let item = Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]);
                            item.body = ItemContent::Deferred { summary: Box::new(item.body().clone()) };
                        }
                        _ => turn.items.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({
                            "id":"output", "body":{"inline":{"body": if change == "assistant" {
                                json!({"assistantText":{"text":"Already started","phase":"commentary"}})
                            } else { json!({"commandExecution":{"command":"touch file","output":""}}) }
                        }}})).unwrap())),
                    }
                }
            }
            assert!(!next.can_change_provider((&id).into()), "{change}");
            let (blocked, _) = reduce_intent(
                &next,
                Intent::SelectModel {
                    thread_id: (&id).into(),
                    model: crate::models::ModelRef {
                        provider: ProviderKind::Codex,
                        id: "other".into(),
                    },
                },
            );
            assert_eq!(blocked.navigation, next.navigation, "{change}");
            assert_eq!(blocked.drafts, next.drafts, "{change}");
        }
    }

    proptest::proptest! {
        #[test]
        fn any_nonempty_assistant_output_keeps_provider_locked(text in ".+") {
            let (mut snapshot, id) = fixture();
            let thread = Arc::make_mut(Arc::make_mut(&mut snapshot.conversations).get_mut(&id).unwrap());
            let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]);
            turn.items.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({
                "id":"answer", "body":{"inline":{"body":{"assistantText":{"text":text,"phase":"final"}}}}
            })).unwrap()));
            proptest::prop_assert!(!snapshot.can_change_provider((&id).into()));
        }
    }
}
