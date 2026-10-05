use crate::*;
use std::collections::BTreeMap;

/// Paths have already been resolved and checked by the effect executor.
pub fn append_answer_attachments(
    answers: &Answers,
    attachments: &BTreeMap<String, Vec<Attachment>>,
) -> Result<Answers, &'static str> {
    validate_attachments(&attachments.values().flatten().cloned().collect::<Vec<_>>())?;
    let mut result = answers.clone();
    for (question, files) in attachments {
        if files.is_empty() {
            continue;
        }
        if files.iter().any(|file| file.path.is_empty()) {
            return Err("attachment-unavailable");
        }
        let references = files
            .iter()
            .map(|file| {
                format!(
                    "Attached {} {}: {}",
                    match file.kind {
                        AttachmentKind::Image => "image",
                        AttachmentKind::File => "file",
                    },
                    serde_json::to_string(&file.name).unwrap(),
                    serde_json::to_string(&file.path).unwrap(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        match result.get_mut(question) {
            Some(Answer::Choices(choices)) => choices.push(references),
            Some(Answer::Text(text)) if !text.is_empty() => {
                text.push_str("\n\n");
                text.push_str(&references);
            }
            _ => {
                result.insert(question.clone(), Answer::Text(references));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> Attachment {
        Attachment {
            kind: AttachmentKind::File,
            source: None,
            id: "file-1".into(),
            name: "spec \"final\".txt".into(),
            mime_type: "text/plain".into(),
            path: "/tmp/spec.txt".into(),
            size: 4,
        }
    }
    #[test]
    fn selected_values_and_quoted_paths_match_reference_answers() {
        let original = Answers::from([
            (
                "q".into(),
                Answer::Choices(vec!["First".into(), "Second".into()]),
            ),
            ("other".into(), Answer::Text("No file".into())),
        ]);
        let attachments = BTreeMap::from([("q".into(), vec![file()])]);
        let result = append_answer_attachments(&original, &attachments).unwrap();
        assert_eq!(
            result["q"],
            Answer::Choices(vec![
                "First".into(),
                "Second".into(),
                "Attached file \"spec \\\"final\\\".txt\": \"/tmp/spec.txt\"".into()
            ])
        );
        assert_eq!(result["other"], Answer::Text("No file".into()));
        assert_eq!(
            original["q"],
            Answer::Choices(vec!["First".into(), "Second".into()])
        );
        let special = append_answer_attachments(
            &Answers::new(),
            &BTreeMap::from([("__proto__".into(), vec![file()])]),
        )
        .unwrap();
        assert_eq!(special.keys().collect::<Vec<_>>(), vec!["__proto__"]);
        assert!(special["__proto__"].text().contains("/tmp/spec.txt"));
    }
    #[test]
    fn unavailable_paths_and_combined_attachment_limits_are_rejected() {
        let mut missing = file();
        missing.path.clear();
        assert!(
            append_answer_attachments(
                &Answers::new(),
                &BTreeMap::from([("q".into(), vec![missing])])
            )
            .is_err()
        );
        let attachments = (0..101)
            .map(|index| {
                let mut file = file();
                file.id = index.to_string();
                (index.to_string(), vec![file])
            })
            .collect();
        assert_eq!(
            append_answer_attachments(&Answers::new(), &attachments),
            Err("too-many-attachments")
        );
    }
}
