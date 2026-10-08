use super::limit_section;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct GeneratedPrContent {
    pub title: String,
    pub body: String,
}

pub(crate) fn pr_content_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "additionalProperties":false,
        "required":["title","body"],
        "properties": {
            "title":{"type":"string","maxLength":120},
            "body":{"type":"string"}
        }
    })
}

pub(crate) fn pr_content_prompt(
    base_branch: &str,
    head_branch: &str,
    commits: &str,
    diff_stat: &str,
    diff_patch: &str,
    template: Option<&str>,
) -> String {
    let structure = template.map_or_else(
        || {
            "- body must be markdown and include headings '## Summary' and '## Testing'\n- under Summary, provide short bullet points\n- under Testing, include bullet points with concrete checks or 'Not run' where appropriate"
        },
        |_| {
            "- body must be markdown and follow the repository change request template structure\n- fill in the template sections appropriately for this change\n- drop HTML comments from the template in the generated body\n- keep the template's markdown structure"
        },
    );
    let commits = limit_section(commits, 12_000);
    let diff_stat = limit_section(diff_stat, 12_000);
    let diff_patch = limit_section(diff_patch, 40_000);
    let template_section = template
        .map(|template| {
            format!(
                "Repository change request template:\n{}",
                limit_section(template, 8_000)
            )
        })
        .unwrap_or_default();
    format!(
        "You write source control change request content.\nReturn a JSON object with keys: title, body.\nRules:\n- title should be concise and specific\n{structure}\n{}\nBase branch: {base_branch}\nHead branch: {head_branch}\n\nCommits:\n{commits}\n\nDiff stat:\n{diff_stat}\n\nDiff patch:\n{diff_patch}",
        template_section,
    )
}

pub(crate) fn sanitize_pr_content(mut content: GeneratedPrContent) -> GeneratedPrContent {
    content.title = content.title.trim().replace(['\n', '\r'], " ");
    if content.title.is_empty() {
        content.title = "Update project files".into();
    }
    if content.title.chars().count() > 120 {
        content.title = content.title.chars().take(120).collect();
        content.title = content.title.trim_end().to_owned();
    }
    content.body = content.body.replace("\r\n", "\n").replace('\r', "\n");
    content.body = strip_html_comments(&content.body).trim().to_owned();
    content
}

fn strip_html_comments(value: &str) -> String {
    let mut result = value.to_owned();
    loop {
        let Some(start) = result.find("<!--") else {
            break;
        };
        let end = result[start + 4..]
            .find("-->")
            .map(|offset| start + 4 + offset + 3)
            .unwrap_or(result.len());
        result.replace_range(start..end, "");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pr_content_has_a_safe_title() {
        let content = sanitize_pr_content(GeneratedPrContent::default());
        assert_eq!(content.title, "Update project files");
        assert!(content.body.is_empty());
    }

    #[test]
    fn generated_pr_bodies_drop_template_comments() {
        let content = sanitize_pr_content(GeneratedPrContent {
            title: "Title".into(),
            body: "<!-- instructions -->\n## Summary\n\nDetails".into(),
        });
        assert_eq!(content.body, "## Summary\n\nDetails");
    }
}
