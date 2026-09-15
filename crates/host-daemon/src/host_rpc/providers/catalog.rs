//! Merge provider histories before the Host applies project membership and limits.
use super::{Failure, Providers};
use agent_core::models::{Thread, ThreadListParams};
use std::{collections::HashSet, iter::Peekable, vec::IntoIter};

pub(in crate::host_rpc) struct ThreadCatalog<'a> {
    providers: &'a Providers,
    params: ThreadListParams<'a>,
    remaining: Peekable<IntoIter<Thread>>,
    cursors: HashSet<String>,
    done: bool,
    failure: Option<Failure>,
}
impl<'a> ThreadCatalog<'a> {
    pub(in crate::host_rpc) async fn new(providers: &'a Providers, search: &'a str) -> Self {
        let remaining = match providers.claude.get() {
            Some(claude) => claude.list(search).await,
            None => Vec::new(),
        }
        .into_iter()
        .peekable();
        Self {
            providers,
            remaining,
            cursors: HashSet::new(),
            done: false,
            failure: None,
            params: ThreadListParams {
                limit: 100,
                sort_key: "updated_at",
                sort_direction: "desc",
                use_state_db_only: true,
                search_term: (!search.trim().is_empty()).then_some(search),
                cursor: None,
            },
        }
    }
    pub(in crate::host_rpc) async fn next_page(&mut self) -> Result<Option<Vec<Thread>>, Failure> {
        if self.done {
            return Ok(None);
        }
        // Check only when another page is needed: the Host may already have
        // filled its visible window, just as in the native paginated history.
        if let Some(cursor) = &self.params.cursor
            && !self.cursors.insert(cursor.clone())
        {
            return Err(Failure::new(
                "invalid_thread_list",
                "thread list cursor repeated",
            ));
        }
        let page = match self.providers.codex.thread_page(&self.params).await {
            Ok(page) => page,
            Err(error) => {
                self.failure = Some(error);
                self.done = true;
                return Ok(None);
            }
        };
        let mut merged = Vec::new();
        for thread in page.data {
            while self
                .remaining
                .peek()
                .is_some_and(|next| updated_at(next) >= updated_at(&thread))
            {
                merged.push(self.remaining.next().unwrap());
            }
            merged.push(thread);
        }
        self.params.cursor = page.next_cursor.filter(|cursor| !cursor.is_empty());
        self.done = self.params.cursor.is_none();
        Ok(Some(merged))
    }
    /// Strict callers (worktree deletion) must account for every provider.
    pub(in crate::host_rpc) fn unavailable_reason(&self) -> Option<String> {
        self.failure.as_ref().map(|error| format!("Codex: {error}"))
    }
    /// Lists remain usable with an independent provider, even when Codex fails.
    pub(in crate::host_rpc) fn finish(
        self,
    ) -> Result<(Vec<Thread>, serde_json::Map<String, serde_json::Value>), Failure> {
        let mut errors = serde_json::Map::new();
        if let Some(error) = self.failure {
            if self.providers.claude.get().is_none() {
                return Err(error);
            }
            errors.insert(
                "codex".into(),
                serde_json::to_value(error).expect("Failure serializes"),
            );
        }
        Ok((self.remaining.collect(), errors))
    }
    pub(in crate::host_rpc) fn remaining(self) -> impl Iterator<Item = Thread> {
        self.remaining
    }
}
pub(in crate::host_rpc) fn updated_at(thread: &Thread) -> u64 {
    thread
        .updated_at
        .as_ref()
        .and_then(|number| number.as_u64())
        .unwrap_or_default()
}
