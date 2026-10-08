use super::limit_section;
use serde::{Deserialize, Serialize};

/// Structured output requested from the Host's configured text provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct GeneratedCommitMessage {
    pub subject: String,
    pub body: String,
    pub branch: String,
}

pub(crate) fn commit_message_schema(include_branch: bool) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "subject".into(),
        serde_json::json!({"type":"string","maxLength":72}),
    );
    properties.insert("body".into(), serde_json::json!({"type":"string"}));
    if include_branch {
        properties.insert(
            "branch".into(),
            serde_json::json!({"type":"string","maxLength":80}),
        );
    }
    let required = if include_branch {
        serde_json::json!(["subject", "body", "branch"])
    } else {
        serde_json::json!(["subject", "body"])
    };
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

pub(crate) fn commit_message_prompt(
    branch: Option<&str>,
    staged_summary: &str,
    staged_patch: &str,
    include_branch: bool,
) -> String {
    let branch_rule = if include_branch {
        "- branch must be a short semantic git branch fragment for this change"
    } else {
        ""
    };
    format!(
        "You write concise git commit messages.\nReturn a JSON object with keys: {}.\nRules:\n- subject must be imperative, <= 72 chars, and no trailing period\n- body can be empty string or short bullet points\n{}- capture the primary user-visible or developer-visible change\n\nBranch: {}\n\nStaged files:\n{}\n\nStaged patch:\n{}",
        if include_branch {
            "subject, body, branch"
        } else {
            "subject, body"
        },
        if branch_rule.is_empty() {
            String::new()
        } else {
            format!("{branch_rule}\n")
        },
        branch.unwrap_or("(detached)"),
        limit_section(staged_summary, 6_000),
        limit_section(staged_patch, 40_000),
    )
}

pub(crate) fn parse_custom_commit_message(value: &str) -> GeneratedCommitMessage {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    let subject = lines.next().unwrap_or_default().trim().to_owned();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    GeneratedCommitMessage {
        subject,
        body,
        branch: String::new(),
    }
}

pub(crate) fn sanitize_commit_message(message: GeneratedCommitMessage) -> GeneratedCommitMessage {
    let mut subject = message.subject.trim().replace(['\n', '\r'], " ");
    while subject.ends_with('.') {
        subject.pop();
        subject = subject.trim_end().to_owned();
    }
    if subject.is_empty() {
        subject = "Update project files".into();
    }
    if subject.chars().count() > 72 {
        subject = subject.chars().take(72).collect::<String>();
        subject = subject.trim_end().to_owned();
    }
    let body = message.body.replace("\r\n", "\n").replace('\r', "\n");
    let branch = sanitize_branch_fragment(&message.branch)
        .split('/')
        .map(|part| {
            part.split('-')
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("-")
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    GeneratedCommitMessage {
        subject,
        body: body.trim().to_owned(),
        branch,
    }
}

fn sanitize_branch_fragment(raw: &str) -> String {
    let chars: Vec<char> = raw.trim().chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(index, character)| {
            if *character == '/' {
                let surrounded_by_space = chars
                    .get(index.wrapping_sub(1))
                    .is_some_and(|value| value.is_whitespace())
                    || chars
                        .get(index + 1)
                        .is_some_and(|value| value.is_whitespace());
                if surrounded_by_space {
                    '-'
                } else {
                    '/'
                }
            } else if character.is_ascii_alphanumeric() || matches!(*character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
}

pub(crate) fn format_commit_message(message: &GeneratedCommitMessage) -> String {
    if message.body.trim().is_empty() {
        message.subject.trim().to_owned()
    } else {
        format!("{}\n\n{}", message.subject.trim(), message.body.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_messages_split_and_normalize_lines() {
        let parsed = parse_custom_commit_message("  Add files.  \r\n\r\n Explain\rmore ");
        assert_eq!(parsed.subject, "Add files.");
        assert_eq!(parsed.body, "Explain\nmore");
    }

    #[test]
    fn generated_subjects_are_bounded_and_period_free() {
        let message = sanitize_commit_message(GeneratedCommitMessage {
            subject: format!("{}.", "a".repeat(100)),
            body: " details ".into(),
            branch: "Feature / New API".into(),
        });
        assert_eq!(message.subject.chars().count(), 72);
        assert!(!message.subject.ends_with('.'));
        assert_eq!(message.branch, "feature-new-api");
        assert_eq!(format_commit_message(&message).lines().count(), 3);
    }

    #[test]
    fn generated_feature_names_keep_a_feature_namespace() {
        let message = sanitize_commit_message(GeneratedCommitMessage {
            subject: "Implement branch naming".into(),
            body: String::new(),
            branch: "Feature/Branch Naming".into(),
        });
        assert_eq!(message.branch, "feature/branch-naming");
    }
}
