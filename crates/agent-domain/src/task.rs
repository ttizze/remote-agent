use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedTaskStatus {
    pub child_run_id: Option<RunId>,
    pub status: ItemStatus,
    pub summary: Option<String>,
    pub provider_instance_id: Option<String>,
    pub result_context_transfer_id: Option<ContextTransferId>,
    pub has_pending_child_runs: bool,
    pub latest_terminal_run_id: Option<RunId>,
    pub latest_terminal_status: Option<RunStatus>,
    pub latest_terminal_summary: Option<String>,
    pub latest_terminal_result_context_transfer_id: Option<ContextTransferId>,
}
/// The task's original outcome is independent of subsequent child follow-ups.
/// The caller supplies committed child runs/items and the parent's transfers.
pub fn delegated_task_status(
    task: &Task,
    child_runs: &[Run],
    child_items: &[Item],
    parent_transfers: &[Transfer],
    child_messages: &[Message],
) -> DelegatedTaskStatus {
    let original = child_runs
        .iter()
        .find(|run| Some(&run.message) == task.original_message.as_ref());
    let latest = child_runs
        .iter()
        .filter(|run| {
            run.status.terminal()
                && run.status != RunStatus::RolledBack
                && (original.is_some_and(|original| original.id == run.id)
                    || run.started_at.is_some())
                && !child_messages.iter().any(|message| {
                    message.run.as_ref() == Some(&run.id)
                        && message.notification.as_ref().is_some_and(|notification| {
                            notification.source
                                == NotificationSource::Native(BackgroundKind::Monitor)
                        })
                })
        })
        .reduce(|latest, run| {
            if run_ran_after(run, latest) {
                run
            } else {
                latest
            }
        });
    let transfer = |run: &Run| {
        parent_transfers
            .iter()
            .rev()
            .find(|transfer| {
                transfer.kind == TransferKind::SubagentResult
                    && transfer.source == task.child_thread
                    && transfer.boundary == run.ordinal
            })
            .map(|transfer| transfer.id.clone())
    };
    let summary = |run: &Run| {
        let text = child_items
            .iter()
            .filter(|item| {
                item.run.as_ref() == Some(&run.id)
                    && matches!(item.kind, ItemKind::AssistantMessage { .. })
            })
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.is_empty()).then_some(text)
    };
    DelegatedTaskStatus {
        child_run_id: original.map(|run| run.id.clone()),
        status: task.status,
        summary: task.result.clone(),
        provider_instance_id: original.map(|run| run.selection.instance.clone()),
        result_context_transfer_id: parent_transfers
            .iter()
            .find(|transfer| {
                transfer.kind == TransferKind::SubagentResult
                    && transfer.source == task.child_thread
            })
            .map(|transfer| transfer.id.clone()),
        has_pending_child_runs: child_runs.iter().any(|run| {
            (run.status.blocking() || run.status == RunStatus::Queued)
                && original.is_none_or(|original| run.ordinal > original.ordinal)
        }),
        latest_terminal_run_id: latest.map(|run| run.id.clone()),
        latest_terminal_status: latest.map(|run| run.status),
        latest_terminal_summary: latest.and_then(summary),
        latest_terminal_result_context_transfer_id: latest.and_then(transfer),
    }
}
