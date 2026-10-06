//! Progressive history of a bounded thread window: cursor bookkeeping and the
//! merge of older pages into the folded state.
use agent_domain::State;
use agent_protocol::conversation::{HistoryPage, HistoryRow, SnapshotWindow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMeta {
    pub cursor: Option<String>,
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// An older page was merged; the timeline is kept out of the disk cache.
    pub expanded: bool,
    /// Highest local item ordinal of the full timeline, for partial windows.
    pub latest_local_ordinal: Option<u64>,
}

impl HistoryMeta {
    pub fn from_window(window: &SnapshotWindow) -> Self {
        Self {
            cursor: window.history_cursor.clone(),
            has_more: window.has_more_history,
            latest_local_ordinal: window.latest_local_ordinal,
            ..Self::default()
        }
    }
    /// The window is incomplete: an open cursor or more history. `expanded`
    /// stays true after the last page, so it does not count.
    pub fn partial(&self) -> bool {
        self.has_more || self.cursor.is_some()
    }
    pub fn shows_load_earlier(&self) -> bool {
        self.has_more || self.error.is_some()
    }
    /// Whether a page response still matches the cursor that requested it.
    pub fn is_active_request(&self, request_cursor: &str) -> bool {
        self.cursor.as_deref() == Some(request_cursor)
    }
    /// Clears `loading` after an abandoned request only while its cursor is current.
    pub fn clear_loading(&self, request_cursor: &str) -> Self {
        if !self.is_active_request(request_cursor) || !self.loading {
            return self.clone();
        }
        Self {
            loading: false,
            ..self.clone()
        }
    }
    pub fn after_page(&self, page: &HistoryPage) -> Self {
        Self {
            cursor: page.next_cursor.clone(),
            has_more: page.has_more,
            loading: false,
            error: None,
            expanded: true,
            latest_local_ordinal: self.latest_local_ordinal,
        }
    }
}

/// Adds older rows the state does not hold. A row whose item is already present
/// is never replaced: a live fact may have changed or hidden it while the page
/// was in flight. Returns `None` when nothing was added.
pub fn merge_history_page(state: &State, rows: &[HistoryRow]) -> Option<State> {
    let mut next: Option<State> = None;
    for row in rows {
        let current = next.as_ref().unwrap_or(state);
        let (items, messages) = if row.inherited {
            (&current.inherited_items, &current.inherited_messages)
        } else {
            (&current.items, &current.messages)
        };
        if items.iter().any(|item| item.id == row.item.id) {
            continue;
        }
        let missing_message = row
            .message
            .as_ref()
            .filter(|message| !messages.iter().any(|m| m.id == message.id))
            .cloned();
        let target = next.get_or_insert_with(|| state.clone());
        if row.inherited {
            target.inherited_items.push(row.item.clone());
            target.inherited_messages.extend(missing_message);
        } else {
            target.items.push(row.item.clone());
            target.messages.extend(missing_message);
        }
        if let Some(plan) = &row.plan {
            match target.plans.iter_mut().find(|p| p.id == plan.id) {
                // Bounded snapshots drop the text of plans whose run ended.
                Some(local) if local.markdown.is_empty() => local.markdown = plan.markdown.clone(),
                Some(_) => {}
                None => target.plans.push(plan.clone()),
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::*;
    use agent_domain::{ItemKind, ItemStatus, RunStatus};

    fn row(index: u64) -> HistoryRow {
        HistoryRow {
            position: index,
            source: thread_id(),
            inherited: false,
            item: command_item(&format!("item-{index}"), index + 1),
            message: None,
            plan: None,
        }
    }
    fn ids(state: &State) -> Vec<String> {
        state
            .visible_items()
            .iter()
            .map(|item| item.id.to_string())
            .collect()
    }

    #[test]
    fn prepends_older_rows_and_dedupes_by_source_identity() {
        let mut state = thread_state("Thread");
        state.items = vec![row(2).item, row(3).item];
        let merged = merge_history_page(&state, &[row(0), row(1), row(2)]).unwrap();
        assert_eq!(ids(&merged), ["item-0", "item-1", "item-2", "item-3"]);
        let stored: Vec<_> = merged.items.iter().map(|i| i.id.to_string()).collect();
        assert_eq!(stored, ["item-2", "item-3", "item-0", "item-1"]);
    }

    #[test]
    fn retains_live_rows_that_arrived_after_the_older_page_was_fetched() {
        let mut state = thread_state("Thread");
        state.items = vec![row(5).item, row(6).item];
        let merged = merge_history_page(&state, &[row(3), row(4)]).unwrap();
        assert_eq!(ids(&merged), ["item-3", "item-4", "item-5", "item-6"]);
    }

    #[test]
    fn does_not_resurrect_a_local_item_changed_while_an_older_page_was_in_flight() {
        let mut state = thread_state("Thread");
        let mut current = row(1).item;
        current.text = "newer live output".into();
        state.items = vec![current.clone()];
        assert_eq!(merge_history_page(&state, &[row(1)]), None);
        assert_eq!(state.items, vec![current]);
    }

    #[test]
    fn marks_history_expanded_after_a_successful_page() {
        let meta = HistoryMeta {
            cursor: Some("cursor-a".into()),
            has_more: true,
            loading: true,
            error: None,
            expanded: false,
            latest_local_ordinal: Some(42),
        };
        let page = HistoryPage {
            rows: vec![],
            next_cursor: Some("cursor-b".into()),
            has_more: true,
        };
        assert_eq!(
            meta.after_page(&page),
            HistoryMeta {
                cursor: Some("cursor-b".into()),
                has_more: true,
                loading: false,
                error: None,
                expanded: true,
                latest_local_ordinal: Some(42),
            }
        );
    }

    #[test]
    fn only_applies_history_responses_for_the_active_request_cursor() {
        let meta = |cursor: Option<&str>| HistoryMeta {
            cursor: cursor.map(Into::into),
            ..HistoryMeta::default()
        };
        assert!(meta(Some("cursor-a")).is_active_request("cursor-a"));
        assert!(!meta(Some("cursor-b")).is_active_request("cursor-a"));
        assert!(!meta(None).is_active_request("cursor-a"));
    }

    #[test]
    fn clears_loading_on_interrupt_only_for_the_active_request_cursor() {
        let loading = HistoryMeta {
            cursor: Some("cursor-a".into()),
            has_more: true,
            loading: true,
            error: None,
            expanded: false,
            latest_local_ordinal: Some(10),
        };
        assert_eq!(
            loading.clear_loading("cursor-a"),
            HistoryMeta {
                loading: false,
                ..loading.clone()
            }
        );
        let replaced = HistoryMeta {
            cursor: Some("cursor-b".into()),
            ..loading.clone()
        };
        assert_eq!(replaced.clear_loading("cursor-a"), replaced);
        let idle = HistoryMeta {
            loading: false,
            ..loading
        };
        assert_eq!(idle.clear_loading("cursor-a"), idle);
    }

    #[test]
    fn shows_the_load_earlier_control_when_history_remains_or_a_local_error_is_set() {
        let more = HistoryMeta {
            cursor: Some("c".into()),
            has_more: true,
            ..HistoryMeta::default()
        };
        assert!(more.shows_load_earlier());
        let failed = HistoryMeta {
            error: Some("Could not load earlier activity.".into()),
            expanded: true,
            ..HistoryMeta::default()
        };
        assert!(failed.shows_load_earlier());
        assert!(!HistoryMeta::default().shows_load_earlier());
    }

    #[test]
    fn retains_merged_older_rows_when_a_live_item_update_arrives() {
        let mut state = thread_state("Thread");
        state.items = vec![row(2).item, row(3).item];
        let mut merged = merge_history_page(&state, &[row(0), row(1)]).unwrap();
        let mut live = row(3).item;
        live.text = "streamed".into();
        agent_domain::apply(&mut merged, &projected(live.clone())).unwrap();
        assert_eq!(ids(&merged), ["item-0", "item-1", "item-2", "item-3"]);
        assert_eq!(merged.visible_items()[3], &live);
    }

    #[test]
    fn keeps_a_page_interrupt_row_visible_when_its_request_was_retained_at_open() {
        let mut state = thread_state("Thread");
        state.runs.push(run("run-interrupt", 1, RunStatus::Running));
        let mut request = command_item("item-interrupt-request", 1);
        request.kind = ItemKind::RunInterruptRequest;
        request.run = Some(state.runs[0].id.clone());
        let mut result = command_item("item-interrupt-result", 2);
        result.kind = ItemKind::RunInterruptResult {
            request: request.id.clone(),
        };
        result.run = request.run.clone();
        state.items = vec![request.clone(), row(5).item];
        let page = |item: &agent_domain::Item| HistoryRow {
            item: item.clone(),
            ..row(0)
        };
        // A retained request is already present and visible.
        assert_eq!(merge_history_page(&state, &[page(&request)]), None);
        assert_eq!(ids(&state), ["item-interrupt-request", "item-5"]);
        let mut rolled_back = state.clone();
        rolled_back.runs[0].status = RunStatus::RolledBack;
        assert_eq!(ids(&rolled_back), ["item-5"]);
        let merged = merge_history_page(&state, &[page(&result)]).unwrap();
        assert_eq!(
            ids(&merged),
            ["item-interrupt-request", "item-interrupt-result", "item-5"]
        );
        assert!(
            merged
                .items
                .iter()
                .all(|item| item.status == ItemStatus::Completed)
        );
    }

    #[test]
    fn fills_plan_text_a_bounded_snapshot_left_out() {
        let mut state = thread_state("Thread");
        let mut plan = plan("plan", "run");
        let mut stored = plan.clone();
        stored.markdown.clear();
        state.plans.push(stored);
        plan.markdown = "# Plan".into();
        let page = HistoryRow {
            plan: Some(plan),
            ..row(0)
        };
        let merged = merge_history_page(&state, &[page]).unwrap();
        assert_eq!(merged.plans[0].markdown, "# Plan");
    }
}
