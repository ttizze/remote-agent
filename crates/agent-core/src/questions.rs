//! Answering a provider's questions from the composer, as in T3 Code: one
//! question at a time, the composer text is the active question's own answer,
//! and choosing an option replaces that text, which returns to the draft.
use crate::state::{DraftKey, Snapshot};
use agent_protocol::{
    ids::RequestId,
    requests::{Answer, Question, QuestionAnswer, RequestBody},
    session::RequestDelivery,
};
use std::collections::BTreeMap;

/// Answers being composed for one question request. Local to this device.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuestionDraft {
    index: usize,
    answers: BTreeMap<String, DraftAnswer>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct DraftAnswer {
    text: String,
    choice_ids: Vec<String>,
}

impl DraftAnswer {
    /// The answer's own text outranks its chosen options.
    fn resolve(&self, question: &Question) -> Option<QuestionAnswer> {
        let answer = crate::presentation::conversation::build_question_answer(
            question.multiple,
            self.text.trim().to_owned(),
            self.choice_ids.clone(),
        );
        question.accepts(&answer).then_some(answer)
    }
}

impl QuestionDraft {
    fn index(&self, questions: &[Question]) -> usize {
        self.index.min(questions.len().saturating_sub(1))
    }
    fn answer(&self, question: &Question) -> Option<QuestionAnswer> {
        self.answers.get(&question.id)?.resolve(question)
    }
    fn answers(&self, questions: &[Question]) -> Option<BTreeMap<String, QuestionAnswer>> {
        questions
            .iter()
            .map(|question| Some((question.id.clone(), self.answer(question)?)))
            .collect()
    }
    fn edit(&mut self, question: &Question, text: String) {
        let answer = self.answers.entry(question.id.clone()).or_default();
        if !text.trim().is_empty() {
            answer.choice_ids.clear();
        }
        answer.text = text;
    }
    /// Returns the replaced text of the question's own answer.
    fn choose(&mut self, question: &Question, choice_id: String) -> String {
        let answer = self.answers.entry(question.id.clone()).or_default();
        if !question.multiple {
            answer.choice_ids = vec![choice_id];
        } else if let Some(index) = answer.choice_ids.iter().position(|id| *id == choice_id) {
            answer.choice_ids.remove(index);
        } else {
            answer.choice_ids.push(choice_id);
        }
        std::mem::take(&mut answer.text)
    }
    /// Moves past an answered question, or completes the answers after the last.
    fn advance(&mut self, questions: &[Question]) -> Option<Answer> {
        let index = self.index(questions);
        if index + 1 >= questions.len() {
            return self
                .answers(questions)
                .map(|answers| Answer::Questions { answers });
        }
        if self.answer(&questions[index]).is_some() {
            self.index = index + 1;
        }
        None
    }
}

/// Text replaced by a chosen option goes after the draft rather than vanishing.
fn carry_into_draft(draft: &str, replaced: &str) -> Option<String> {
    let replaced = replaced.trim();
    if replaced.is_empty() {
        None
    } else if draft.trim().is_empty() {
        Some(replaced.to_owned())
    } else {
        Some(format!("{}\n\n{replaced}", draft.trim_end()))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QuestionAction {
    /// The active question's own answer is the composer text.
    Edit {
        text: String,
    },
    /// Choosing a single option also moves on, as T3 Code does.
    Choose {
        choice_id: String,
    },
    /// Moves to the next question or sends every answer after the last.
    Advance,
    Back,
}

/// Applies `action` to the active question of `request_id`. Only an awaiting
/// question request accepts answers; other requests leave the state unchanged.
pub(crate) fn act(
    snapshot: &mut Snapshot,
    request_id: RequestId,
    action: QuestionAction,
) -> Option<Answer> {
    let (session, request) =
        snapshot
            .sessions
            .conversations
            .iter()
            .find_map(|(session, thread)| {
                Some((session.clone(), thread.requests.get(&request_id)?.clone()))
            })?;
    let RequestBody::Question { questions } = &request.body else {
        return None;
    };
    if request.delivery != RequestDelivery::Awaiting {
        return None;
    }
    let saved = snapshot
        .input
        .question_drafts
        .get(&request_id)
        .cloned()
        .unwrap_or_default();
    let mut draft = saved.clone();
    let question = questions.get(draft.index(questions))?;
    let mut replaced = String::new();
    let answer = match action {
        QuestionAction::Edit { text } => {
            if question.allow_free_text {
                draft.edit(question, text);
            }
            None
        }
        QuestionAction::Choose { choice_id } => {
            if !question.choices.iter().any(|choice| choice.id == choice_id) {
                return None;
            }
            replaced = draft.choose(question, choice_id);
            (!question.multiple)
                .then(|| draft.advance(questions))
                .flatten()
        }
        QuestionAction::Advance => draft.advance(questions),
        QuestionAction::Back => {
            draft.index = draft.index(questions).saturating_sub(1);
            None
        }
    };
    if draft != saved {
        let drafts = std::sync::Arc::make_mut(&mut snapshot.input.question_drafts);
        // Answered, withdrawn and cleared requests no longer need their drafts.
        drafts.retain(|id, _| {
            snapshot
                .sessions
                .conversations
                .values()
                .any(|thread| thread.requests.contains_key(id))
        });
        drafts.insert(request_id, draft);
    }
    let key = DraftKey::from(session);
    let text = snapshot
        .input
        .drafts
        .get(&key)
        .map_or("", |draft| draft.text.as_str());
    if let Some(text) = carry_into_draft(text, &replaced) {
        crate::state::set_draft_text(snapshot, key, text);
    }
    answer
}

/// The question a conversation's composer answers, one at a time.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionPrompt {
    pub request_id: RequestId,
    /// Shown while an answer is not awaited, such as while it is being sent.
    pub status: Option<String>,
    pub question: Question,
    /// "2/3" when the request asks more than one question.
    pub progress: Option<String>,
    /// The composer text: the question's own answer.
    pub text: String,
    /// Typing an own answer drops them; choosing one returns the text to the draft.
    pub chosen_choice_ids: Vec<String>,
    pub placeholder: String,
    pub can_respond: bool,
    pub can_go_back: bool,
    pub action: String,
    pub can_advance: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn question_prompt(&self, thread_id: crate::session::SessionRef) -> Option<QuestionPrompt> {
        let request = crate::presentation::conversation::docked_request(
            self.sessions.conversations.get(&thread_id)?,
            true,
        )?;
        let RequestBody::Question { questions } = &request.body else {
            return None;
        };
        let unanswered = QuestionDraft::default();
        let draft = self
            .input
            .question_drafts
            .get(&request.id)
            .unwrap_or(&unanswered);
        let index = draft.index(questions);
        let question = questions.get(index)?.clone();
        let answer = draft.answers.get(&question.id).cloned().unwrap_or_default();
        let can_respond = request.delivery == RequestDelivery::Awaiting;
        let last = index + 1 == questions.len();
        let presented =
            crate::presentation::conversation::request(self.catalog.ui_language, request);
        Some(QuestionPrompt {
            request_id: request.id.clone(),
            status: (!can_respond).then_some(presented.title),
            progress: (questions.len() > 1).then(|| format!("{}/{}", index + 1, questions.len())),
            chosen_choice_ids: answer.choice_ids,
            text: answer.text,
            placeholder: self
                .catalog
                .ui_language
                .text(if question.allow_free_text {
                    "回答を入力（空欄なら選んだ選択肢で回答）"
                } else {
                    "上の選択肢から選んでください"
                })
                .into(),
            can_respond,
            can_go_back: can_respond && index > 0,
            action: self
                .catalog
                .ui_language
                .text(if last {
                    "回答を送信"
                } else {
                    "次の質問"
                })
                .into(),
            can_advance: can_respond
                && if last {
                    draft.answers(questions).is_some()
                } else {
                    draft.answer(&question).is_some()
                },
            question,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        requests::{QuestionChoice, Request, RequestTarget},
        session::{ProviderKind, SessionRef},
    };
    use std::sync::Arc;

    fn question(id: &str, multiple: bool, allow_free_text: bool) -> Question {
        Question {
            id: id.into(),
            header: String::new(),
            prompt: format!("{id}?"),
            secret: false,
            allow_free_text,
            multiple,
            choices: ["a", "b"]
                .map(|choice| QuestionChoice {
                    id: format!("{id}/{choice}"),
                    label: choice.into(),
                    description: String::new(),
                })
                .into(),
        }
    }

    fn session() -> SessionRef {
        SessionRef {
            provider: Some(ProviderKind::Claude),
            id: "thread".into(),
        }
    }

    fn snapshot(questions: Vec<Question>, delivery: RequestDelivery, draft: &str) -> Snapshot {
        let mut snapshot = Snapshot::default();
        let mut thread = crate::models::Thread {
            id: Some(session()),
            ..Default::default()
        };
        thread.requests.insert(
            "request".into(),
            Arc::new(Request {
                id: "request".into(),
                target: RequestTarget::Session,
                delivery,
                body: RequestBody::Question { questions },
            }),
        );
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(session(), Arc::new(thread));
        crate::state::set_draft_text(&mut snapshot, session().into(), draft.into());
        snapshot
    }

    fn act_on(snapshot: &mut Snapshot, action: QuestionAction) -> Option<Answer> {
        act(snapshot, "request".into(), action)
    }

    fn edit(text: &str) -> QuestionAction {
        QuestionAction::Edit { text: text.into() }
    }

    fn choose(id: &str) -> QuestionAction {
        QuestionAction::Choose {
            choice_id: id.into(),
        }
    }

    fn answers<const N: usize>(answers: [(&str, QuestionAnswer); N]) -> Option<Answer> {
        Some(Answer::Questions {
            answers: answers
                .into_iter()
                .map(|(id, answer)| (id.to_owned(), answer))
                .collect(),
        })
    }

    #[test]
    fn composer_text_answers_the_question_and_a_chosen_option_returns_it_to_the_draft() {
        let mut typed = snapshot(
            vec![question("q", false, true)],
            RequestDelivery::Awaiting,
            "",
        );
        assert_eq!(act_on(&mut typed, edit("  own answer  ")), None);
        let prompt = typed.question_prompt(session()).unwrap();
        assert_eq!(prompt.text, "  own answer  ");
        assert_eq!((prompt.can_advance, prompt.progress), (true, None));
        assert_eq!(prompt.action, "回答を送信");
        assert_eq!(
            act_on(&mut typed, QuestionAction::Advance),
            answers([(
                "q",
                QuestionAnswer::FreeText {
                    text: "own answer".into()
                }
            )])
        );

        for (draft, typed, expected) in [
            ("", "typed", "typed"),
            ("draft  \n", " typed ", "draft\n\ntyped"),
            ("draft", "  ", "draft"),
        ] {
            let mut chosen = snapshot(
                vec![question("q", false, true)],
                RequestDelivery::Awaiting,
                draft,
            );
            act_on(&mut chosen, edit(typed));
            assert_eq!(
                act_on(&mut chosen, choose("q/b")),
                answers([(
                    "q",
                    QuestionAnswer::SingleChoice {
                        choice_id: "q/b".into()
                    }
                )]),
                "choosing the only question's option sends it"
            );
            assert_eq!(chosen.input.drafts[&session().into()].text, expected);
            assert_eq!(chosen.question_prompt(session()).unwrap().text, "");
        }
    }

    #[test]
    fn questions_are_answered_one_at_a_time_and_sent_after_the_last() {
        let mut snapshot = snapshot(
            vec![
                question("first", false, true),
                question("second", true, true),
            ],
            RequestDelivery::Awaiting,
            "",
        );
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(prompt.question.id, "first");
        assert_eq!(prompt.progress.as_deref(), Some("1/2"));
        assert_eq!(prompt.action, "次の質問");
        assert!(!prompt.can_advance && !prompt.can_go_back);
        assert_eq!(act_on(&mut snapshot, QuestionAction::Advance), None);
        assert_eq!(
            snapshot.question_prompt(session()).unwrap().question.id,
            "first"
        );

        assert_eq!(act_on(&mut snapshot, choose("first/a")), None);
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(prompt.question.id, "second");
        assert_eq!(prompt.progress.as_deref(), Some("2/2"));
        assert_eq!(prompt.action, "回答を送信");
        assert!(!prompt.can_advance && prompt.can_go_back);

        for choice in ["second/a", "second/b", "second/a"] {
            assert_eq!(
                act_on(&mut snapshot, choose(choice)),
                None,
                "options of a multiple choice toggle"
            );
        }
        act_on(&mut snapshot, edit(""));
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(
            prompt.chosen_choice_ids,
            ["second/b"],
            "an empty answer keeps the options"
        );
        assert!(prompt.can_advance);

        act_on(&mut snapshot, QuestionAction::Back);
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(prompt.question.id, "first");
        assert_eq!(prompt.chosen_choice_ids, ["first/a"]);
        assert_eq!(act_on(&mut snapshot, QuestionAction::Advance), None);

        act_on(&mut snapshot, edit("own"));
        assert!(
            snapshot
                .question_prompt(session())
                .unwrap()
                .chosen_choice_ids
                .is_empty()
        );
        act_on(&mut snapshot, edit(""));
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert!(
            prompt.chosen_choice_ids.is_empty() && !prompt.can_advance,
            "typing an own answer drops the chosen options"
        );
        act_on(&mut snapshot, edit("own"));
        assert_eq!(
            act_on(&mut snapshot, QuestionAction::Advance),
            answers([
                (
                    "first",
                    QuestionAnswer::SingleChoice {
                        choice_id: "first/a".into()
                    }
                ),
                ("second", QuestionAnswer::FreeText { text: "own".into() }),
            ])
        );
    }

    #[test]
    fn choice_only_questions_ignore_text_and_foreign_options() {
        let mut snapshot = snapshot(
            vec![question("q", false, false)],
            RequestDelivery::Awaiting,
            "",
        );
        assert_eq!(act_on(&mut snapshot, edit("text")), None);
        assert_eq!(act_on(&mut snapshot, choose("other/a")), None);
        assert_eq!(act_on(&mut snapshot, QuestionAction::Back), None);
        assert!(snapshot.input.question_drafts.is_empty(), "nothing changed");
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(prompt.text, "");
        assert!(prompt.chosen_choice_ids.is_empty() && !prompt.can_advance);
        assert_eq!(prompt.placeholder, "上の選択肢から選んでください");
    }

    #[test]
    fn only_an_awaiting_question_accepts_answers() {
        let mut snapshot = snapshot(
            vec![question("q", false, true)],
            RequestDelivery::Sending,
            "",
        );
        assert_eq!(act_on(&mut snapshot, edit("text")), None);
        assert_eq!(act_on(&mut snapshot, choose("q/a")), None);
        assert!(snapshot.input.question_drafts.is_empty());
        let prompt = snapshot.question_prompt(session()).unwrap();
        assert_eq!(prompt.status.as_deref(), Some("回答を送信中"));
        assert!(!prompt.can_respond && !prompt.can_advance);
        assert_eq!(act(&mut snapshot, "missing".into(), choose("q/a")), None);
    }

    #[test]
    fn drafts_of_requests_that_left_their_conversation_are_dropped() {
        let mut snapshot = snapshot(
            vec![question("q", false, true)],
            RequestDelivery::Awaiting,
            "",
        );
        act_on(&mut snapshot, edit("text"));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session())
                .unwrap(),
        );
        let request = thread.requests.remove("request").unwrap();
        thread.requests.insert(
            "next".into(),
            Arc::new(Request {
                id: "next".into(),
                ..request.as_ref().clone()
            }),
        );
        let other = SessionRef {
            id: "other".into(),
            ..session()
        };
        let mut other_thread = crate::models::Thread {
            id: Some(other.clone()),
            ..Default::default()
        };
        other_thread.requests.insert(
            "other".into(),
            Arc::new(Request {
                id: "other".into(),
                ..request.as_ref().clone()
            }),
        );
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(other.clone(), Arc::new(other_thread));
        act(&mut snapshot, "other".into(), edit("kept"));
        act(&mut snapshot, "next".into(), edit("next"));
        assert_eq!(
            snapshot
                .input
                .question_drafts
                .keys()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            ["next", "other"]
        );
        assert_eq!(snapshot.question_prompt(other).unwrap().text, "kept");
        assert_eq!(snapshot.question_prompt(session()).unwrap().text, "next");
    }
}
