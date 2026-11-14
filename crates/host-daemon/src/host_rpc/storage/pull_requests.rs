use super::*;
use agent_protocol::pull_requests::{self, PullRequest};

pub(in crate::host_rpc) enum Update {
    Link(PullRequest),
    Unlink(String),
    Refresh(Vec<PullRequest>),
}

impl Storage {
    pub(in crate::host_rpc) fn pull_requests(
        &self,
        id: &SessionRef,
        update: Update,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let (mut header, counters) = rows::header(&transaction, key)?;
            let links = &mut header.details.pull_requests;
            let previous = links.clone();
            match update {
                Update::Link(value) => {
                    if let Some(link) = links.iter_mut().find(|link| link.url == value.url) {
                        *link = value;
                    } else {
                        if links.len() >= pull_requests::LINK_LIMIT {
                            return Err("too many linked pull requests".into());
                        }
                        links.push(value);
                    }
                }
                Update::Unlink(url) => links.retain(|link| link.url != url),
                Update::Refresh(updates) => *links = pull_requests::refresh(links, &updates),
            }
            let mut changed = Vec::new();
            if *links != previous {
                transaction
                    .execute(
                        "UPDATE threads SET body=?1 WHERE key=?2",
                        params![codec::header(&header, &counters)?, key],
                    )
                    .map_err(failure)?;
                changed.push(Row {
                    key: Key::Thread(id.clone()),
                    version: 0,
                    position: Position::Unordered,
                    content: Content::Full(Arc::new(Record::Thread(header))),
                });
            }
            Self::commit(
                transaction,
                Some(id),
                Step {
                    outcome: Outcome::Accepted,
                    ..Default::default()
                },
                changed,
                Vec::new(),
                false,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pr(number: u64) -> PullRequest {
        PullRequest {
            url: format!("https://github.com/acme/repo/pull/{number}"),
            number,
            title: "Fix".into(),
            state: pull_requests::State::Open,
            draft: false,
            head: "fix".into(),
            base: "main".into(),
            updated_at: "now".into(),
            error: None,
        }
    }
    #[test]
    fn links_survive_reopen_and_stale_refresh_does_not_resurrect_removed_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversations.sqlite");
        let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let storage = Storage::open(&path);
        let header = Header {
            state: decision::Thread {
                context_start: 0,
                name: "PR".into(),
                workspace: decision::Workspace::Missing,
                provider: ProviderKind::Codex,
                queue_held: false,
            },
            details: Default::default(),
        };
        storage.create(&id, &header).unwrap();
        storage.pull_requests(&id, Update::Link(pr(1))).unwrap();
        storage.pull_requests(&id, Update::Link(pr(2))).unwrap();
        storage
            .pull_requests(&id, Update::Unlink(pr(1).url))
            .unwrap();
        let mut updated = pr(2);
        updated.state = pull_requests::State::Merged;
        storage
            .pull_requests(&id, Update::Refresh(vec![pr(1), updated.clone()]))
            .unwrap();
        drop(storage);
        let stored = Storage::open(&path).header(&id).unwrap();
        assert_eq!(stored.details.pull_requests, vec![updated]);
    }
}
