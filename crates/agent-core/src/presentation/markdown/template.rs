//! Codex artifact-template directives retain their native card metadata.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArtifactTemplateGallery {
    Imagegen,
    ProductDesign,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtifactTemplate {
    pub artifact_kind: ArtifactTemplateKind,
    pub display_name: String,
    pub gallery_kind: Option<ArtifactTemplateGallery>,
    pub skill_directory: String,
    pub skill_name: String,
}

impl ArtifactTemplate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.display_name.trim().is_empty()
            || self.display_name.len() > 1024
            || self.display_name.chars().any(char::is_control)
        {
            return Err("template name is invalid");
        }
        let path = &self.skill_directory;
        let absolute = agent_protocol::paths::is_windows_absolute_host_path(path)
            || path.starts_with('/') && !path.starts_with("//")
            || path
                .strip_prefix("//")
                .and_then(|path| path.split_once('/'))
                .is_some_and(|(server, path)| {
                    !server.is_empty() && !path.split('/').next().unwrap_or_default().is_empty()
                });
        if !absolute || path.len() > 4096 || path.chars().any(char::is_control) {
            return Err("template skill directory must be an absolute Host path");
        }
        if !self
            .skill_name
            .strip_prefix("artifact-template-")
            .is_some_and(|name| !name.is_empty())
            || self.skill_name.len() > 512
            || self.skill_name.chars().any(|c| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(
                        c,
                        '$' | '/'
                            | '\\'
                            | '"'
                            | '\''
                            | '<'
                            | '>'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                    )
            })
        {
            return Err("template skill name is invalid");
        }
        Ok(())
    }
    pub fn presentation_label(&self) -> &'static str {
        use ArtifactTemplateKind::*;
        match self.artifact_kind {
            Document => "Document template",
            Presentation => "Presentation template",
            Spreadsheet => "Spreadsheet template",
            Site => "Site template",
            GoogleDocs => "Google Doc template",
            GoogleSlides => "Google Slides template",
            GoogleSheets => "Google Sheet template",
            Image => "Image template",
            Email => "Email template",
            Slack => "Slack template",
        }
    }
}

/// Called only for complete paragraph lines outside code/math spans.
pub(super) fn parse(source: &str) -> Option<ArtifactTemplate> {
    let attributes = source
        .trim()
        .strip_prefix("::artifact-template{")?
        .strip_suffix('}')?;
    let mut attributes = attributes_map(attributes)?;
    let template = ArtifactTemplate {
        artifact_kind: serde_json::from_value(serde_json::Value::String(
            attributes.remove("artifact_kind")?,
        ))
        .ok()?,
        display_name: attributes.remove("display_name")?.trim().into(),
        gallery_kind: attributes
            .remove("gallery_kind")
            .map(|value| serde_json::from_value(serde_json::Value::String(value)))
            .transpose()
            .ok()?,
        skill_directory: attributes.remove("skill_directory")?,
        skill_name: attributes.remove("skill_name")?,
    };
    template.validate().ok()?;
    Some(template)
}

fn attributes_map(mut source: &str) -> Option<BTreeMap<String, String>> {
    let mut attributes = BTreeMap::new();
    while !source.trim().is_empty() {
        source = source.trim_start();
        let end = source.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))?;
        if end == 0 {
            return None;
        }
        let name = &source[..end];
        source = source[end..].trim_start().strip_prefix('=')?.trim_start();
        let (value, rest) = attribute_value(source)?;
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return None;
        }
        if attributes.insert(name.into(), entities(&value)).is_some() {
            return None;
        }
        source = rest;
    }
    Some(attributes)
}

fn attribute_value(source: &str) -> Option<(String, &str)> {
    let quote = source.chars().next()?;
    if !matches!(quote, '"' | '\'') {
        let end = source.find(char::is_whitespace).unwrap_or(source.len());
        return Some((source[..end].into(), &source[end..]));
    }
    let mut value = String::new();
    let mut chars = source[1..].char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        if character == quote {
            return Some((value, &source[offset + 2..]));
        }
        if character == '\\' && chars.peek().is_some_and(|(_, next)| *next == quote) {
            value.push(chars.next()?.1);
        } else {
            value.push(character);
        }
    }
    None
}

fn entities(source: &str) -> String {
    let mut output = String::new();
    let mut offset = 0;
    for (start, _) in source.match_indices('&') {
        if start < offset {
            continue;
        }
        let Some(end) = source[start + 1..]
            .char_indices()
            .take(32)
            .find(|(_, c)| *c == ';')
            .map(|(end, _)| start + end + 1)
        else {
            continue;
        };
        let entity = &source[start + 1..end];
        let decoded = if let Some(number) = entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
        {
            (!number.is_empty() && number.bytes().all(|c| c.is_ascii_hexdigit()))
                .then(|| ::markdown::decode_numeric(number, 16))
        } else if let Some(number) = entity.strip_prefix('#') {
            (!number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()))
                .then(|| ::markdown::decode_numeric(number, 10))
        } else {
            ::markdown::decode_named(entity, true)
        };
        if let Some(decoded) = decoded {
            output.push_str(&source[offset..start]);
            output.push_str(&decoded);
            offset = end + 1;
        }
    }
    output.push_str(&source[offset..]);
    output
}

pub(super) fn blocks(
    node: &super::Node,
    source: &str,
    context: &super::Context<'_>,
    mut style: super::MarkdownStyle,
    output: &mut Vec<super::MarkdownBlock>,
) -> bool {
    let Some(position) = node.position() else {
        return false;
    };
    let range = position.start.offset..position.end.offset;
    if !source[range.clone()].contains("::artifact-template{") {
        return false;
    }
    fn code_ranges(node: &super::Node, ranges: &mut Vec<std::ops::Range<usize>>) {
        if matches!(
            node,
            super::Node::InlineCode(_) | super::Node::InlineMath(_)
        ) && let Some(position) = node.position()
        {
            ranges.push(position.start.offset..position.end.offset);
        }
        for child in node.children().into_iter().flatten() {
            code_ranges(child, ranges);
        }
    }
    let mut ignored = Vec::new();
    code_ranges(node, &mut ignored);
    let mut offset = range.start;
    let mut previous = offset;
    let mut found = false;
    for line in source[range.clone()].split_inclusive('\n') {
        let mut content = line.trim();
        if style.quoted {
            while let Some(rest) = content.strip_prefix('>') {
                content = rest.trim_start();
            }
        }
        let start = offset + line.find("::artifact-template").unwrap_or_default();
        if !ignored.iter().any(|range| range.contains(&start))
            && let Some(template) = parse(content)
        {
            let before = output.len();
            super::append_fragment(&source[previous..offset], context, style.clone(), output);
            if output.len() != before {
                style.marker = None;
            }
            let text = format!(
                "{} ({})",
                template.display_name,
                template.presentation_label()
            );
            output.push(super::MarkdownBlock::Paragraph {
                runs: vec![super::MarkdownRun {
                    text,
                    template: Some(template),
                    ..Default::default()
                }],
                style: style.clone(),
            });
            style.marker = None;
            previous = offset + line.len();
            found = true;
        }
        offset += line.len();
    }
    if found {
        super::append_fragment(&source[previous..range.end], context, style, output);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    const DIRECTIVE: &str = r#"::artifact-template{skill_name="artifact-template-report" skill_directory="/skills/artifact-template-report" display_name="Report &amp; tables" artifact_kind="document"}"#;
    fn runs(source: &str) -> Vec<super::super::MarkdownRun> {
        super::super::markdown_runs(&super::super::markdown_blocks(source.into()))
            .into_iter()
            .cloned()
            .collect()
    }
    #[test]
    fn template_lines_survive_surrounding_text_lists_and_disclosures() {
        for source in [
            format!("Before\n{DIRECTIVE}\nAfter"),
            format!("<details><summary>More</summary>\n\n{DIRECTIVE}\n\n</details>"),
            format!("- {DIRECTIVE}"),
            format!("> {DIRECTIVE}"),
            format!("> Before\n> {DIRECTIVE}\n> After"),
        ] {
            let runs = runs(&source);
            let cards: Vec<_> = runs.iter().filter(|run| run.template.is_some()).collect();
            assert_eq!(cards.len(), 1, "{source}");
            assert_eq!(cards[0].text, "Report & tables (Document template)");
            assert_eq!(
                cards[0].template.as_ref().unwrap().skill_name,
                "artifact-template-report"
            );
            if source.contains("Before") {
                assert!(runs.iter().any(|run| run.text.contains("Before")));
                assert!(runs.iter().any(|run| run.text.contains("After")));
            }
        }
    }
    #[test]
    fn code_examples_and_partial_streams_are_not_cards() {
        for source in [
            format!("`{DIRECTIVE}`"),
            format!("```text\n{DIRECTIVE}\n```"),
            format!("`multiline\n{DIRECTIVE}\ncode`"),
            format!("prefix {DIRECTIVE}"),
            DIRECTIVE[..DIRECTIVE.len() - 1].into(),
        ] {
            let runs = runs(&source);
            assert!(runs.iter().all(|run| run.template.is_none()), "{source}");
            assert!(!runs.is_empty(), "literal source stays readable");
        }
        let long = "&".repeat(100_000);
        assert_eq!(entities(&long), long);
    }
    #[test]
    fn directives_keep_kind_path_and_gallery_without_executing_anything() {
        let template = parse(DIRECTIVE).unwrap();
        assert_eq!(template.display_name, "Report & tables");
        assert_eq!(template.artifact_kind, ArtifactTemplateKind::Document);
        assert_eq!(template.skill_name, "artifact-template-report");
        assert_eq!(template.skill_directory, "/skills/artifact-template-report");
        assert_eq!(template.presentation_label(), "Document template");
        for path in [
            r"C:\Users\skills\artifact-template-report",
            r"\\server\share\template",
            "//server/share/template",
        ] {
            assert_eq!(
                parse(&DIRECTIVE.replace("/skills/artifact-template-report", path))
                    .unwrap()
                    .skill_directory,
                path
            );
        }
        let image = DIRECTIVE.replace(
            "artifact_kind=\"document\"",
            "artifact_kind=image gallery_kind=imagegen",
        );
        assert_eq!(
            parse(&image).unwrap().gallery_kind,
            Some(ArtifactTemplateGallery::Imagegen)
        );
        assert_eq!(entities("&#65; &#x1F600; &bogus; &"), "A 😀 &bogus; &");
    }
    #[test]
    fn incomplete_and_invalid_directives_remain_literal_markdown() {
        for source in [
            DIRECTIVE[..DIRECTIVE.len() - 1].to_owned(),
            DIRECTIVE.replace("document", "unknown"),
            DIRECTIVE.replace("/skills/artifact-template-report", "relative"),
            DIRECTIVE.replace(
                "artifact-template-report\" skill_directory",
                "bad\" skill_directory",
            ),
            DIRECTIVE.replace("display_name=\"Report &amp; tables\"", "display_name=\"\""),
            DIRECTIVE.replace(
                "artifact_kind=\"document\"",
                "artifact_kind=document gallery_kind=unknown",
            ),
        ] {
            assert!(parse(&source).is_none(), "{source}");
        }
        assert!(attributes_map("name='one' name='two'").is_none());
        assert!(attributes_map("name='unfinished").is_none());
    }
}
