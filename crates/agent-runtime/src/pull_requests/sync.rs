use agent_domain::{PullRequestDetail, PullRequestLink, PullRequestSummary, Timestamp};
use super::watch::update_watch;
use super::watch::{update_watch_detail, PullRequestWatchWake};

/// A provider refresh result is merged by this pure helper before it is
/// committed to the durable store and thread facts.
pub fn merge_pull_request_snapshot(
    current: &[PullRequestLink],
    refreshed: &[PullRequestSummary],
    observed_at: Timestamp,
) -> Vec<PullRequestLink> {
    current
        .iter()
        .map(|link| {
            let mut link = link.clone();
            if let Some(snapshot) = refreshed.iter().find(|snapshot| snapshot.key == link.key()) {
                link.snapshot = Some(snapshot.clone());
                link.url = snapshot.url.clone();
                link.stack = snapshot.stack.clone();
            }
            if let Some(watch) = link.watch.as_mut() {
                if let Some(snapshot) = link.snapshot.as_ref() {
                    let previous = watch.clone();
                    update_watch(watch, snapshot);
                    if watch.wakes == 0
                        && (watch.head_sha != previous.head_sha
                            || watch.failed_checks != previous.failed_checks
                            || watch.passed != previous.passed
                            || watch.conflicting != previous.conflicting)
                    {
                        watch.started_at = observed_at.clone();
                    }
                }
            }
            if link.snapshot.is_none() {
                link.linked_at = observed_at.clone();
            }
            link
        })
        .collect()
}

/// Merges a detail refresh, including one bounded watch wake. The link is
/// returned separately from the wake so the Host can commit the link before
/// queueing the message that explains the refresh.
pub fn merge_pull_request_detail(
    current: &[PullRequestLink],
    detail: &PullRequestDetail,
    observed_at: Timestamp,
) -> (Vec<PullRequestLink>, Option<PullRequestWatchWake>) {
    let mut wake = None;
    let links = current
        .iter()
        .map(|link| {
            let mut link = link.clone();
            if link.key() == detail.summary.key {
                link.snapshot = Some(detail.summary.clone());
                link.url = detail.summary.url.clone();
                link.stack = detail.summary.stack.clone();
                if let Some(watch) = link.watch.as_mut() {
                    let previous = watch.clone();
                    wake = update_watch_detail(watch, detail);
                    if watch.wakes == 0
                        && (watch.head_sha != previous.head_sha
                            || watch.failed_checks != previous.failed_checks
                            || watch.passed != previous.passed
                            || watch.conflicting != previous.conflicting)
                    {
                        watch.started_at = observed_at.clone();
                    }
                }
                if wake.as_ref().is_some_and(|wake| wake.exhausted) {
                    link.watch = None;
                }
                if detail.summary.state != agent_domain::PullRequestState::Open {
                    link.watch = None;
                }
            }
            if link.snapshot.is_none() {
                link.linked_at = observed_at.clone();
            }
            link
        })
        .collect();
    (links, wake)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_preserves_link_source_and_watch() {
        let at = Timestamp::parse("2026-10-01T00:00:00Z").unwrap();
        let link = PullRequestLink {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 1,
            url: "https://github.com/owner/repo/pull/1".into(),
            source: agent_domain::PullRequestLinkSource::Agent,
            linked_at: at.clone(),
            snapshot: None,
            stack: None,
            watch: None,
        };
        let result = merge_pull_request_snapshot(&[link], &[], at);
        assert_eq!(result[0].source, agent_domain::PullRequestLinkSource::Agent);
    }
}
