//! Citation navigation trusts canonical item identities and rejects foreign storage.
use super::*;
use crate::presentation::conversation::RenderedConversation;
use crate::state::operations::{ReadItem, ReadThread};
use agent_protocol::{
    context::TextRange,
    models::{Thread, Turn},
};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CitationDocumentRange {
    pub document: u32,
    pub range: TextRange,
}

/// A quote repeated in separate native text documents is still ambiguous.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn locate_citation_documents(
    citation: AssistantCitation,
    rendered_documents: Vec<String>,
) -> Option<CitationDocumentRange> {
    let mut found = rendered_documents
        .into_iter()
        .enumerate()
        .filter_map(|(document, text)| {
            locate_citation(citation.clone(), text).map(|range| CitationDocumentRange {
                document: document as u32,
                range,
            })
        });
    let result = found.next()?;
    found.next().is_none().then_some(result)
}

pub(super) fn citation_turn<'a>(
    thread: &'a Thread,
    item_id: &agent_protocol::ids::ItemId,
) -> Option<&'a Turn> {
    let mut matches = thread.turns.as_ref()?.iter().flat_map(|turn| {
        turn.items
            .iter()
            .flatten()
            .filter(|item| item.id == *item_id)
            .map(|item| (turn.as_ref(), item.as_ref()))
    });
    let (turn, item) = matches.next()?;
    (matches.next().is_none()
        && matches!(
            item.body(),
            agent_protocol::items::ItemBody::AssistantText { .. }
        ))
    .then_some(turn)
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CitationSourceStatus {
    Ready,
    LoadItem {
        item: ReadItem,
    },
    Older {
        session: agent_protocol::session::SessionRef,
    },
    Unavailable,
}

fn source_in_scope(
    host: &str,
    storage: &str,
    citation: &AssistantCitation,
) -> Result<(), crate::client::AgentError> {
    citation.validate().map_err(crate::client::error)?;
    if citation.source.host != host || citation.source.storage_scope != storage {
        return Err(crate::client::error(
            "citation source belongs to a different Host or storage scope",
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn citation_source_request(
        &self,
        host: String,
        citation: AssistantCitation,
    ) -> Result<ReadThread, crate::client::AgentError> {
        source_in_scope(&host, &self.connection.storage_scope, &citation)?;
        Ok(ReadThread::open(citation.source.session))
    }

    pub fn citation_source_status(
        &self,
        host: String,
        citation: AssistantCitation,
    ) -> Result<CitationSourceStatus, crate::client::AgentError> {
        source_in_scope(&host, &self.connection.storage_scope, &citation)?;
        let source = &citation.source;
        let Some(thread) = self.sessions.conversations.get(&source.session) else {
            return Ok(CitationSourceStatus::Unavailable);
        };
        let Some(turn) = citation_turn(thread, &source.item_id) else {
            return Ok(if thread.history_has_more == Some(true) {
                CitationSourceStatus::Older {
                    session: source.session.clone(),
                }
            } else {
                CitationSourceStatus::Unavailable
            });
        };
        if turn.id != source.turn_id {
            return Ok(CitationSourceStatus::Unavailable);
        }
        let item = ReadItem {
            thread_id: source.session.clone(),
            turn_id: source.turn_id.clone(),
            item_id: source.item_id.clone(),
        };
        let deferred = turn
            .items
            .iter()
            .flatten()
            .any(|candidate| candidate.id == source.item_id && candidate.is_deferred());
        Ok(if deferred {
            CitationSourceStatus::LoadItem { item }
        } else {
            CitationSourceStatus::Ready
        })
    }
}

fn source_row<'a>(
    source: &'a crate::presentation::conversation::RenderedConversation,
    host: &str,
    storage: &str,
    citation: &AssistantCitation,
) -> Option<(&'a crate::presentation::conversation::RenderedTurn, usize)> {
    use crate::presentation::conversation::ConversationRowContent;
    source_in_scope(host, storage, citation).ok()?;
    if source.source.id.as_ref() != Some(&citation.source.session) {
        return None;
    }
    let canonical = citation_turn(&source.source, &citation.source.item_id)?;
    if canonical.id != citation.source.turn_id {
        return None;
    }
    source.turns.iter().find_map(|turn| {
        if !std::ptr::eq(turn.source.as_ref(), canonical) {
            return None;
        }
        turn.rows
            .iter()
            .position(|row| match &row.content {
                ConversationRowContent::Response { item, .. }
                | ConversationRowContent::Activity { item, .. } => {
                    item.data.native_id.as_ref() == Some(&citation.source.item_id)
                }
                _ => false,
            })
            .map(|index| (turn.as_ref(), index))
    })
}

pub(crate) fn citation_row(
    source: &crate::presentation::conversation::RenderedConversation,
    scope: &crate::panels::PanelScope,
    citation: &AssistantCitation,
) -> Option<String> {
    let (turn, index) = source_row(source, &scope.host, &scope.storage, citation)?;
    Some(turn.rows[index].id.clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CitationExpansionChoice {
    pub id: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CitationExpansion {
    pub turn_key: String,
    pub activity: Option<CitationExpansionChoice>,
    pub groups: Vec<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedConversation {
    /// Open only the controls that contain the canonical source, preserving other reading choices.
    pub fn citation_expansion(
        &self,
        host: String,
        storage: String,
        citation: AssistantCitation,
    ) -> Option<CitationExpansion> {
        use crate::presentation::conversation::ConversationRowContent;
        let (turn, index) = source_row(self, &host, &storage, &citation)?;
        let activity = if matches!(
            turn.rows[index].content,
            ConversationRowContent::Activity { .. }
        ) {
            turn.rows[..index]
                .iter()
                .rev()
                .take_while(|row| {
                    !matches!(
                        row.content,
                        ConversationRowContent::User { .. }
                            | ConversationRowContent::Response { .. }
                    )
                })
                .find(|row| matches!(row.content, ConversationRowContent::ActivityHeader { .. }))
                .and_then(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity }
                        if activity.activity_can_collapse =>
                    {
                        Some(CitationExpansionChoice {
                            id: activity.id.clone(),
                            status: activity.status.clone(),
                        })
                    }
                    _ => None,
                })
        } else {
            None
        };
        let groups = crate::presentation::conversation::attempts::attempt_folds(&turn.source)
            .into_iter()
            .filter(|fold| fold.items.contains(&citation.source.item_id))
            .map(|fold| fold.group_id())
            .collect();
        Some(CitationExpansion {
            turn_key: turn.key(),
            activity,
            groups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        context::CitationSource,
        execution::{ItemStatus, TurnStatus},
        items::{AssistantPhase, Item, ItemBody},
        session::{ProviderKind, SessionRef},
    };
    use std::sync::Arc;

    fn fixture() -> (Snapshot, AssistantCitation, Arc<Thread>) {
        let session =
            SessionRef::new(ProviderKind::Codex, "thread:with:delimiters".into()).unwrap();
        let citation = make_citation(
            CitationSource {
                host: "host".into(),
                storage_scope: "scope".into(),
                session: session.clone(),
                turn_id: "turn:0".into(),
                item_id: "answer:0".into(),
            },
            "😀quote".into(),
            TextRange { start: 2, end: 7 },
        )
        .unwrap();
        let thread = Arc::new(Thread {
            id: Some(session.clone()),
            turns: Some(vec![Arc::new(Turn {
                id: "turn:0".into(),
                status: TurnStatus::Completed,
                items: Some(vec![Arc::new(Item::new(
                    "answer:0".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: "😀quote".into(),
                        phase: AssistantPhase::Final,
                    },
                ))]),
                ..Default::default()
            })]),
            ..Default::default()
        });
        let mut snapshot = Snapshot::default();
        snapshot.connection.storage_scope = "scope".into();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(session, thread.clone());
        (snapshot, citation, thread)
    }

    #[test]
    fn citation_document_lookup_keeps_utf16_and_rejects_ambiguous_or_changed_text() {
        let (_, citation, _) = fixture();
        assert_eq!(
            locate_citation_documents(citation.clone(), vec!["other".into(), "x😀quote".into()]),
            Some(CitationDocumentRange {
                document: 1,
                range: TextRange { start: 3, end: 8 }
            })
        );
        assert_eq!(
            locate_citation_documents(citation.clone(), vec!["😀quote".into(), "😀quote".into()]),
            None
        );
        assert_eq!(
            locate_citation_documents(citation, vec!["😀changed".into()]),
            None
        );
    }

    #[test]
    fn citation_navigation_checks_host_storage_turn_identity_and_ambiguous_items() {
        let (mut snapshot, citation, thread) = fixture();
        assert!(
            snapshot
                .citation_source_request("foreign".into(), citation.clone())
                .is_err()
        );
        assert!(matches!(
            snapshot
                .citation_source_status("host".into(), citation.clone())
                .unwrap(),
            CitationSourceStatus::Ready
        ));
        let mut wrong_turn = citation.clone();
        wrong_turn.source.turn_id = "other".into();
        assert!(matches!(
            snapshot
                .citation_source_status("host".into(), wrong_turn)
                .unwrap(),
            CitationSourceStatus::Unavailable
        ));
        snapshot.connection.storage_scope = "other".into();
        assert!(
            snapshot
                .citation_source_request("host".into(), citation.clone())
                .is_err()
        );
        snapshot.connection.storage_scope = "scope".into();
        let mut duplicate = thread.as_ref().clone();
        let turn = Arc::make_mut(&mut duplicate.turns.as_mut().unwrap()[0]);
        let items = turn.items.as_mut().unwrap();
        items.push(items[0].clone());
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(citation.source.session.clone(), Arc::new(duplicate));
        assert!(
            snapshot
                .citation_source(
                    "host".into(),
                    citation.source.session.clone(),
                    citation.source.item_id.clone()
                )
                .is_none()
        );
        assert!(matches!(
            snapshot
                .citation_source_status("host".into(), citation)
                .unwrap(),
            CitationSourceStatus::Unavailable
        ));
    }

    #[test]
    fn citation_navigation_pages_history_and_hydrates_deferred_sources_then_resolves_a_native_row()
    {
        let (mut snapshot, citation, thread) = fixture();
        let mut older = thread.as_ref().clone();
        older.turns = Some(vec![]);
        older.history_has_more = Some(true);
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(citation.source.session.clone(), Arc::new(older));
        assert!(matches!(
            snapshot
                .citation_source_status("host".into(), citation.clone())
                .unwrap(),
            CitationSourceStatus::Older { .. }
        ));
        let mut deferred = thread.as_ref().clone();
        let turn = Arc::make_mut(&mut deferred.turns.as_mut().unwrap()[0]);
        Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]).defer();
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(citation.source.session.clone(), Arc::new(deferred));
        assert!(matches!(
            snapshot
                .citation_source_status("host".into(), citation.clone())
                .unwrap(),
            CitationSourceStatus::LoadItem { .. }
        ));
        let scope = crate::panels::PanelScope {
            host: "host".into(),
            storage: "scope".into(),
            draft: citation.source.session.clone().into(),
        };
        let rendered = crate::presentation::conversation::project_conversation(
            crate::localization::UiLanguage::English,
            &scope.draft,
            &Default::default(),
            thread,
            &None,
        );
        let rows = crate::presentation::conversation::rows::ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            rendered,
            scope,
            None,
            Default::default(),
            None,
        );
        let id = rows.citation_row(citation.clone()).unwrap();
        assert!(rows.rows().iter().any(|row| row.id() == id));
        let mut wrong_turn = citation;
        wrong_turn.source.turn_id = "other".into();
        assert!(rows.citation_row(wrong_turn).is_none());
    }

    fn commentary_fixture(
        attempt: bool,
    ) -> (AssistantCitation, Arc<Thread>, crate::panels::PanelScope) {
        use agent_protocol::models::{AttemptState, ExecutionAttempt};
        let (_, citation, mut thread) = fixture();
        let turn = Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]);
        let mut commentary = Item::new(
            citation.source.item_id.clone(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "😀quote".into(),
                phase: AssistantPhase::Commentary,
            },
        );
        let mut command = Item::new(
            "command".into(),
            ItemStatus::Completed,
            ItemBody::CommandExecution {
                output_omitted: false,
                command: "pwd".into(),
                cwd: None,
                output: "/work".into(),
                exit_code: Some(0),
            },
        );
        if attempt {
            let id = uuid::Uuid::from_u128(9);
            commentary.attempt = Some(id);
            command.attempt = Some(id);
            turn.attempts = vec![ExecutionAttempt {
                id,
                state: AttemptState::Superseded { by_steer: true },
            }];
        }
        turn.items = Some(vec![
            Arc::new(commentary),
            Arc::new(command),
            Arc::new(Item::new(
                "final".into(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: "final answer".into(),
                    phase: AssistantPhase::Final,
                },
            )),
        ]);
        let scope = crate::panels::PanelScope {
            host: "host".into(),
            storage: "scope".into(),
            draft: citation.source.session.clone().into(),
        };
        (citation, thread, scope)
    }

    fn projected(
        thread: Arc<Thread>,
        scope: &crate::panels::PanelScope,
        expansion: std::collections::HashMap<String, String>,
    ) -> Arc<crate::presentation::conversation::rows::ConversationRows> {
        use crate::presentation::conversation::{project_conversation, rows::ConversationRows};
        let source = project_conversation(
            crate::localization::UiLanguage::English,
            &scope.draft,
            &Default::default(),
            thread,
            &None,
        );
        ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source,
            scope.clone(),
            None,
            expansion,
            None,
        )
    }

    #[test]
    fn citation_reveal_opens_commentary_fold_and_keeps_exact_source_identity() {
        use crate::presentation::conversation::ConversationRowContent;
        let (citation, thread, scope) = commentary_fixture(false);
        let closed = projected(thread.clone(), &scope, Default::default());
        assert!(
            closed.citation_row(citation.clone()).is_none(),
            "completed commentary starts inside closed work"
        );
        let original = [("another-control".to_owned(), "open".to_owned())]
            .into_iter()
            .collect();
        let expanded = closed.citation_reveal(citation.clone(), original).unwrap();
        assert_eq!(
            expanded.get("another-control").map(String::as_str),
            Some("open")
        );
        let opened = projected(thread, &scope, expanded);
        let id = opened.citation_row(citation.clone()).unwrap();
        let actual = opened
            .rows()
            .into_iter()
            .find(|row| row.id() == id)
            .unwrap();
        assert!(
            matches!(&actual.data.content, ConversationRowContent::Activity { item, .. } if item.data.native_id.as_ref() == Some(&citation.source.item_id))
        );
        assert!(
            opened
                .citation_reveal(citation.clone(), Default::default())
                .is_some()
        );
        let mut stale = citation;
        stale.source.storage_scope = "stale".into();
        assert!(
            opened
                .citation_reveal(stale.clone(), Default::default())
                .is_none()
        );
        assert!(opened.citation_row(stale).is_none());
    }

    #[test]
    fn citation_reveal_opens_containing_attempt_group_without_opening_unrelated_groups() {
        use crate::presentation::conversation::{
            TimelineStyle, TurnRow,
            rows::{ConversationRows, VisibleConversationRow},
        };
        use std::collections::HashMap;
        let (citation, thread, scope) = commentary_fixture(true);
        let closed = projected(thread, &scope, Default::default());
        let source = closed.source();
        let plan = source
            .citation_expansion(scope.host.clone(), scope.storage.clone(), citation.clone())
            .unwrap();
        assert_eq!(
            plan.groups,
            vec![
                crate::presentation::conversation::attempts::attempt_folds(&source.turns[0].source)
                    [0]
                .group_id()
            ]
        );
        let choices = HashMap::from([("unrelated-group".to_owned(), "open".to_owned())]);
        let expanded = closed.citation_reveal(citation.clone(), choices).unwrap();
        let closed_group = ConversationRows::project(
            TimelineStyle::NATIVE,
            source.clone(),
            scope.clone(),
            None,
            expanded,
            None,
        );
        let is_source = |row: &Arc<VisibleConversationRow>| matches!(row.timeline(), Some(TurnRow::Commentary(item)) if item.data.native_id.as_ref() == Some(&citation.source.item_id));
        assert!(
            closed_group
                .rows
                .iter()
                .any(|row| matches!(row.timeline(), Some(TurnRow::Group { open: false, .. })))
        );
        assert!(!closed_group.rows.iter().any(is_source));
        let expanded = closed_group
            .citation_reveal(citation.clone(), closed_group.expansion())
            .unwrap();
        let visible = ConversationRows::project(
            TimelineStyle::NATIVE,
            source.clone(),
            scope.clone(),
            None,
            expanded,
            Some(closed_group),
        );
        assert_eq!(visible.rows.iter().filter(|row| is_source(row)).count(), 1);
        assert!(visible.expansion().contains_key("unrelated-group"));
        assert!(
            source
                .citation_expansion("foreign".into(), scope.storage, citation)
                .is_none()
        );
    }

    #[test]
    fn citation_row_excludes_response_actions_footer_with_the_same_native_message() {
        use crate::presentation::conversation::{
            ConversationRowContent, project_conversation, rows::ConversationRows,
        };
        let (_, citation, thread) = fixture();
        let scope = crate::panels::PanelScope {
            host: "host".into(),
            storage: "scope".into(),
            draft: citation.source.session.clone().into(),
        };
        let mut source = project_conversation(
            crate::localization::UiLanguage::English,
            &scope.draft,
            &Default::default(),
            thread,
            &None,
        );
        let body = source.turns[0]
            .rows
            .iter()
            .find(|row| matches!(row.content, ConversationRowContent::Response { .. }))
            .unwrap()
            .clone();
        let mut footer = body.clone();
        footer.id = format!("actions:{}", footer.id);
        Arc::get_mut(&mut source).unwrap().session_rows.push(footer);
        let mut projection = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source,
            scope,
            None,
            Default::default(),
            None,
        );
        let body_id = projection
            .rows
            .iter()
            .find(|row| row.data.id == body.id)
            .unwrap()
            .id();
        let last = projection.rows.len() - 1;
        Arc::get_mut(&mut projection).unwrap().rows.swap(0, last);
        assert_eq!(projection.citation_row(citation), Some(body_id));
    }

    #[test]
    fn citation_native_timeline_reveal_opens_real_fold_then_real_attempt_group() {
        use crate::presentation::conversation::{
            ConversationRowContent, TurnRow, project_conversation, rows::ConversationRows,
        };
        let (citation, thread, scope) = commentary_fixture(true);
        let source = project_conversation(
            crate::localization::UiLanguage::English,
            &scope.draft,
            &Default::default(),
            thread,
            &None,
        );
        let canonical_id = source.turns[0].rows.iter().find(|row| matches!(&row.content,
            ConversationRowContent::Activity { item, .. } if item.data.native_id.as_ref() == Some(&citation.source.item_id))).unwrap().id.clone();
        let closed = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source.clone(),
            scope.clone(),
            None,
            Default::default(),
            None,
        );
        assert!(closed.citation_row(citation.clone()).is_none());
        assert!(
            !closed
                .rows
                .iter()
                .any(|row| matches!(row.timeline(), Some(TurnRow::Group { .. })))
        );
        let first = closed
            .citation_reveal(citation.clone(), Default::default())
            .unwrap();
        assert_eq!(
            first.len(),
            1,
            "only the actual visible work control can be opened first"
        );
        let fold_opened = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source.clone(),
            scope.clone(),
            None,
            first.clone(),
            Some(closed),
        );
        assert!(fold_opened.citation_row(citation.clone()).is_none());
        assert!(
            fold_opened
                .rows
                .iter()
                .any(|row| matches!(row.timeline(), Some(TurnRow::Group { open: false, .. })))
        );
        let second = fold_opened
            .citation_reveal(citation.clone(), first)
            .unwrap();
        assert_eq!(second.len(), 2);
        let opened = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source,
            scope,
            None,
            second,
            Some(fold_opened),
        );
        let id = opened.citation_row(citation).unwrap();
        let actual = opened.rows.iter().find(|row| row.id() == id).unwrap();
        assert!(matches!(actual.timeline(), Some(TurnRow::Commentary(_))));
        assert_eq!(actual.data.id, canonical_id);
    }

    #[test]
    fn citation_native_timeline_targets_response_text_and_never_its_actions_footer() {
        use crate::presentation::conversation::{
            TurnRow, project_conversation, rows::ConversationRows,
        };
        let (_, citation, mut thread) = fixture();
        let turn = Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]);
        turn.status = TurnStatus::Failed;
        turn.items = Some(vec![
            Arc::new(Item::new(
                citation.source.item_id.clone(),
                ItemStatus::Completed,
                ItemBody::AssistantText {
                    text: "😀quote".into(),
                    phase: AssistantPhase::Unknown,
                },
            )),
            Arc::new(Item::new(
                "later-work".into(),
                ItemStatus::Completed,
                ItemBody::CommandExecution {
                    output_omitted: false,
                    command: "pwd".into(),
                    cwd: None,
                    output: "/work".into(),
                    exit_code: Some(0),
                },
            )),
        ]);
        let scope = crate::panels::PanelScope {
            host: "host".into(),
            storage: "scope".into(),
            draft: citation.source.session.clone().into(),
        };
        let source = project_conversation(
            crate::localization::UiLanguage::English,
            &scope.draft,
            &Default::default(),
            thread,
            &None,
        );
        let rows = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source,
            scope,
            None,
            Default::default(),
            None,
        );
        let body = rows
            .rows
            .iter()
            .find(|row| {
                matches!(row.timeline(), Some(TurnRow::Row(_)))
                    && matches!(
                        &row.data.content,
                        crate::presentation::conversation::ConversationRowContent::Response { .. }
                    )
            })
            .unwrap();
        let footer = rows
            .rows
            .iter()
            .find(|row| matches!(row.timeline(), Some(TurnRow::Actions(_))))
            .unwrap();
        assert_eq!(rows.citation_row(citation), Some(body.id()));
        assert_ne!(body.id(), footer.id());
    }
    #[test]
    fn legacy_citation_source_is_already_visible_in_non_completed_turns() {
        for status in [
            TurnStatus::Running,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let (citation, mut thread, scope) = commentary_fixture(false);
            Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).status =
                status;
            let rows = projected(thread, &scope, Default::default());
            assert!(
                rows.citation_row(citation).is_some(),
                "unfinished-turn commentary does not require a completed-work fold"
            );
        }
    }

    #[test]
    fn native_citation_reveals_folded_commentary_and_keeps_unfinished_response_text_visible() {
        use crate::presentation::conversation::{project_conversation, rows::ConversationRows};
        for status in [
            TurnStatus::Running,
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            let (citation, mut thread, scope) = commentary_fixture(false);
            Arc::make_mut(&mut Arc::make_mut(&mut thread).turns.as_mut().unwrap()[0]).status =
                status;
            let source = project_conversation(
                crate::localization::UiLanguage::English,
                &scope.draft,
                &Default::default(),
                thread,
                &None,
            );
            let closed = ConversationRows::project(
                crate::presentation::conversation::TimelineStyle::NATIVE,
                source.clone(),
                scope.clone(),
                None,
                Default::default(),
                None,
            );
            if status == TurnStatus::Completed {
                assert!(closed.citation_row(citation.clone()).is_none());
                let expansion = closed
                    .citation_reveal(citation.clone(), Default::default())
                    .unwrap();
                let opened = ConversationRows::project(
                    crate::presentation::conversation::TimelineStyle::NATIVE,
                    source,
                    scope,
                    None,
                    expansion,
                    Some(closed),
                );
                assert!(
                    opened.citation_row(citation).is_some(),
                    "citation remained hidden for {status:?}"
                );
            } else {
                assert!(
                    closed.citation_row(citation).is_some(),
                    "response text was hidden for {status:?}"
                );
            }
        }
    }
}
