//! Workspace preparation rows: hidden bookkeeping, and the runs a Retry can
//! prepare again.
use agent_domain::{
    Item, ItemKind, ItemStatus, Run, RunId, RunStatus, WORKSPACE_PREPARATION_FAILURE_CODE,
    WORKSPACE_PREPARATION_INPUT,
};

fn is_preparation_failure(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Error { code: Some(code), .. } if code == WORKSPACE_PREPARATION_FAILURE_CODE)
}

/// Workspace setup is bookkeeping; a preparation failure has its own error
/// item. A retry cancels that item, which then has nothing left to say.
pub fn turn_item_is_workspace_preparation(item: &Item) -> bool {
    matches!(&item.kind, ItemKind::CommandExecution { command, .. } if command == WORKSPACE_PREPARATION_INPUT)
        || (item.status == ItemStatus::Cancelled && is_preparation_failure(&item.kind))
}

/// Runs a Retry can prepare again: their workspace preparation failed and the
/// run still ended there. In item order, without repeats.
pub fn workspace_preparation_retry_run_ids(runs: &[Run], items: &[Item]) -> Vec<RunId> {
    let failed_runs: Vec<&RunId> = runs
        .iter()
        .filter(|run| run.status == RunStatus::Failed)
        .map(|run| &run.id)
        .collect();
    let mut retryable: Vec<RunId> = Vec::new();
    if failed_runs.is_empty() {
        return retryable;
    }
    for item in items {
        if item.status == ItemStatus::Failed
            && is_preparation_failure(&item.kind)
            && let Some(run) = &item.run
            && failed_runs.contains(&run)
            && !retryable.contains(run)
        {
            retryable.push(run.clone());
        }
    }
    retryable
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{Driver, MessageId, ModelSelection, Timestamp, TurnItemId};

    fn now() -> Timestamp {
        Timestamp::parse("2026-08-03T00:00:00.000Z").unwrap()
    }

    fn command(input: &str) -> Item {
        Item {
            id: TurnItemId::new("item-command").unwrap(),
            run: Some(RunId::new("run-1").unwrap()),
            attempt: None,
            native_key: String::new(),
            ordinal: 1,
            kind: ItemKind::CommandExecution {
                command: input.into(),
                cwd: None,
                exit_code: Some(0),
                title: Some("Workspace ready".into()),
            },
            status: ItemStatus::Completed,
            text: "Workspace preparation completed.".into(),
            started_at: now(),
            completed_at: Some(now()),
            output_omitted: false,
            output_indicates_failure: false,
        }
    }

    fn preparation_failure(status: ItemStatus, code: Option<&str>) -> Item {
        Item {
            id: TurnItemId::new("item-error").unwrap(),
            status,
            kind: ItemKind::Error {
                message: "fetch failed".into(),
                retry: None,
                code: code.map(Into::into),
                class: Some("validation_error".into()),
                retryable: Some(false),
                reset_at: None,
            },
            text: String::new(),
            ..command("")
        }
    }

    fn run(status: RunStatus) -> Run {
        Run {
            restart_of: None,
            restart_cancelled_work: vec![],
            checkpoint_scope: None,
            native_baseline_heads: Default::default(),
            id: RunId::new("run-1").unwrap(),
            ordinal: 1,
            message: MessageId::new("message-1").unwrap(),
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "gpt-5.4".into(),
                options: Default::default(),
            },
            status,
            attempt: None,
            queue_position: None,
            queue_held: false,
            requested_at: now(),
            started_at: None,
            completed_at: None,
            source_plan: None,
            checkpoint: None,
            continuation: false,
        }
    }

    #[test]
    fn identifies_the_synthetic_workspace_preparation_command() {
        assert!(turn_item_is_workspace_preparation(&command(
            "Preparing workspace"
        )));
        assert!(!turn_item_is_workspace_preparation(&command(
            "prepare workspace"
        )));
    }

    #[test]
    fn hides_a_preparation_failure_only_once_a_retry_cancelled_it() {
        let code = Some(WORKSPACE_PREPARATION_FAILURE_CODE);
        assert!(!turn_item_is_workspace_preparation(&preparation_failure(
            ItemStatus::Failed,
            code
        )));
        assert!(turn_item_is_workspace_preparation(&preparation_failure(
            ItemStatus::Cancelled,
            code
        )));
        assert!(!turn_item_is_workspace_preparation(&preparation_failure(
            ItemStatus::Cancelled,
            None
        )));
    }

    #[test]
    fn offers_a_retry_while_the_run_still_ends_in_its_failed_preparation() {
        let failure = [preparation_failure(
            ItemStatus::Failed,
            Some(WORKSPACE_PREPARATION_FAILURE_CODE),
        )];
        assert_eq!(
            workspace_preparation_retry_run_ids(&[run(RunStatus::Failed)], &failure),
            [RunId::new("run-1").unwrap()]
        );
        // Retried: the run is preparing again and the old failure is cancelled.
        assert!(
            workspace_preparation_retry_run_ids(&[run(RunStatus::Preparing)], &failure).is_empty()
        );
        // A provider error on a run that did reach the provider is not a preparation failure.
        assert!(
            workspace_preparation_retry_run_ids(
                &[run(RunStatus::Failed)],
                &[preparation_failure(
                    ItemStatus::Failed,
                    Some("provider_crashed")
                )],
            )
            .is_empty()
        );
    }
}
