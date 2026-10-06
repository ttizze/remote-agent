//! One-line previews of the answers given to a question request.
use super::QuestionAnswer;
use super::command_label::{js_space, js_trim};
use agent_domain::Answer;

/// Runs of white space as one space, trimmed.
fn collapsed(text: &str) -> String {
    let mut result = String::new();
    let mut in_space = false;
    for c in text.chars() {
        if js_space(c) {
            if !in_space {
                result.push(' ');
            }
            in_space = true;
        } else {
            result.push(c);
            in_space = false;
        }
    }
    js_trim(&result).to_owned()
}

pub fn question_answer_text(answer: &Answer) -> String {
    match answer {
        Answer::Text(text) => text.clone(),
        Answer::Choices(choices) => choices
            .iter()
            .filter(|choice| !choice.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    }
}

pub fn question_text_preview(answer: &QuestionAnswer) -> String {
    answer
        .question_text
        .iter()
        .map(|(_, text)| collapsed(text))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn question_answer_preview(answer: &QuestionAnswer) -> String {
    let answers: Vec<String> = answer
        .answers
        .iter()
        .map(|(_, answer)| question_answer_text(answer))
        .filter(|text| !text.is_empty())
        .collect();
    let attachments: Vec<&str> = answer
        .attachments
        .iter()
        .flat_map(|(_, attachments)| attachments.iter().map(|a| a.name.as_str()))
        .collect();
    let preview = if !answers.is_empty() {
        answers.join(" · ")
    } else if !attachments.is_empty() {
        attachments.join(", ")
    } else {
        answer
            .question_text
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    collapsed(&preview)
}

pub fn has_question_answer(answer: &QuestionAnswer) -> bool {
    answer
        .answers
        .iter()
        .any(|(_, answer)| !question_answer_text(answer).is_empty())
        || answer
            .attachments
            .iter()
            .any(|(_, attachments)| !attachments.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{Attachment, AttachmentKind, RuntimeRequestId};

    fn answer(question_text: &[(&str, &str)]) -> QuestionAnswer {
        QuestionAnswer {
            request: RuntimeRequestId::new("request-1").unwrap(),
            answers: vec![(
                "scope".into(),
                Answer::Text("Use the private repository".into()),
            )],
            attachments: vec![],
            question_text: question_text
                .iter()
                .map(|(id, text)| ((*id).into(), (*text).into()))
                .collect(),
        }
    }

    #[test]
    fn joins_the_question_texts() {
        assert_eq!(
            question_text_preview(&answer(&[
                ("scope", "Which repository?"),
                ("name", "What name?")
            ])),
            "Which repository? · What name?"
        );
    }

    #[test]
    fn normalizes_whitespace_and_skips_blank_texts() {
        assert_eq!(
            question_text_preview(&answer(&[("scope", "Which\nrepository?"), ("x", "  ")])),
            "Which repository?"
        );
    }

    #[test]
    fn returns_an_empty_string_without_question_texts() {
        assert_eq!(question_text_preview(&answer(&[])), "");
    }

    #[test]
    fn previews_answers_before_attachments_and_questions() {
        let mut value = answer(&[("scope", "Which\u{a0} repository?")]);
        value.answers.push((
            "tags".into(),
            Answer::Choices(vec!["one".into(), String::new(), "two".into()]),
        ));
        assert_eq!(
            question_answer_preview(&value),
            "Use the private repository · one, two"
        );
        assert!(has_question_answer(&value));

        value.answers = vec![("scope".into(), Answer::Text(String::new()))];
        value.attachments = vec![(
            "scope".into(),
            vec![Attachment {
                kind: AttachmentKind::File,
                source: None,
                id: "file-1".into(),
                name: "spec.txt".into(),
                mime_type: "text/plain".into(),
                path: "/tmp/spec.txt".into(),
                size: 4,
            }],
        )];
        assert_eq!(question_answer_preview(&value), "spec.txt");
        assert!(has_question_answer(&value));

        value.attachments = vec![("scope".into(), vec![])];
        assert_eq!(question_answer_preview(&value), "Which repository?");
        assert!(!has_question_answer(&value));
    }
}
