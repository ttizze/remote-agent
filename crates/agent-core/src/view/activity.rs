use agent_domain::{
    ACTIVITY_LINK_LIMIT, ACTIVITY_ROWS_LIMIT, ACTIVITY_STATUS_LIMIT, ACTIVITY_SUMMARY_LIMIT,
    ActivityDeepLink, bounded_activity_text, parse_activity_deep_link,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityRow {
    project_title: String,
    thread_title: String,
    phase: String,
    status: String,
    deep_link: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityContent {
    active_count: u32,
    activities: Vec<ActivityRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityDisplayRow {
    project: String,
    title: String,
    status: String,
    glyph: &'static str,
    color: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityDisplay {
    headline: String,
    attention: String,
    compact_label: String,
    summary: String,
    expanded_label: String,
    minimal_glyph: bool,
    glyph: &'static str,
    color: &'static str,
    deep_link: Option<String>,
    rows: Vec<ActivityDisplayRow>,
}

fn priority(phase: &str) -> u8 {
    match phase {
        "waiting_for_approval" | "waiting_for_input" => 0,
        "failed" => 1,
        "running" | "starting" => 2,
        _ => 3,
    }
}

fn glyph(phase: &str) -> &'static str {
    match phase {
        "waiting_for_approval" => "exclamationmark.circle.fill",
        "waiting_for_input" => "questionmark.circle.fill",
        "failed" => "xmark.octagon.fill",
        "completed" => "checkmark.circle.fill",
        "starting" => "circle.dotted",
        "stale" => "clock.arrow.circlepath",
        _ => "arrow.triangle.2.circlepath",
    }
}

fn color(phase: &str, light: bool, monochrome: bool, reduced: bool) -> &'static str {
    if reduced || phase == "stale" {
        return "secondary";
    }
    if monochrome {
        return "primary";
    }
    match (phase, light) {
        ("waiting_for_approval", true) => "#d97706",
        ("waiting_for_approval", false) => "#fcd34d",
        ("waiting_for_input", true) => "#4f46e5",
        ("waiting_for_input", false) => "#a5b4fc",
        ("failed", true) => "#dc2626",
        ("failed", false) => "#fca5a5",
        ("completed", true) => "#059669",
        ("completed", false) => "#6ee7b7",
        (_, true) => "#0284c7",
        (_, false) => "#7dd3fc",
    }
}

fn display(
    content: ActivityContent,
    stale: bool,
    light: bool,
    monochrome: bool,
    reduced: bool,
) -> ActivityDisplay {
    let mut rows = content.activities;
    if stale {
        for row in &mut rows {
            if !matches!(row.phase.as_str(), "completed" | "failed") {
                row.phase = "stale".into();
                row.status = "Out of date".into();
            }
        }
    }
    rows.sort_by_key(|row| priority(&row.phase));
    let attention = rows
        .iter()
        .filter(|row| {
            matches!(
                row.phase.as_str(),
                "waiting_for_approval" | "waiting_for_input"
            )
        })
        .count();
    let failed = rows.iter().any(|row| row.phase == "failed");
    let done = content.active_count == 0;
    let terminal_phase = if failed { "failed" } else { "completed" };
    let phase = if done {
        terminal_phase
    } else {
        rows.first().map_or("stale", |row| row.phase.as_str())
    };
    let attention_label = match attention {
        0 => String::new(),
        1 => "1 needs attention".into(),
        n => format!("{n} need attention"),
    };
    let active_label = if done {
        if failed {
            "Failed".into()
        } else {
            "Done".into()
        }
    } else if stale {
        "Out of date".into()
    } else {
        format!("{} active", content.active_count)
    };
    let compact_label = if attention > 0 {
        if phase == "waiting_for_approval" {
            "Approval".into()
        } else {
            "Input".into()
        }
    } else {
        active_label.clone()
    };
    let headline = if done {
        if failed {
            "Agent work failed".into()
        } else {
            "Agent work completed".into()
        }
    } else if stale {
        "Agent status out of date".into()
    } else {
        format!(
            "{} active agent{}",
            content.active_count,
            if content.active_count == 1 { "" } else { "s" }
        )
    };
    let deep_link = rows.first().and_then(|row| {
        (row.deep_link.encode_utf16().count() <= ACTIVITY_LINK_LIMIT
            && matches!(
                parse_activity_deep_link(&row.deep_link),
                Some(ActivityDeepLink::Thread { .. })
            ))
        .then(|| row.deep_link.clone())
    });
    ActivityDisplay {
        headline,
        attention: attention_label.clone(),
        summary: if attention_label.is_empty() {
            active_label.clone()
        } else {
            attention_label
        },
        expanded_label: if done {
            active_label
        } else {
            content.active_count.to_string()
        },
        minimal_glyph: !rows.is_empty() && (attention > 0 || failed || done),
        compact_label,
        glyph: glyph(phase),
        color: color(phase, light, monochrome, reduced),
        deep_link,
        rows: rows
            .into_iter()
            .take(ACTIVITY_ROWS_LIMIT)
            .map(|row| ActivityDisplayRow {
                glyph: glyph(&row.phase),
                color: color(&row.phase, light, monochrome, reduced),
                project: bounded_activity_text(&row.project_title, ACTIVITY_SUMMARY_LIMIT),
                title: bounded_activity_text(&row.thread_title, ACTIVITY_SUMMARY_LIMIT),
                status: bounded_activity_text(&row.status, ACTIVITY_STATUS_LIMIT),
            })
            .collect(),
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn agent_activity_widget_json(
    json: String,
    stale: bool,
    light: bool,
    monochrome: bool,
    reduced: bool,
) -> String {
    let content = (json.len() <= 64 * 1024)
        .then(|| serde_json::from_str::<ActivityContent>(&json).ok())
        .flatten()
        .unwrap_or(ActivityContent {
            active_count: 0,
            activities: Vec::new(),
        });
    serde_json::to_string(&display(content, stale, light, monochrome, reduced))
        .expect("activity display data serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(phase: &str, title: &str) -> ActivityRow {
        ActivityRow {
            project_title: "Project".into(),
            thread_title: title.into(),
            phase: phase.into(),
            status: "Working".into(),
            deep_link: format!("remoteagent://threads/host/{title}"),
        }
    }
    #[test]
    fn attention_rows_order_first_and_define_count_tint_glyph_and_navigation() {
        let view = display(
            ActivityContent {
                active_count: 3,
                activities: vec![row("running", "work"), row("waiting_for_input", "input")],
            },
            false,
            false,
            false,
            false,
        );
        assert_eq!(view.rows[0].title, "input");
        assert_eq!(view.headline, "3 active agents");
        assert_eq!(view.attention, "1 needs attention");
        assert_eq!(view.color, "#a5b4fc");
        assert_eq!(view.glyph, "questionmark.circle.fill");
        assert_eq!(
            view.deep_link.as_deref(),
            Some("remoteagent://threads/host/input")
        );
        assert_eq!(view.rows[1].color, "#7dd3fc");
    }
    #[test]
    fn system_display_bounds_preserve_unicode_and_never_truncate_navigation_ids() {
        let mut oversized = row("running", &"😀".repeat(300));
        oversized.project_title = format!(" {} ", "p".repeat(121));
        oversized.status = "s".repeat(41);
        let view = display(
            ActivityContent {
                active_count: 9,
                activities: vec![oversized; 9],
            },
            false,
            false,
            false,
            false,
        );
        assert_eq!(view.rows.len(), 5);
        assert_eq!(view.headline, "9 active agents");
        assert_eq!(view.rows[0].project, format!("{}...", "p".repeat(117)));
        assert_eq!(view.rows[0].title, format!("{}...", "😀".repeat(58)));
        assert_eq!(view.rows[0].status, format!("{}...", "s".repeat(37)));
        assert!(view.deep_link.is_none());
    }
    #[test]
    fn stale_live_work_preserves_terminal_outcomes_and_reduces_treatment() {
        let view = display(
            ActivityContent {
                active_count: 2,
                activities: vec![row("running", "work"), row("completed", "done")],
            },
            true,
            false,
            false,
            false,
        );
        assert_eq!(view.headline, "Agent status out of date");
        assert_eq!(view.compact_label, "Out of date");
        assert_eq!(view.rows[0].status, "Out of date");
        assert_eq!(view.rows[0].color, "secondary");
        assert_eq!(view.rows[1].color, "#6ee7b7");
    }
    #[test]
    fn failure_wins_a_newer_success_when_every_agent_has_finished() {
        let view = display(
            ActivityContent {
                active_count: 0,
                activities: vec![row("completed", "done"), row("failed", "failure")],
            },
            false,
            true,
            false,
            false,
        );
        assert_eq!(view.headline, "Agent work failed");
        assert_eq!(view.compact_label, "Failed");
        assert_eq!(view.color, "#dc2626");
        let muted = display(
            ActivityContent {
                active_count: 1,
                activities: vec![row("running", "work")],
            },
            false,
            true,
            true,
            true,
        );
        assert_eq!(muted.color, "secondary");
    }
    #[test]
    fn external_and_protocol_relative_links_do_not_leave_the_app() {
        for link in [
            "https://example.test/",
            "//example.test/threads/host/thread",
            "remoteagent://threads/host/thread?url=private",
            "remoteagent://private@threads/host/thread",
        ] {
            let mut item = row("running", "work");
            item.deep_link = link.into();
            assert!(
                display(
                    ActivityContent {
                        active_count: 1,
                        activities: vec![item]
                    },
                    false,
                    false,
                    false,
                    false
                )
                .deep_link
                .is_none()
            );
        }
    }
}
