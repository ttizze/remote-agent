//! Using a template prepares editable input; sending remains the user's action.
use crate::presentation::markdown::{ArtifactTemplate, ArtifactTemplateKind};

pub(crate) fn append(
    text: &str,
    template: &ArtifactTemplate,
    busy: bool,
) -> Result<String, &'static str> {
    template.validate()?;
    let skill = format!("${}", template.skill_name);
    use ArtifactTemplateKind::*;
    let prompt = match template.artifact_kind {
        Document => format!("Create a document using this {skill} about…"),
        Presentation => format!("Create a presentation using the {skill} template about…"),
        Spreadsheet => format!("Create a spreadsheet using this {skill} about…"),
        Site => format!("Create a Site using this {skill} about…"),
        GoogleDocs => format!("Create a Google Doc using this {skill} about…"),
        GoogleSlides => format!("Create a Google Slides presentation using this {skill} about…"),
        GoogleSheets => format!("Create a Google Sheet using this {skill} about…"),
        Image => format!("Create an image using this {skill} of…"),
        Email => format!("Draft an email using this {skill} about…"),
        Slack => format!("Draft a Slack message using this {skill} about…"),
    };
    let trimmed = text.trim_end();
    if let Some(prefix) = trimmed.strip_suffix(&prompt)
        && (prefix.is_empty() || prefix.ends_with(char::is_whitespace))
    {
        return Ok(text.into());
    }
    if busy {
        return Err("入力欄の処理が終わってからテンプレートを追加してください。");
    }
    let separator = if text.is_empty() || text.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    Ok(format!("{text}{separator}{prompt}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn template(kind: ArtifactTemplateKind) -> ArtifactTemplate {
        ArtifactTemplate {
            artifact_kind: kind,
            display_name: "Report".into(),
            gallery_kind: None,
            skill_directory: "/skills/artifact-template-report".into(),
            skill_name: "artifact-template-report".into(),
        }
    }
    #[test]
    fn native_action_is_scoped_and_only_changes_editable_text() {
        use crate::state::{Draft, DraftKey, Event, Intent, Snapshot, reduce};
        use std::sync::Arc;
        let key = DraftKey::from("/project");
        let mut snapshot = Snapshot::default();
        snapshot.connection.storage_scope = "host-one".into();
        let draft = Arc::new(Draft {
            text: "Keep my draft".into(),
            effort: Some("high".into()),
            contexts: vec![agent_protocol::context::MessageContext {
                id: "context".into(),
                label: "Skill".into(),
                content: agent_protocol::context::ContextContent::Skill {
                    name: "review".into(),
                    path: "/skills/review/SKILL.md".into(),
                    description: "Review".into(),
                },
            }],
            ..Default::default()
        });
        Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), draft.clone());
        let intent = |scope: &str| {
            Event::Intent(Intent::UseArtifactTemplate {
                storage_scope: scope.into(),
                thread_id: key.clone(),
                template: template(ArtifactTemplateKind::Document),
            })
        };
        let (ignored, effects) = reduce(&snapshot, "", intent("host-two"));
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(&ignored.input.drafts, &snapshot.input.drafts));
        let (changed, effects) = reduce(&snapshot, "", intent("host-one"));
        assert!(
            effects.is_empty(),
            "using a template does not start a request"
        );
        let mut expected = (*draft).clone();
        expected.text =
            "Keep my draft Create a document using this $artifact-template-report about…".into();
        assert_eq!(*changed.input.drafts[&key], expected);
        assert!(changed.sessions.pending_submissions.is_empty());
        let (same, _) = reduce(&changed, "", intent("host-one"));
        assert!(Arc::ptr_eq(&same.input.drafts, &changed.input.drafts));
    }
    #[test]
    fn using_a_template_preserves_input_and_does_not_duplicate_a_prompt() {
        let template = template(ArtifactTemplateKind::Document);
        let prompt = "Create a document using this $artifact-template-report about…";
        assert_eq!(append("", &template, false).unwrap(), prompt);
        assert_eq!(
            append("Existing", &template, false).unwrap(),
            format!("Existing {prompt}")
        );
        assert_eq!(
            append("Existing\n", &template, false).unwrap(),
            format!("Existing\n{prompt}")
        );
        let existing = format!("Existing\n{prompt}  \n");
        assert_eq!(append(&existing, &template, false).unwrap(), existing);
        assert_eq!(append(&existing, &template, true).unwrap(), existing);
        assert!(append("Existing", &template, true).is_err());
        assert_eq!(
            append(&format!("x{prompt}"), &template, false).unwrap(),
            format!("x{prompt} {prompt}")
        );
        let mut invalid = template;
        invalid.skill_name = "invalid".into();
        assert!(append("", &invalid, false).is_err());
    }
    #[test]
    fn every_template_kind_prepares_its_own_artifact() {
        use ArtifactTemplateKind::*;
        for (kind, start) in [
            (Presentation, "Create a presentation"),
            (Spreadsheet, "Create a spreadsheet"),
            (Site, "Create a Site"),
            (GoogleDocs, "Create a Google Doc"),
            (GoogleSlides, "Create a Google Slides presentation"),
            (GoogleSheets, "Create a Google Sheet"),
            (Image, "Create an image"),
            (Email, "Draft an email"),
            (Slack, "Draft a Slack message"),
        ] {
            let text = append("", &template(kind), false).unwrap();
            assert!(text.starts_with(start), "{text}");
            assert!(text.contains("$artifact-template-report"));
        }
    }
}
