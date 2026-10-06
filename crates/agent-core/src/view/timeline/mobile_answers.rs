//! Drafts of the answers to a pending question request: option chips, a
//! custom answer and attachments, and the answers they submit.
use crate::js_text::js_trim;
use agent_domain::{Answer, Answers, Question};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftAnswer {
    pub selected_option_values: Option<Vec<String>>,
    pub custom_answer: Option<String>,
    pub attachment_count: u64,
    /// An attachment is still uploading or failed.
    pub attachments_blocked: bool,
}

fn normalize_draft_answer(value: Option<&str>) -> Option<&str> {
    value.map(js_trim).filter(|value| !value.is_empty())
}

/// Options carry no value of their own, so a chip matches its trimmed label.
fn resolve_option_value(question: &Question, value: &str) -> Option<String> {
    let label = js_trim(value);
    (!label.is_empty()
        && question
            .options
            .iter()
            .any(|option| js_trim(&option.label) == label))
    .then(|| label.to_owned())
}

fn normalize_selected_option_values(question: &Question, values: Option<&[String]>) -> Vec<String> {
    let mut normalized: Vec<String> = vec![];
    for value in values
        .unwrap_or_default()
        .iter()
        .filter_map(|value| resolve_option_value(question, value))
    {
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn resolve_answer(question: &Question, draft: Option<&DraftAnswer>) -> Option<Answer> {
    if draft.is_some_and(|draft| draft.attachments_blocked) {
        return None;
    }
    if let Some(custom) =
        normalize_draft_answer(draft.and_then(|draft| draft.custom_answer.as_deref()))
    {
        return Some(Answer::Text(custom.to_owned()));
    }
    let selected = normalize_selected_option_values(
        question,
        draft.and_then(|draft| draft.selected_option_values.as_deref()),
    );
    let attached = draft.is_some_and(|draft| draft.attachment_count > 0);
    if question.multiple {
        return if !selected.is_empty() {
            Some(Answer::Choices(selected))
        } else {
            attached.then(|| Answer::Text(String::new()))
        };
    }
    selected
        .into_iter()
        .next()
        .map(Answer::Text)
        .or_else(|| attached.then(|| Answer::Text(String::new())))
}

pub fn set_custom_answer(
    question: &Question,
    draft: Option<&DraftAnswer>,
    custom_answer: &str,
) -> DraftAnswer {
    let selected = if js_trim(custom_answer).is_empty() {
        normalize_selected_option_values(
            question,
            draft.and_then(|draft| draft.selected_option_values.as_deref()),
        )
    } else {
        vec![]
    };
    DraftAnswer {
        custom_answer: Some(custom_answer.to_owned()),
        selected_option_values: (!selected.is_empty()).then_some(selected),
        ..DraftAnswer::default()
    }
}

pub fn is_option_selected(question: &Question, draft: Option<&DraftAnswer>, option: &str) -> bool {
    if normalize_draft_answer(draft.and_then(|draft| draft.custom_answer.as_deref())).is_some() {
        return false;
    }
    resolve_option_value(question, option).is_some_and(|value| {
        normalize_selected_option_values(
            question,
            draft.and_then(|draft| draft.selected_option_values.as_deref()),
        )
        .contains(&value)
    })
}

pub fn toggle_option_selection(
    question: &Question,
    draft: Option<&DraftAnswer>,
    option: &str,
) -> DraftAnswer {
    let Some(value) = resolve_option_value(question, option) else {
        return draft.cloned().unwrap_or_default();
    };
    if question.multiple {
        let mut selected = normalize_selected_option_values(
            question,
            draft.and_then(|draft| draft.selected_option_values.as_deref()),
        );
        match selected.iter().position(|selected| selected == &value) {
            Some(index) => {
                selected.remove(index);
            }
            None => selected.push(value),
        }
        return DraftAnswer {
            custom_answer: Some(String::new()),
            selected_option_values: (!selected.is_empty()).then_some(selected),
            ..DraftAnswer::default()
        };
    }
    DraftAnswer {
        custom_answer: Some(String::new()),
        selected_option_values: Some(vec![value]),
        ..DraftAnswer::default()
    }
}

/// The answers to submit, or `None` while a question lacks one.
pub fn build_answers(
    questions: &[Question],
    drafts: &std::collections::BTreeMap<String, DraftAnswer>,
) -> Option<Answers> {
    questions
        .iter()
        .map(|question| {
            resolve_answer(question, drafts.get(&question.id))
                .map(|answer| (question.id.clone(), answer))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::QuestionOption;
    use std::collections::BTreeMap;

    fn question(
        id: &str,
        header: &str,
        text: &str,
        options: &[(&str, &str)],
        multiple: bool,
    ) -> Question {
        Question {
            required: true,
            id: id.into(),
            header: header.into(),
            question: text.into(),
            multiple,
            options: options
                .iter()
                .map(|(label, description)| QuestionOption {
                    label: (*label).into(),
                    description: Some((*description).into()),
                })
                .collect(),
        }
    }

    fn single_select() -> Question {
        question(
            "runtime",
            "Runtime",
            "Which runtime should be used?",
            &[("Go", "One binary"), ("Node.js", "Reuse TypeScript")],
            false,
        )
    }

    fn multi_select() -> Question {
        question(
            "scope",
            "Scope",
            "Which data should be collected?",
            &[("Orders", "Receipts"), ("Listings", "Inventory")],
            true,
        )
    }

    fn selected(values: &[&str]) -> DraftAnswer {
        DraftAnswer {
            selected_option_values: Some(values.iter().map(|value| (*value).into()).collect()),
            ..DraftAnswer::default()
        }
    }

    fn chips(custom: &str, values: &[&str]) -> DraftAnswer {
        DraftAnswer {
            custom_answer: Some(custom.into()),
            selected_option_values: (!values.is_empty())
                .then(|| values.iter().map(|value| (*value).into()).collect()),
            ..DraftAnswer::default()
        }
    }

    #[test]
    fn replaces_single_select_options_and_toggles_multi_select_options() {
        assert_eq!(
            toggle_option_selection(&single_select(), Some(&selected(&["Go"])), "Node.js"),
            chips("", &["Node.js"])
        );
        let orders = toggle_option_selection(&multi_select(), None, "Orders");
        let orders_and_listings =
            toggle_option_selection(&multi_select(), Some(&orders), "Listings");
        assert_eq!(orders_and_listings, chips("", &["Orders", "Listings"]));
        assert_eq!(
            toggle_option_selection(&multi_select(), Some(&orders_and_listings), "Orders"),
            chips("", &["Listings"])
        );
        let padded = toggle_option_selection(&multi_select(), None, "  Orders  ");
        assert_eq!(padded, chips("", &["Orders"]));
        assert_eq!(
            toggle_option_selection(&multi_select(), Some(&padded), "  Orders  "),
            chips("", &[])
        );
    }

    #[test]
    fn builds_array_answers_for_multi_select_questions() {
        let drafts = BTreeMap::from([
            ("runtime".to_owned(), selected(&["Go"])),
            ("scope".to_owned(), selected(&["Orders", "Listings"])),
        ]);
        assert_eq!(
            build_answers(&[single_select(), multi_select()], &drafts),
            Some(BTreeMap::from([
                ("runtime".to_owned(), Answer::Text("Go".into())),
                (
                    "scope".to_owned(),
                    Answer::Choices(vec!["Orders".into(), "Listings".into()])
                ),
            ]))
        );
    }

    #[test]
    fn clears_selected_options_while_a_custom_answer_is_active() {
        assert_eq!(
            set_custom_answer(
                &multi_select(),
                Some(&selected(&["Orders", "Listings"])),
                "Orders first"
            ),
            chips("Orders first", &[])
        );
    }

    #[test]
    fn matches_selected_chips_against_normalized_option_labels() {
        assert!(is_option_selected(
            &multi_select(),
            Some(&selected(&["Orders"])),
            "  Orders  "
        ));
        assert!(!is_option_selected(
            &multi_select(),
            Some(&chips("Orders first", &["Orders"])),
            "  Orders  "
        ));
    }

    #[test]
    fn accepts_ready_attachment_only_answers_while_preserving_selected_options() {
        let question = question(
            "q",
            "Spec",
            "Provide a specification",
            &[("Yes", "Approve")],
            false,
        );
        let answers = |draft: DraftAnswer| {
            build_answers(
                std::slice::from_ref(&question),
                &BTreeMap::from([("q".to_owned(), draft)]),
            )
        };
        let attached = DraftAnswer {
            attachment_count: 1,
            ..DraftAnswer::default()
        };
        assert_eq!(
            answers(attached.clone()),
            Some(BTreeMap::from([(
                "q".to_owned(),
                Answer::Text(String::new())
            )]))
        );
        assert_eq!(
            answers(DraftAnswer {
                selected_option_values: Some(vec!["Yes".into()]),
                ..attached.clone()
            }),
            Some(BTreeMap::from([(
                "q".to_owned(),
                Answer::Text("Yes".into())
            )]))
        );
        assert_eq!(
            answers(DraftAnswer {
                attachments_blocked: true,
                ..attached
            }),
            None
        );
    }
}
