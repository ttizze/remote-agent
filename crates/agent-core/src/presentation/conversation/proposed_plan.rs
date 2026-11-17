//! Pinned proposedPlanText/proposedPlan behavior, shared with native exports.
use crate::text;
use regex::Regex;
use std::sync::LazyLock;

// JavaScript's multiline anchors also recognize lone CR and Unicode line
// separators. The title helper deliberately scans lines, even inside fences.
static TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:^|[\r\n\x{{2028}}\x{{2029}}]){s}{{0,3}}#{{1,6}}{s}+([^\r\n\x{{2028}}\x{{2029}}]+)(?:$|[\r\n\x{{2028}}\x{{2029}}])",
        s = text::SPACE,
    )).unwrap()
});
static HEADING_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^{s}{{0,3}}#{{1,6}}{s}+", s = text::SPACE)).unwrap());
static SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^{s}{{0,3}}#{{1,6}}{s}+([^\r\n\x{{2028}}\x{{2029}}]+)$",
        s = text::SPACE,
    ))
    .unwrap()
});

pub(super) fn plan_title(markdown: &str) -> Option<String> {
    let matched = TITLE.captures(markdown)?;
    let title = text::trim(matched.get(1)?.as_str());
    (!title.is_empty()).then(|| title.to_owned())
}

pub fn plan_export(markdown: &str) -> (String, String) {
    let title = plan_title(markdown).unwrap_or_default();
    let mut name = String::new();
    for character in title.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character);
        } else if !"`'\".,!?()[]{}".contains(character) && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    (
        format!("{}.md", if name.is_empty() { "plan" } else { name }),
        format!("{}\n", text::trim_end(markdown)),
    )
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn plan_write_enabled(connected: bool, cwd: String) -> bool {
    connected && !cwd.is_empty()
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlanDisplay {
    pub body: String,
    pub preview: Option<String>,
    pub filename: String,
    pub contents: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn plan_display(markdown: String) -> PlanDisplay {
    let lines = text::trim_end(&markdown)
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    let mut start = usize::from(
        lines
            .first()
            .is_some_and(|line| HEADING_START.is_match(line)),
    );
    while lines
        .get(start)
        .is_some_and(|line| text::trim(line).is_empty())
    {
        start += 1;
    }
    if lines
        .get(start)
        .and_then(|line| SUMMARY.captures(line))
        .and_then(|matched| matched.get(1))
        .is_some_and(|title| text::trim(title.as_str()).eq_ignore_ascii_case("summary"))
    {
        start += 1;
        while lines
            .get(start)
            .is_some_and(|line| text::trim(line).is_empty())
        {
            start += 1;
        }
    }
    let body = lines[start..].join("\n");
    let collapsible = markdown.encode_utf16().count() > 900 || markdown.split('\n').count() > 20;
    let preview = collapsible.then(|| preview(&body, plan_title(&markdown).as_deref()));
    let (filename, contents) = plan_export(&markdown);
    PlanDisplay {
        body,
        preview,
        filename,
        contents,
    }
}

fn preview(body: &str, title: Option<&str>) -> String {
    let mut lines = Vec::new();
    let mut visible = 0;
    let mut more = false;
    for line in text::trim_end(body).split('\n') {
        let line = text::trim_end(line);
        let nonempty = !text::trim(line).is_empty();
        if nonempty && visible >= 10 {
            more = true;
            break;
        }
        lines.push(line);
        visible += usize::from(nonempty);
    }
    while lines.last().is_some_and(|line| text::trim(line).is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return title.unwrap_or("Plan preview unavailable.").into();
    }
    let mut result = lines.join("\n");
    if more {
        result.push_str("\n\n...");
    }
    result
}

#[cfg(test)]
mod tests;
