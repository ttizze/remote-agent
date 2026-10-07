//! One-line previews of the answers given to a question request.
use super::QuestionAnswer;
use crate::js_text::collapse_js_spaces;
use agent_domain::Answer;

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
        .map(|(_, text)| collapse_js_spaces(text))
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
    collapse_js_spaces(&preview)
}

/// A file attached to an answer; images also show a preview.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AnswerHistoryFile {
    /// The attachment to download.
    pub id: String,
    pub name: String,
    pub image: bool,
}

/// One question of an answered request as its expanded row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AnswerHistoryQuestion {
    pub question_id: String,
    pub question: Option<String>,
    pub answer: Option<String>,
    pub files: Vec<AnswerHistoryFile>,
}

/// Every question that has a text, an answer or files, each once, in the
/// order of the question texts, then the answers, then the files.
pub fn question_answer_history(answer: &QuestionAnswer) -> Vec<AnswerHistoryQuestion> {
    let mut ids: Vec<&str> = vec![];
    let keys = answer
        .question_text
        .iter()
        .map(|(id, _)| id)
        .chain(answer.answers.iter().map(|(id, _)| id))
        .chain(answer.attachments.iter().map(|(id, _)| id));
    for id in keys {
        if !ids.contains(&id.as_str()) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .map(|id| AnswerHistoryQuestion {
            question_id: id.into(),
            question: answer
                .question_text
                .iter()
                .find(|(question, text)| question == id && !text.is_empty())
                .map(|(_, text)| text.clone()),
            answer: answer
                .answers
                .iter()
                .find(|(question, _)| question == id)
                .map(|(_, answer)| question_answer_text(answer))
                .filter(|text| !text.is_empty()),
            files: answer
                .attachments
                .iter()
                .filter(|(question, _)| question == id)
                .flat_map(|(_, files)| files)
                .map(|file| AnswerHistoryFile {
                    id: file.id.clone(),
                    name: file.name.clone(),
                    image: file.kind == agent_domain::AttachmentKind::Image,
                })
                .collect(),
        })
        .collect()
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

    fn file(kind: AttachmentKind, id: &str, name: &str) -> Attachment {
        Attachment {
            kind,
            source: None,
            id: id.into(),
            name: name.into(),
            mime_type: "text/plain".into(),
            path: name.into(),
            size: 4,
        }
    }

    #[test]
    fn renders_attachment_only_questions_alongside_text_answers() {
        for answers in [
            vec![],
            vec![
                ("text".to_owned(), Answer::Text("Text-only answer".into())),
                ("file".to_owned(), Answer::Text("Answer with a file".into())),
            ],
        ] {
            let value = QuestionAnswer {
                request: RuntimeRequestId::new("question-request").unwrap(),
                answers: answers.clone(),
                question_text: vec![
                    ("file".into(), "Provide a spec".into()),
                    ("image".into(), "Provide a screenshot".into()),
                ],
                attachments: vec![
                    (
                        "file".into(),
                        vec![file(AttachmentKind::File, "spec", "spec.txt")],
                    ),
                    (
                        "image".into(),
                        vec![file(AttachmentKind::Image, "shot", "shot.png")],
                    ),
                ],
            };
            let history = question_answer_history(&value);
            let questions: Vec<_> = history
                .iter()
                .filter_map(|q| q.question.as_deref())
                .collect();
            assert_eq!(questions, ["Provide a spec", "Provide a screenshot"]);
            let files: Vec<_> = history.iter().flat_map(|q| &q.files).collect();
            assert_eq!(files.len(), 2);
            assert_eq!(files[0].name, "spec.txt");
            assert!(files[1].image);
            for (_, answer) in &answers {
                let text = question_answer_text(answer);
                assert!(
                    history
                        .iter()
                        .any(|q| q.answer.as_deref() == Some(text.as_str()))
                );
            }
        }
    }
}
