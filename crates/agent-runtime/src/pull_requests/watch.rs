use agent_domain::{
    PullRequestChecksState, PullRequestDetail, PullRequestLink, PullRequestMergeability,
    PullRequestSummary, PullRequestWatch, Timestamp,
};
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_WATCH_WAKES: u32 = 10;

/// The bounded message a Host queues when a watched request has actionable
/// provider changes. Text is produced here so every client gets the same
/// report and the Host only owns dispatching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestWatchWake {
    pub text: String,
    pub detail: String,
    pub failed: bool,
    pub exhausted: bool,
}

/// Bounded watch scheduling. Comment-only wakes stop polling after the
/// configured budget; check and conflict state remains observable on later
/// passes so a resolved conflict can wake the agent again.
pub fn next_watch(
    link: &PullRequestLink,
    now: &Timestamp,
    interval: Duration,
) -> Option<Timestamp> {
    let watch = link.watch.as_ref()?;
    if watch.wakes >= MAX_WATCH_WAKES {
        return None;
    }
    let millis = now.millis().checked_add(interval.as_millis() as i64)?;
    Timestamp::from_millis(millis).ok()
}

pub fn watch_is_due(link: &PullRequestLink, now: &Timestamp, interval: Duration) -> bool {
    let Some(watch) = link.watch.as_ref() else {
        return false;
    };
    if watch.wakes >= MAX_WATCH_WAKES {
        return false;
    }
    let elapsed = now.millis().saturating_sub(watch.started_at.millis());
    elapsed
        >= interval
            .as_millis()
            .saturating_mul((watch.wakes as u128).saturating_add(1)) as i64
}

pub fn start_watch(now: Timestamp, head_sha: Option<String>) -> PullRequestWatch {
    PullRequestWatch {
        started_at: now.clone(),
        head_sha,
        failed_checks: vec![],
        passed: false,
        remarks_through: Some(now),
        remark_ids: vec![],
        conflicting: false,
        wakes: 0,
    }
}

/// Folds one provider refresh into a durable watch without counting the same
/// terminal result on every later refresh. Comment-only wake budgeting is
/// handled by update_watch_detail, where the provider's remarks are present.
pub fn update_watch(watch: &mut PullRequestWatch, summary: &PullRequestSummary) {
    let head_moved = summary.head_sha != watch.head_sha;
    let previous_failed = watch.failed_checks.clone();
    let previous_passed = watch.passed;
    let previous_conflicting = watch.conflicting;
    if head_moved {
        watch.failed_checks.clear();
        watch.passed = false;
        watch.conflicting = false;
        watch.wakes = 0;
    }
    watch.head_sha = summary.head_sha.clone();
    if !summary.checks.is_empty() {
        watch.failed_checks = summary
            .checks
            .iter()
            .filter(|check| check.state == PullRequestChecksState::Failure)
            .map(|check| check.name.clone())
            .collect();
        watch.passed = summary.checks_state == PullRequestChecksState::Success
            && summary.mergeability != PullRequestMergeability::Conflicting;
    } else if summary.checks_state != PullRequestChecksState::Unknown {
        watch.passed = summary.checks_state == PullRequestChecksState::Success
            && summary.mergeability != PullRequestMergeability::Conflicting;
    }
    watch.conflicting = match summary.mergeability {
        PullRequestMergeability::Unknown => previous_conflicting,
        PullRequestMergeability::Conflicting => true,
        PullRequestMergeability::Mergeable => false,
    };
    let progress = head_moved
        || watch.failed_checks != previous_failed
        || watch.passed != previous_passed
        || watch.conflicting != previous_conflicting;
    if progress {
        watch.wakes = 0;
    }
}

/// Folds a detail refresh and returns one bounded wake report when the
/// refresh contains news the agent has not been told yet. When the provider
/// omits the viewer account, the request author is the available fallback.
pub fn update_watch_detail(
    watch: &mut PullRequestWatch,
    detail: &PullRequestDetail,
) -> Option<PullRequestWatchWake> {
    let before = watch.clone();
    update_watch(watch, &detail.summary);

    let newly_failed = watch
        .failed_checks
        .iter()
        .filter(|name| !before.failed_checks.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    let checks_passed = watch.passed && !before.passed;
    let conflicting = watch.conflicting && !before.conflicting;

    let own_login = detail
        .viewer
        .as_ref()
        .map(|actor| actor.login.to_ascii_lowercase())
        .or_else(|| {
            detail
                .summary
                .author
                .as_ref()
                .map(|actor| actor.login.to_ascii_lowercase())
        });
    let mut remarks: BTreeMap<String, &agent_domain::PullRequestComment> = BTreeMap::new();
    for comment in detail.comments.iter().chain(
        detail
            .review_threads
            .iter()
            .flat_map(|thread| thread.comments.iter()),
    ) {
        if own_login.as_deref().is_some_and(|own| {
            comment
                .author
                .as_ref()
                .is_some_and(|author| author.login.eq_ignore_ascii_case(own))
        }) {
            continue;
        }
        remarks.entry(comment.id.clone()).or_insert(comment);
    }
    let through = watch
        .remarks_through
        .as_ref()
        .map(|timestamp| timestamp.millis())
        .unwrap_or_else(|| watch.started_at.millis());
    let fresh = remarks
        .values()
        .filter(|comment| {
            comment.created_at.millis() > through
                || (comment.created_at.millis() == through
                    && !watch.remark_ids.contains(&comment.id))
        })
        .copied()
        .collect::<Vec<_>>();
    if let Some(latest) = fresh
        .iter()
        .map(|comment| comment.created_at.millis())
        .max()
    {
        if let Ok(timestamp) = Timestamp::from_millis(latest) {
            watch.remarks_through = Some(timestamp);
            watch.remark_ids = fresh
                .iter()
                .filter(|comment| comment.created_at.millis() == latest)
                .map(|comment| comment.id.clone())
                .collect();
        }
    }

    let progress = detail.summary.head_sha != before.head_sha
        || !newly_failed.is_empty()
        || checks_passed
        || conflicting;
    let comments_only = !fresh.is_empty() && !progress;
    if progress {
        watch.wakes = 0;
    } else if comments_only {
        watch.wakes = watch.wakes.saturating_add(1);
    }
    if newly_failed.is_empty() && !checks_passed && fresh.is_empty() && !conflicting {
        return None;
    }

    let mut lines = vec![format!(
        "Update on pull request #{} ({}):",
        detail.summary.key.number, detail.summary.url
    )];
    if !newly_failed.is_empty() {
        lines.push(format!("- Checks failed: {}.", newly_failed.join(", ")));
    }
    if checks_passed {
        lines.push("- Checks passed.".into());
    }
    if conflicting {
        lines.push(format!(
            "- The branch now conflicts with {}.",
            detail.summary.base_branch
        ));
    }
    if !fresh.is_empty() {
        lines.push(format!(
            "- {} new {}:",
            fresh.len(),
            if fresh.len() == 1 {
                "comment"
            } else {
                "comments"
            }
        ));
        for comment in fresh.iter().take(10) {
            let body = comment
                .body
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let body = truncate_chars(&body, 200);
            let author = comment
                .author
                .as_ref()
                .map(|actor| actor.login.as_str())
                .unwrap_or("someone");
            lines.push(format!("  - {author}: {body}"));
        }
        if fresh.len() > 10 {
            lines.push(format!("  - and {} more", fresh.len() - 10));
        }
    }
    if watch.wakes >= MAX_WATCH_WAKES && comments_only {
        lines.push(format!(
            "Stopped watching after {} comment-only updates.",
            MAX_WATCH_WAKES
        ));
    }
    let detail_text = lines.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
    Some(PullRequestWatchWake {
        text: lines.join("\n"),
        detail: truncate_chars(&detail_text, 500),
        failed: !newly_failed.is_empty() || conflicting,
        exhausted: comments_only && watch.wakes >= MAX_WATCH_WAKES,
    })
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(watch: Option<PullRequestWatch>) -> PullRequestLink {
        PullRequestLink {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 1,
            url: "https://github.com/owner/repo/pull/1".into(),
            source: agent_domain::PullRequestLinkSource::Manual,
            linked_at: Timestamp::parse("2026-10-01T00:00:00Z").unwrap(),
            snapshot: None,
            stack: None,
            watch,
        }
    }

    #[test]
    fn watch_stops_after_terminal_or_budget() {
        let now = Timestamp::parse("2026-10-01T00:00:00Z").unwrap();
        assert!(next_watch(&link(None), &now, DEFAULT_WATCH_INTERVAL).is_none());
        let mut watch = start_watch(now.clone(), None);
        watch.wakes = MAX_WATCH_WAKES;
        assert!(next_watch(&link(Some(watch)), &now, DEFAULT_WATCH_INTERVAL).is_none());
        let watch = start_watch(now.clone(), None);
        assert_eq!(
            next_watch(&link(Some(watch)), &now, Duration::from_secs(30))
                .unwrap()
                .as_str(),
            "2026-10-01T00:00:30.000Z"
        );
        let now = Timestamp::parse("2026-10-01T00:00:30.000Z").unwrap();
        assert!(watch_is_due(
            &link(Some(start_watch(
                Timestamp::parse("2026-10-01T00:00:00Z").unwrap(),
                None,
            ))),
            &now,
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn terminal_result_is_counted_once_and_failed_checks_are_kept() {
        let at = Timestamp::parse("2026-10-01T00:00:00Z").unwrap();
        let mut watch = start_watch(at.clone(), None);
        let mut summary = agent_domain::PullRequestSummary {
            key: agent_domain::PullRequestKey::new("github.com", "owner/repo", 1),
            project: Some("project".into()),
            url: "https://github.com/owner/repo/pull/1".into(),
            title: "Fix".into(),
            state: agent_domain::PullRequestState::Open,
            is_draft: false,
            head_branch: "feature".into(),
            head_sha: None,
            base_branch: "main".into(),
            opened_at: None,
            closed_at: None,
            merged_at: None,
            updated_at: at.clone(),
            observed_at: at,
            author: None,
            additions: None,
            deletions: None,
            changed_files: None,
            review_decision: agent_domain::PullRequestReviewDecision::Unknown,
            checks_state: PullRequestChecksState::Failure,
            mergeability: PullRequestMergeability::Mergeable,
            checks: vec![agent_domain::PullRequestCheck {
                name: "unit".into(),
                state: PullRequestChecksState::Failure,
                url: None,
                started_at: None,
                completed_at: None,
            }],
            labels: vec![],
            stack: None,
        };
        update_watch(&mut watch, &summary);
        assert_eq!(watch.failed_checks, vec!["unit"]);
        assert!(!watch.passed);
        summary.checks_state = PullRequestChecksState::Success;
        summary.checks.clear();
        update_watch(&mut watch, &summary);
        assert!(watch.passed);
        assert_eq!(watch.wakes, 0);
        update_watch(&mut watch, &summary);
        assert_eq!(watch.wakes, 0);
    }

    #[test]
    fn reports_new_remarks_once_and_ignores_the_viewer() {
        let started = Timestamp::parse("2026-10-01T00:00:00Z").unwrap();
        let summary = agent_domain::PullRequestSummary {
            key: agent_domain::PullRequestKey::new("github.com", "owner/repo", 2),
            project: Some("project".into()),
            url: "https://github.com/owner/repo/pull/2".into(),
            title: "Review".into(),
            state: agent_domain::PullRequestState::Open,
            is_draft: false,
            head_branch: "feature".into(),
            head_sha: Some("sha".into()),
            base_branch: "main".into(),
            opened_at: None,
            closed_at: None,
            merged_at: None,
            updated_at: started.clone(),
            observed_at: started.clone(),
            author: Some(agent_domain::PullRequestActor {
                login: "agent".into(),
                display_name: None,
                avatar_url: None,
            }),
            additions: None,
            deletions: None,
            changed_files: None,
            review_decision: agent_domain::PullRequestReviewDecision::Unknown,
            checks_state: PullRequestChecksState::Pending,
            mergeability: PullRequestMergeability::Mergeable,
            checks: vec![],
            labels: vec![],
            stack: None,
        };
        let comment = |id: &str, login: &str, at: &str| agent_domain::PullRequestComment {
            id: id.into(),
            author: Some(agent_domain::PullRequestActor {
                login: login.into(),
                display_name: None,
                avatar_url: None,
            }),
            body: "Please update this".into(),
            created_at: Timestamp::parse(at).unwrap(),
            updated_at: None,
            url: None,
        };
        let detail = agent_domain::PullRequestDetail {
            summary,
            body: String::new(),
            comments: vec![
                comment("self", "agent", "2026-10-01T00:01:00Z"),
                comment("review", "reviewer", "2026-10-01T00:02:00Z"),
            ],
            review_threads: vec![],
            reviewers: vec![],
            requested_reviewers: vec![],
            viewer: Some(agent_domain::PullRequestActor {
                login: "agent".into(),
                display_name: None,
                avatar_url: None,
            }),
        };
        let mut watch = start_watch(started, Some("sha".into()));
        let wake = update_watch_detail(&mut watch, &detail).expect("new reviewer remark");
        assert!(wake.text.contains("reviewer"));
        assert!(!wake.text.contains("self"));
        assert_eq!(watch.wakes, 1);
        assert!(update_watch_detail(&mut watch, &detail).is_none());
        assert_eq!(watch.wakes, 1);
    }
}
