//! Codex result cards announced by `::artifact-template{…}`: a skill the user can reuse
//! to make a document, presentation, image and so on.
use super::citations::DirectiveAttributes;
use super::js_text::{js_space, js_trim};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactTemplateKind {
    Document,
    Presentation,
    Spreadsheet,
    Site,
    GoogleDocs,
    GoogleSlides,
    GoogleSheets,
    Image,
    Email,
    Slack,
}

impl ArtifactTemplateKind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "document" => Self::Document,
            "presentation" => Self::Presentation,
            "spreadsheet" => Self::Spreadsheet,
            "site" => Self::Site,
            "google-docs" => Self::GoogleDocs,
            "google-slides" => Self::GoogleSlides,
            "google-sheets" => Self::GoogleSheets,
            "image" => Self::Image,
            "email" => Self::Email,
            "slack" => Self::Slack,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactTemplateGalleryKind {
    Imagegen,
    ProductDesign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactTemplate {
    pub artifact_kind: ArtifactTemplateKind,
    pub display_name: String,
    pub gallery_kind: Option<ArtifactTemplateGalleryKind>,
    pub skill_directory: String,
    pub skill_name: String,
}

fn is_absolute_skill_directory(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    // `\\server\share` or `//server/share`: two non-empty segments after the marker.
    let unc = |separator: char| {
        let marker = format!("{separator}{separator}");
        value.strip_prefix(&marker).is_some_and(|rest| {
            rest.split_once(separator).is_some_and(|(server, share)| {
                !server.is_empty() && !share.starts_with(separator) && !share.is_empty()
            })
        })
    };
    (value.starts_with('/') && !value.starts_with("//")) || drive || unc('\\') || unc('/')
}

/// Mirrors the Codex result-card schema so malformed directives remain literal Markdown.
pub fn artifact_template(attributes: &DirectiveAttributes) -> Option<ArtifactTemplate> {
    let artifact_kind = ArtifactTemplateKind::parse(attributes.get("artifact_kind")?)?;
    let display_name = js_trim(attributes.get("display_name")?);
    let skill_directory = attributes.get("skill_directory")?;
    let skill_name = attributes.get("skill_name")?;
    let gallery_kind = match attributes.get("gallery_kind").map(String::as_str) {
        None => None,
        Some("imagegen") => Some(ArtifactTemplateGalleryKind::Imagegen),
        Some("product-design") => Some(ArtifactTemplateGalleryKind::ProductDesign),
        Some(_) => return None,
    };
    if display_name.is_empty()
        || !is_absolute_skill_directory(skill_directory)
        || !skill_name.starts_with("artifact-template-")
    {
        return None;
    }
    Some(ArtifactTemplate {
        artifact_kind,
        display_name: display_name.to_owned(),
        gallery_kind,
        skill_directory: skill_directory.clone(),
        skill_name: skill_name.clone(),
    })
}

pub fn artifact_template_use_prompt(template: &ArtifactTemplate) -> String {
    let skill = format!("${}", template.skill_name);
    match template.artifact_kind {
        ArtifactTemplateKind::Document => format!("Create a document using this {skill} about…"),
        ArtifactTemplateKind::Presentation => {
            format!("Create a presentation using the {skill} template about…")
        }
        ArtifactTemplateKind::Spreadsheet => {
            format!("Create a spreadsheet using this {skill} about…")
        }
        ArtifactTemplateKind::Site => format!("Create a Site using this {skill} about…"),
        ArtifactTemplateKind::GoogleDocs => {
            format!("Create a Google Doc using this {skill} about…")
        }
        ArtifactTemplateKind::GoogleSlides => {
            format!("Create a Google Slides presentation using this {skill} about…")
        }
        ArtifactTemplateKind::GoogleSheets => {
            format!("Create a Google Sheet using this {skill} about…")
        }
        ArtifactTemplateKind::Image => format!("Create an image using this {skill} of…"),
        ArtifactTemplateKind::Email => format!("Draft an email using this {skill} about…"),
        ArtifactTemplateKind::Slack => format!("Draft a Slack message using this {skill} about…"),
    }
}

pub fn artifact_template_presentation_label(kind: ArtifactTemplateKind) -> &'static str {
    match kind {
        ArtifactTemplateKind::Document => "Document template",
        ArtifactTemplateKind::Presentation => "Presentation template",
        ArtifactTemplateKind::Spreadsheet => "Spreadsheet template",
        ArtifactTemplateKind::Site => "Site template",
        ArtifactTemplateKind::GoogleDocs => "Google Doc template",
        ArtifactTemplateKind::GoogleSlides => "Google Slides template",
        ArtifactTemplateKind::GoogleSheets => "Google Sheet template",
        ArtifactTemplateKind::Image => "Image template",
        ArtifactTemplateKind::Email => "Email template",
        ArtifactTemplateKind::Slack => "Slack template",
    }
}

/// Adds the template's prompt to the draft unless the draft already ends with it.
pub fn append_artifact_template_use_prompt(draft: &str, template: &ArtifactTemplate) -> String {
    let prompt = artifact_template_use_prompt(template);
    let trimmed_draft = draft.trim_end_matches(js_space);
    if let Some(before) = trimmed_draft.strip_suffix(prompt.as_str())
        && before.chars().next_back().is_none_or(js_space)
    {
        return draft.to_owned();
    }
    let needs_leading_space = draft.chars().next_back().is_some_and(|c| !js_space(c));
    format!(
        "{draft}{}{prompt}",
        if needs_leading_space { " " } else { "" }
    )
}

#[cfg(test)]
#[path = "artifact_templates_tests.rs"]
mod tests;
