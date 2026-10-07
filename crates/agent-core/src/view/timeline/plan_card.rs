//! The proposed plan card: its title, the markdown it shows, the collapsed
//! preview of a long plan and the file it saves to.
use super::entries::{PlanStatus, ProposedPlan};
use crate::commands::build::{atx_heading, proposed_plan_title};
use crate::js_text::utf16_len;
use agent_domain::{PlanId, RunId};

/// A plan longer than this collapses behind "Expand plan".
const COLLAPSE_CHARACTERS: usize = 900;
const COLLAPSE_LINES: usize = 20;
const PREVIEW_LINES: usize = 8;

fn skip_blank<'a, 'b>(mut lines: &'a [&'b str]) -> &'a [&'b str] {
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines = &lines[1..];
    }
    lines
}

fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// The plan without its title heading or a leading "Summary" heading.
pub fn strip_displayed_plan_markdown(markdown: &str) -> String {
    let lines = split_lines(markdown.trim_end());
    let mut lines: &[&str] = if lines
        .first()
        .is_some_and(|line| atx_heading(line).is_some())
    {
        &lines[1..]
    } else {
        &lines
    };
    lines = skip_blank(lines);
    if lines
        .first()
        .and_then(|line| atx_heading(line))
        .is_some_and(|title| title.to_lowercase() == "summary")
    {
        lines = skip_blank(&lines[1..]);
    }
    lines.join("\n")
}

/// The first `max_lines` non-blank lines of the displayed plan, with "..."
/// when more follows.
pub fn collapsed_plan_preview(markdown: &str, max_lines: usize) -> String {
    let stripped = strip_displayed_plan_markdown(markdown);
    let mut preview: Vec<&str> = vec![];
    let mut visible = 0;
    let mut more = false;
    for line in split_lines(stripped.trim_end()) {
        let line = line.trim_end();
        let shown = !line.trim().is_empty();
        if shown && visible >= max_lines {
            more = true;
            break;
        }
        preview.push(line);
        if shown {
            visible += 1;
        }
    }
    while preview.last().is_some_and(|line| line.trim().is_empty()) {
        preview.pop();
    }
    if preview.is_empty() {
        return proposed_plan_title(markdown).unwrap_or_else(|| "Plan preview unavailable.".into());
    }
    if more {
        preview.extend(["", "..."]);
    }
    preview.join("\n")
}

fn sanitize_file_segment(input: &str) -> String {
    let lower = input.to_lowercase();
    let kept: String = lower
        .chars()
        .filter(|c| !"`'\".,!?()[]{}".contains(*c))
        .collect();
    let mut segment = String::new();
    for c in kept.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            segment.push(c);
        } else if !segment.ends_with('-') {
            segment.push('-');
        }
    }
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "plan".into()
    } else {
        segment.into()
    }
}

/// The file a downloaded or saved plan is named.
pub fn plan_markdown_filename(markdown: &str) -> String {
    format!(
        "{}.md",
        sanitize_file_segment(&proposed_plan_title(markdown).unwrap_or_else(|| "plan".into()))
    )
}

/// The saved contents: the plan with one trailing newline.
pub fn plan_markdown_for_export(markdown: &str) -> String {
    format!("{}\n", markdown.trim_end())
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlanCard {
    pub plan: PlanId,
    pub run: Option<RunId>,
    pub status: PlanStatus,
    pub title: String,
    pub markdown: String,
    /// What the card shows: the plan without its title.
    pub displayed_markdown: String,
    /// The preview a collapsed long plan shows; `None` when it never collapses.
    pub collapsed_preview: Option<String>,
    pub filename: String,
    pub export_markdown: String,
}

pub fn plan_card(plan: &ProposedPlan) -> PlanCard {
    let markdown = &plan.markdown;
    let collapses =
        utf16_len(markdown) > COLLAPSE_CHARACTERS || markdown.split('\n').count() > COLLAPSE_LINES;
    PlanCard {
        plan: plan.id.clone(),
        run: plan.run.clone(),
        status: plan.status,
        title: proposed_plan_title(markdown).unwrap_or_else(|| "Proposed plan".into()),
        markdown: markdown.clone(),
        displayed_markdown: strip_displayed_plan_markdown(markdown),
        collapsed_preview: collapses.then(|| collapsed_plan_preview(markdown, PREVIEW_LINES)),
        filename: plan_markdown_filename(markdown),
        export_markdown: plan_markdown_for_export(markdown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_the_redundant_title_heading_and_preserves_the_following_markdown_lines() {
        assert_eq!(
            collapsed_plan_preview("# Integrate RPC\n\n## Summary\n\n- step 1\n- step 2", 4),
            "- step 1\n- step 2"
        );
    }

    #[test]
    fn appends_an_overflow_marker_when_the_preview_truncates_remaining_content() {
        assert_eq!(
            collapsed_plan_preview("# Integrate RPC\n\n- step 1\n- step 2\n- step 3", 2),
            "- step 1\n- step 2\n\n..."
        );
    }

    #[test]
    fn drops_the_leading_title_heading_from_displayed_plan_markdown() {
        assert_eq!(
            strip_displayed_plan_markdown("# Integrate RPC\n\n## Summary\n\n- step 1\n"),
            "- step 1"
        );
    }

    #[test]
    fn preserves_non_summary_headings_after_dropping_the_title_heading() {
        assert_eq!(
            strip_displayed_plan_markdown("# Integrate RPC\n\n## Scope\n\n- step 1\n"),
            "## Scope\n\n- step 1"
        );
    }

    #[test]
    fn derives_a_stable_markdown_filename_from_the_plan_heading() {
        assert_eq!(
            plan_markdown_filename("# Integrate Effect RPC Into Server App"),
            "integrate-effect-rpc-into-server-app.md"
        );
    }

    #[test]
    fn falls_back_to_a_generic_filename_when_the_plan_has_no_heading() {
        assert_eq!(plan_markdown_filename("- step 1"), "plan.md");
    }

    #[test]
    fn a_long_plan_collapses_behind_its_preview() {
        let plan = |markdown: String| ProposedPlan {
            id: PlanId::new("plan").unwrap(),
            run: None,
            markdown,
            status: PlanStatus::Active,
            created_at: crate::sync::fixtures::at(),
            updated_at: crate::sync::fixtures::at(),
        };
        let short = plan_card(&plan("# Ship it\n\n- step".into()));
        assert_eq!(
            (short.title.as_str(), short.collapsed_preview),
            ("Ship it", None)
        );
        let steps: Vec<String> = (1..=21).map(|step| format!("- step {step}")).collect();
        let long = plan_card(&plan(format!("# Ship it\n{}", steps.join("\n"))));
        assert!(long.collapsed_preview.unwrap().ends_with("- step 8\n\n..."));
        assert_eq!(long.export_markdown.matches('\n').count(), 21 + 1);
    }
}
