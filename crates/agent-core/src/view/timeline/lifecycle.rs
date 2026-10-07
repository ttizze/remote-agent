//! Lifecycle rows of the conversation: interrupts, compactions, forks,
//! created threads, subagents and provider handoffs.
use crate::js_text::js_trim;
use crate::view::agents::{format_subagent_display_title, subagent_card_detail};
use agent_domain::{
    ContextTransferId, Driver, Item, ItemKind, ItemStatus, NodeId, Notification,
    NotificationOutcome, Run, RunId, RunStatus, State, ThreadId, Timestamp, Transfer, TransferKind,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DividerTone {
    Neutral,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DividerIcon {
    Stop,
    Compaction,
    Handoff,
    Fork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DividerAction {
    pub label: String,
    pub thread: ThreadId,
}

/// A centered line across the timeline with a label and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SystemDivider {
    pub label: String,
    /// Follows the label after a "·" separator.
    pub detail: Option<String>,
    pub tone: DividerTone,
    pub icon: DividerIcon,
    /// Makes the whole divider a button with this label as its tooltip.
    pub action: Option<DividerAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InterruptRequestRow {
    pub label: String,
    pub message: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CreatedThreadLayout {
    /// A bordered card for a resource summary.
    ResourceCard,
    WorkLogRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CreatedThreadRow {
    pub layout: CreatedThreadLayout,
    pub label: String,
    pub action_label: String,
    pub accessibility_label: String,
    pub thread: ThreadId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SubagentDot {
    Info,
    Success,
    Destructive,
    Muted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentStatusVisual {
    pub dot: SubagentDot,
    pub label: String,
}

/// In-flight states keep their own words; only settled states differ in color.
pub fn subagent_status_visual(status: ItemStatus) -> SubagentStatusVisual {
    let (dot, label) = match status {
        ItemStatus::Pending => (SubagentDot::Info, "Queued"),
        ItemStatus::Running => (SubagentDot::Info, "Running"),
        ItemStatus::Waiting => (SubagentDot::Info, "Waiting"),
        ItemStatus::Completed => (SubagentDot::Success, "Completed"),
        ItemStatus::Failed => (SubagentDot::Destructive, "Failed"),
        ItemStatus::Cancelled | ItemStatus::Interrupted => (SubagentDot::Muted, "Stopped"),
    };
    SubagentStatusVisual {
        dot,
        label: label.into(),
    }
}

/// One subagent drawn as a link row to its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentLink {
    pub task: Option<NodeId>,
    pub title: String,
    /// The status the dot shows; `None` draws no dot.
    pub status: Option<ItemStatus>,
    pub live_status: ItemStatus,
    pub status_label: String,
    pub detail: Option<String>,
    /// A single path-like detail that clients truncate in the middle.
    pub detail_is_path: bool,
    pub status_label_beside_title: bool,
    pub failed: bool,
    pub driver: Option<Driver>,
    pub model: Option<String>,
    /// Elapsed time is drawn from these unless `event_at` is set.
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub event_at: Option<Timestamp>,
    pub thread: Option<ThreadId>,
    pub open_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LifecycleRow {
    InterruptRequest(InterruptRequestRow),
    Divider(SystemDivider),
    CreatedThread(CreatedThreadRow),
    Subagent(SubagentLink),
}

pub fn is_lifecycle_item(kind: &ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::RunInterruptRequest
            | ItemKind::RunInterruptResult { .. }
            | ItemKind::Compaction { .. }
            | ItemKind::Fork { .. }
            | ItemKind::Subagent { .. }
            | ItemKind::ThreadCreated { .. }
    )
}

/// `resource_summary` draws a created thread as a card instead of a work-log row.
pub fn lifecycle_row(state: &State, item: &Item, resource_summary: bool) -> Option<LifecycleRow> {
    Some(match &item.kind {
        ItemKind::RunInterruptRequest => LifecycleRow::InterruptRequest(InterruptRequestRow {
            label: "Interrupt requested".into(),
            message: item.text.clone(),
            created_at: item.started_at.clone(),
        }),
        ItemKind::RunInterruptResult { .. } => LifecycleRow::Divider(SystemDivider {
            label: "Run interrupted".into(),
            detail: non_empty(&item.text),
            tone: DividerTone::Danger,
            icon: DividerIcon::Stop,
            action: None,
        }),
        ItemKind::Compaction { before, after } => {
            LifecycleRow::Divider(compaction_divider(item, *before, *after))
        }
        ItemKind::Fork { parent, .. } => LifecycleRow::Divider(SystemDivider {
            label: "Forked from conversation".into(),
            detail: None,
            tone: DividerTone::Neutral,
            icon: DividerIcon::Fork,
            action: Some(DividerAction {
                label: "Open source conversation".into(),
                thread: parent.clone(),
            }),
        }),
        ItemKind::ThreadCreated { thread, title, .. } => {
            LifecycleRow::CreatedThread(created_thread(thread, title, resource_summary))
        }
        ItemKind::Subagent { task } => LifecycleRow::Subagent(subagent_link(state, item, task)),
        _ => return None,
    })
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn compaction_divider(item: &Item, before: Option<u64>, after: Option<u64>) -> SystemDivider {
    let tokens = (before.is_some() || after.is_some()).then(|| {
        let count = |value: Option<u64>| value.map_or("?".into(), |value| value.to_string());
        format!("{} → {} tokens", count(before), count(after))
    });
    let label = match item.status {
        ItemStatus::Failed => "Context compaction failed",
        ItemStatus::Cancelled | ItemStatus::Interrupted => "Context compaction stopped",
        ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting => "Compacting context",
        ItemStatus::Completed => "Context compacted",
    };
    SystemDivider {
        label: label.into(),
        detail: non_empty(&item.text).or(tokens),
        tone: DividerTone::Neutral,
        icon: DividerIcon::Compaction,
        action: None,
    }
}

/// The mobile compaction divider shimmers while the unsettled run compacts.
pub fn compaction_divider_active(item: &Item, unsettled_run: Option<&RunId>) -> bool {
    unsettled_run.is_some_and(|run| item.run.as_ref() == Some(run))
        && item.status == ItemStatus::Running
}

fn created_thread(thread: &ThreadId, title: &str, resource_summary: bool) -> CreatedThreadRow {
    let title = (!title.is_empty()).then_some(title);
    CreatedThreadRow {
        layout: if resource_summary {
            CreatedThreadLayout::ResourceCard
        } else {
            CreatedThreadLayout::WorkLogRow
        },
        label: if resource_summary {
            title.unwrap_or("Created thread").into()
        } else {
            format!(
                "Created thread{}",
                title.map_or(String::new(), |title| format!(" · {title}"))
            )
        },
        action_label: "Open chat".into(),
        accessibility_label: format!("Open {}", title.unwrap_or("created thread")),
        thread: thread.clone(),
    }
}

/// The past event a notification reports about a subagent.
struct SubagentEvent {
    status: Option<ItemStatus>,
    label: &'static str,
    at: Timestamp,
}

fn subagent_link(state: &State, item: &Item, task: &NodeId) -> SubagentLink {
    present_subagent(state, Some(task), item.status, None)
}

/// A notification about one subagent, drawn as that subagent's card; `None`
/// when the thread has no record of the subagent.
pub fn subagent_notification_link(
    state: &State,
    notification: &Notification,
    created_at: &Timestamp,
) -> Option<SubagentLink> {
    let child = notification.child_thread.as_ref()?;
    let task = state
        .tasks
        .iter()
        .find(|task| &task.child_thread == child)?;
    let (status, label) = match notification.outcome {
        NotificationOutcome::Completed => (Some(ItemStatus::Completed), "Finished"),
        NotificationOutcome::Failed => (Some(ItemStatus::Failed), "Failed"),
        NotificationOutcome::Cancelled => (Some(ItemStatus::Cancelled), "Stopped"),
        NotificationOutcome::Updated => (None, "Updated"),
        NotificationOutcome::Unknown => (None, "Finished"),
    };
    Some(present_subagent(
        state,
        Some(&task.id),
        task.status,
        Some(SubagentEvent {
            status,
            label,
            at: created_at.clone(),
        }),
    ))
}

fn present_subagent(
    state: &State,
    task_id: Option<&NodeId>,
    item_status: ItemStatus,
    event: Option<SubagentEvent>,
) -> SubagentLink {
    let task = task_id.and_then(|id| state.task(id));
    let live_status = task.map_or(item_status, |task| task.status);
    let status = match &event {
        Some(event) => event.status,
        None => Some(live_status),
    };
    let status_label = event.as_ref().map_or_else(
        || subagent_status_visual(live_status).label,
        |event| event.label.into(),
    );
    fn trimmed(value: Option<&String>) -> Option<&str> {
        value
            .map(|value| js_trim(value))
            .filter(|value| !value.is_empty())
    }
    let result = trimmed(task.and_then(|task| task.result.as_ref()));
    let progress = trimmed(task.and_then(|task| task.progress.as_ref()));
    let settled = status.unwrap_or(live_status).terminal();
    let raw = if settled {
        result.or(progress)
    } else {
        progress.or(result)
    };
    let detail = subagent_card_detail(raw);
    let title = format_subagent_display_title(
        task.and_then(|task| task.title.as_deref())
            .unwrap_or("Subagent"),
    );
    // A provider-native subagent runs on its parent's provider.
    let driver = task.filter(|task| !task.app_owned()).and_then(|task| {
        let run = task.run.as_ref()?;
        state
            .runs
            .iter()
            .find(|candidate| &candidate.id == run)
            .map(|run| run.selection.driver)
    });
    SubagentLink {
        task: task_id.cloned(),
        status,
        live_status,
        status_label_beside_title: detail.is_some()
            && (event.is_some() || status != Some(ItemStatus::Completed)),
        detail_is_path: detail
            .as_ref()
            .is_some_and(|detail| detail.contains('/') && !detail.contains(' ')),
        detail,
        failed: status == Some(ItemStatus::Failed),
        status_label,
        driver,
        model: task.and_then(|task| task.model.clone()),
        started_at: task.map(|task| task.started_at.clone()),
        completed_at: task.and_then(|task| task.completed_at.clone()),
        event_at: event.map(|event| event.at),
        thread: task.map(|task| task.child_thread.clone()),
        open_label: format!("Open {title}"),
        title,
    }
}

/// A provider instance and the model it ran, when known.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HandoffEndpoint {
    pub instance: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HandoffEndpoints {
    pub from: Vec<HandoffEndpoint>,
    pub to: HandoffEndpoint,
}

/// A context handoff divider; the row derivations place it before `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HandoffDivider {
    pub run: RunId,
    pub transfers: Vec<ContextTransferId>,
    pub label: String,
    pub icon: DividerIcon,
    pub tone: DividerTone,
    pub endpoints: HandoffEndpoints,
}

fn provider_handoff(transfer: &Transfer) -> bool {
    matches!(
        transfer.kind,
        TransferKind::ProviderHandoff | TransferKind::ProviderHandoffDelta
    )
}

/// Endpoints of the provider handoffs delivered to `run`: the distinct
/// selections of the runs they cover and the run's own selection.
fn endpoints_for(state: &State, transfers: &[&Transfer], run: Option<&Run>) -> HandoffEndpoints {
    let to_instance = transfers
        .iter()
        .find_map(|transfer| transfer.instance.clone())
        .or_else(|| run.map(|run| run.selection.instance.clone()))
        .unwrap_or_default();
    let covered: BTreeSet<&str> = transfers
        .iter()
        .flat_map(|transfer| {
            let history = &transfer.history;
            history
                .messages
                .iter()
                .filter_map(|message| message.run.as_deref())
                .chain(history.omitted_item_ids.iter().filter_map(|id| {
                    state
                        .items
                        .iter()
                        .find(|item| item.id.as_str() == id)
                        .and_then(|item| item.run.as_ref())
                        .map(RunId::as_str)
                }))
        })
        .collect();
    let mut runs: Vec<&Run> = state
        .runs
        .iter()
        .filter(|run| covered.contains(run.id.as_str()))
        .collect();
    runs.sort_by_key(|run| run.ordinal);
    let mut from: Vec<HandoffEndpoint> = vec![];
    for run in runs {
        let endpoint = HandoffEndpoint {
            instance: run.selection.instance.clone(),
            model: Some(run.selection.model.clone()),
        };
        if !from.contains(&endpoint) {
            from.push(endpoint);
        }
    }
    HandoffEndpoints {
        from,
        to: HandoffEndpoint {
            model: run
                .filter(|run| run.selection.instance == to_instance)
                .map(|run| run.selection.model.clone()),
            instance: to_instance,
        },
    }
}

/// The endpoints of one delivered provider handoff.
pub fn resolve_handoff_endpoints(state: &State, transfer: &Transfer) -> HandoffEndpoints {
    let run = transfer
        .delivery
        .as_ref()
        .and_then(|delivery| state.runs.iter().find(|run| run.id == delivery.run));
    endpoints_for(state, &[transfer], run)
}

/// One divider per run that received a provider handoff from another
/// provider instance, in run order. A rolled-back run takes its divider along.
pub fn handoff_dividers(state: &State) -> Vec<HandoffDivider> {
    let mut targets: Vec<&Run> = vec![];
    for transfer in state.transfers.iter().filter(|t| provider_handoff(t)) {
        let Some(run) = transfer
            .delivery
            .as_ref()
            .and_then(|delivery| state.runs.iter().find(|run| run.id == delivery.run))
        else {
            continue;
        };
        if run.status != RunStatus::RolledBack && !targets.iter().any(|t| t.id == run.id) {
            targets.push(run);
        }
    }
    targets.sort_by_key(|run| run.ordinal);
    targets
        .into_iter()
        .filter_map(|run| {
            let transfers: Vec<&Transfer> = state
                .transfers
                .iter()
                .filter(|transfer| {
                    provider_handoff(transfer)
                        && transfer
                            .delivery
                            .as_ref()
                            .is_some_and(|delivery| delivery.run == run.id)
                })
                .collect();
            let endpoints = endpoints_for(state, &transfers, Some(run));
            // A session restored on the same provider is not a handoff.
            endpoints
                .from
                .iter()
                .any(|from| from.instance != endpoints.to.instance)
                .then(|| HandoffDivider {
                    run: run.id.clone(),
                    transfers: transfers.iter().map(|t| t.id.clone()).collect(),
                    label: "Context handoff".into(),
                    icon: DividerIcon::Handoff,
                    tone: DividerTone::Neutral,
                    endpoints,
                })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct HandoffEndpointPresentation {
    pub label: String,
    pub provider_name: String,
    pub tooltip: String,
}

/// `model_name` and `instance_name` are the provider catalog's names for the
/// endpoint's model and instance, when listed.
pub fn present_handoff_endpoint(
    endpoint: &HandoffEndpoint,
    model_name: Option<&str>,
    instance_name: Option<&str>,
) -> HandoffEndpointPresentation {
    let model = endpoint
        .model
        .as_deref()
        .map(js_trim)
        .filter(|model| !model.is_empty());
    let provider_name = instance_name.unwrap_or(&endpoint.instance).to_owned();
    let label = model
        .and(model_name)
        .or(model)
        .unwrap_or(&provider_name)
        .to_owned();
    HandoffEndpointPresentation {
        tooltip: format!("{provider_name} · {label}"),
        label,
        provider_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        CompletionWake, ContextDelivery, ContextDeliveryStatus, DeliveryState, HistoricalContext,
        HistoricalMessage, MessageId, ModelSelection, NotificationSource, Role, RunAttemptId, Task,
        TurnItemId,
    };
    use std::collections::BTreeMap;

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
    }
    fn item(kind: ItemKind, status: ItemStatus, text: &str) -> Item {
        Item {
            id: TurnItemId::new("item").unwrap(),
            run: Some(RunId::new("run-1").unwrap()),
            attempt: None,
            native_key: String::new(),
            ordinal: 1,
            kind,
            status,
            text: text.into(),
            started_at: at("2026-03-17T19:12:28.000Z"),
            completed_at: None,
            output_omitted: false,
            output_indicates_failure: false,
        }
    }
    fn task(status: ItemStatus) -> Task {
        Task {
            original_message: None,
            native_task: None,
            background: false,
            id: NodeId::new("task-1").unwrap(),
            native_key: String::new(),
            run: Some(RunId::new("run-1").unwrap()),
            attempt: RunAttemptId::new("attempt-1").unwrap(),
            child_thread: ThreadId::new("child").unwrap(),
            parent_task: None,
            prompt: "Review".into(),
            title: Some("Reviewer".into()),
            started_at: at("2026-03-17T19:12:28.000Z"),
            completed_at: None,
            model: Some("gpt-6".into()),
            status,
            result: None,
            progress: None,
            wake: CompletionWake::Always,
            delivery: DeliveryState::Pending,
            generation: 0,
        }
    }
    fn run(id: &str, ordinal: u64, instance: &str, model: &str) -> Run {
        Run {
            restart_of: None,
            restart_cancelled_work: vec![],
            checkpoint_scope: None,
            native_baseline_heads: BTreeMap::new(),
            id: RunId::new(id).unwrap(),
            ordinal,
            message: MessageId::new(format!("message-{id}")).unwrap(),
            selection: ModelSelection {
                instance: instance.into(),
                driver: Driver::Codex,
                model: model.into(),
                options: BTreeMap::new(),
            },
            status: RunStatus::Completed,
            attempt: None,
            queue_position: None,
            queue_held: false,
            requested_at: at("2026-03-17T19:12:28.000Z"),
            started_at: None,
            completed_at: None,
            source_plan: None,
            checkpoint: None,
            continuation: false,
        }
    }
    fn history_message(run: &str) -> HistoricalMessage {
        HistoricalMessage {
            role: Role::User,
            text: "hi".into(),
            thread: "thread".into(),
            run: Some(run.into()),
            item: format!("item-{run}"),
            provider_thread: None,
            status: "completed".into(),
            kind: "user_message".into(),
            run_status: None,
        }
    }
    fn handoff(id: &str, instance: &str, covered: &[&str], target: Option<&str>) -> Transfer {
        Transfer {
            native_source: None,
            instance: Some(instance.into()),
            target_run: None,
            delivery: target.map(|run| ContextDelivery {
                attempt: RunAttemptId::new("attempt").unwrap(),
                run: RunId::new(run).unwrap(),
                native_thread: None,
                status: ContextDeliveryStatus::Injected,
                item_ids: vec![],
                omitted_item_ids: vec![],
            }),
            id: ContextTransferId::new(id).unwrap(),
            kind: TransferKind::ProviderHandoff,
            source: ThreadId::new("thread").unwrap(),
            target: ThreadId::new("thread").unwrap(),
            boundary: 0,
            history: HistoricalContext {
                messages: covered.iter().map(|run| history_message(run)).collect(),
                context: String::new(),
                omitted_items: 0,
                omitted_item_ids: vec![],
            },
            superseded: false,
        }
    }
    fn endpoint(instance: &str, model: Option<&str>) -> HandoffEndpoint {
        HandoffEndpoint {
            instance: instance.into(),
            model: model.map(Into::into),
        }
    }

    #[test]
    fn preserves_stamped_models_including_several_models_from_the_same_provider() {
        let state = State {
            runs: vec![
                run("source-a", 1, "codex_personal", "source-a"),
                run("source-b", 2, "codex_personal", "source-b"),
                run("target", 3, "claudeAgent", "destination"),
            ],
            transfers: vec![handoff(
                "handoff",
                "claudeAgent",
                &["source-a", "source-b"],
                Some("target"),
            )],
            ..State::default()
        };
        assert_eq!(
            resolve_handoff_endpoints(&state, &state.transfers[0]),
            HandoffEndpoints {
                from: vec![
                    endpoint("codex_personal", Some("source-a")),
                    endpoint("codex_personal", Some("source-b")),
                ],
                to: endpoint("claudeAgent", Some("destination")),
            }
        );
    }

    #[test]
    fn does_not_borrow_a_target_model_from_another_provider() {
        let state = State {
            runs: vec![run("target", 2, "codex_personal", "wrong-model")],
            transfers: vec![handoff("handoff", "claudeAgent", &[], Some("target"))],
            ..State::default()
        };
        assert_eq!(
            resolve_handoff_endpoints(&state, &state.transfers[0]).to,
            endpoint("claudeAgent", None)
        );
    }

    #[test]
    fn places_one_divider_before_each_run_that_switched_providers() {
        let mut delta = handoff("delta", "claudeAgent", &["second"], Some("fourth"));
        delta.kind = TransferKind::ProviderHandoffDelta;
        let mut rolled_back = run("rolled", 5, "claudeAgent", "fable");
        rolled_back.status = RunStatus::RolledBack;
        let state = State {
            runs: vec![
                run("first", 1, "codex", "gpt"),
                run("second", 2, "codex", "gpt-mini"),
                run("third", 3, "claudeAgent", "fable"),
                run("fourth", 4, "claudeAgent", "fable"),
                rolled_back,
            ],
            transfers: vec![
                handoff("pending", "claudeAgent", &["first"], None),
                handoff("same", "claudeAgent", &["third"], Some("fourth")),
                delta,
                handoff("full", "claudeAgent", &["first"], Some("third")),
                handoff("rolled", "claudeAgent", &["first"], Some("rolled")),
                handoff("restore", "codex", &["first"], Some("second")),
            ],
            ..State::default()
        };
        let dividers = handoff_dividers(&state);
        assert_eq!(
            dividers
                .iter()
                .map(|divider| (
                    divider.run.as_str(),
                    divider
                        .transfers
                        .iter()
                        .map(ContextTransferId::as_str)
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>(),
            [("third", vec!["full"]), ("fourth", vec!["same", "delta"])]
        );
        assert_eq!(dividers[0].label, "Context handoff");
        assert_eq!(
            dividers[1].endpoints,
            HandoffEndpoints {
                from: vec![
                    endpoint("codex", Some("gpt-mini")),
                    endpoint("claudeAgent", Some("fable")),
                ],
                to: endpoint("claudeAgent", Some("fable")),
            }
        );
    }

    #[test]
    fn renders_context_handoffs_as_from_to_model_endpoints_instead_of_the_summary() {
        let stamped = present_handoff_endpoint(
            &endpoint("codex_personal", Some("gpt-5.6-sol")),
            Some("GPT 5.6 Sol"),
            Some("Codex Personal"),
        );
        assert_eq!(stamped.label, "GPT 5.6 Sol");
        assert_eq!(stamped.tooltip, "Codex Personal · GPT 5.6 Sol");
        let bare = present_handoff_endpoint(
            &endpoint("codex_personal", None),
            None,
            Some("Codex Personal"),
        );
        assert_eq!(bare.label, "Codex Personal");
        let unlisted = present_handoff_endpoint(&endpoint("custom", Some(" m ")), None, None);
        assert_eq!(
            (unlisted.label.as_str(), unlisted.provider_name.as_str()),
            ("m", "custom")
        );
    }

    #[test]
    fn presents_interrupts_and_compactions_as_dividers() {
        let state = State::default();
        let request = item(
            ItemKind::RunInterruptRequest,
            ItemStatus::Completed,
            "Waiting for the provider to stop.",
        );
        assert_eq!(
            lifecycle_row(&state, &request, false),
            Some(LifecycleRow::InterruptRequest(InterruptRequestRow {
                label: "Interrupt requested".into(),
                message: "Waiting for the provider to stop.".into(),
                created_at: at("2026-03-17T19:12:28.000Z"),
            }))
        );
        let result = item(
            ItemKind::RunInterruptResult {
                request: TurnItemId::new("request").unwrap(),
            },
            ItemStatus::Completed,
            "Stopped by user.",
        );
        let Some(LifecycleRow::Divider(divider)) = lifecycle_row(&state, &result, false) else {
            panic!("divider");
        };
        assert_eq!(
            (
                divider.label.as_str(),
                divider.detail.as_deref(),
                divider.tone
            ),
            (
                "Run interrupted",
                Some("Stopped by user."),
                DividerTone::Danger
            )
        );
        for (status, before, after, text, label, detail) in [
            (
                ItemStatus::Completed,
                Some(1200),
                Some(300),
                "",
                "Context compacted",
                Some("1200 → 300 tokens"),
            ),
            (
                ItemStatus::Running,
                None,
                Some(300),
                "",
                "Compacting context",
                Some("? → 300 tokens"),
            ),
            (
                ItemStatus::Failed,
                None,
                None,
                "",
                "Context compaction failed",
                None,
            ),
            (
                ItemStatus::Interrupted,
                Some(5),
                None,
                "Summary",
                "Context compaction stopped",
                Some("Summary"),
            ),
        ] {
            let compaction = item(ItemKind::Compaction { before, after }, status, text);
            let Some(LifecycleRow::Divider(divider)) = lifecycle_row(&state, &compaction, false)
            else {
                panic!("divider");
            };
            assert_eq!(
                (divider.label.as_str(), divider.detail.as_deref()),
                (label, detail)
            );
        }
    }

    #[test]
    fn shimmers_only_the_compaction_of_the_unsettled_run() {
        let compaction = item(
            ItemKind::Compaction {
                before: None,
                after: None,
            },
            ItemStatus::Running,
            "",
        );
        let run = RunId::new("run-1").unwrap();
        let other = RunId::new("run-2").unwrap();
        assert!(compaction_divider_active(&compaction, Some(&run)));
        assert!(!compaction_divider_active(&compaction, Some(&other)));
        assert!(!compaction_divider_active(&compaction, None));
    }

    #[test]
    fn links_a_fork_back_to_its_source_conversation() {
        let fork = item(
            ItemKind::Fork {
                parent: ThreadId::new("parent").unwrap(),
                boundary: 2,
            },
            ItemStatus::Completed,
            "",
        );
        let Some(LifecycleRow::Divider(divider)) = lifecycle_row(&State::default(), &fork, false)
        else {
            panic!("divider");
        };
        assert_eq!(divider.label, "Forked from conversation");
        assert_eq!(
            divider.action,
            Some(DividerAction {
                label: "Open source conversation".into(),
                thread: ThreadId::new("parent").unwrap(),
            })
        );
    }

    #[test]
    fn renders_created_threads_as_lean_rows_with_inline_chat_links() {
        let created = |title: &str| {
            item(
                ItemKind::ThreadCreated {
                    thread: ThreadId::new("thread-2").unwrap(),
                    run: Some(RunId::new("run-2").unwrap()),
                    title: title.into(),
                    instance: "claude-default".into(),
                    model: "claude-sonnet-4-6".into(),
                },
                ItemStatus::Completed,
                "",
            )
        };
        let row = |item: &Item, summary| match lifecycle_row(&State::default(), item, summary) {
            Some(LifecycleRow::CreatedThread(row)) => row,
            other => panic!("{other:?}"),
        };
        let lean = row(&created("Claude research thread"), false);
        assert_eq!(lean.layout, CreatedThreadLayout::WorkLogRow);
        assert_eq!(lean.label, "Created thread · Claude research thread");
        assert_eq!(lean.accessibility_label, "Open Claude research thread");
        assert_eq!(lean.action_label, "Open chat");
        let card = row(&created(""), true);
        assert_eq!(card.layout, CreatedThreadLayout::ResourceCard);
        assert_eq!(card.label, "Created thread");
        assert_eq!(card.accessibility_label, "Open created thread");
        assert_eq!(row(&created(""), false).label, "Created thread");
    }

    #[test]
    fn presents_subagents_with_status_and_a_plain_detail() {
        let subagent = item(
            ItemKind::Subagent {
                task: NodeId::new("task-1").unwrap(),
            },
            ItemStatus::Running,
            "",
        );
        let mut running = task(ItemStatus::Running);
        running.title = Some("Subagent: /root/review_math/".into());
        running.progress = Some("- Reading [math](src/math.ts) `add`\n  * next".into());
        running.result = Some("old".into());
        let state = State {
            runs: vec![run("run-1", 1, "codex", "gpt")],
            tasks: vec![running],
            ..State::default()
        };
        let Some(LifecycleRow::Subagent(link)) = lifecycle_row(&state, &subagent, false) else {
            panic!("subagent");
        };
        assert_eq!(link.title, "Review Math");
        assert_eq!(link.open_label, "Open Review Math");
        assert_eq!(link.detail.as_deref(), Some("Reading math add next"));
        assert_eq!(link.status_label, "Running");
        assert!(link.status_label_beside_title);
        assert_eq!(link.driver, Some(Driver::Codex));
        assert_eq!(link.thread, Some(ThreadId::new("child").unwrap()));

        let mut done = task(ItemStatus::Completed);
        done.result = Some("src/math.ts".into());
        done.progress = Some("working".into());
        let state = State {
            tasks: vec![done],
            ..State::default()
        };
        let Some(LifecycleRow::Subagent(link)) = lifecycle_row(&state, &subagent, false) else {
            panic!("subagent");
        };
        assert_eq!(link.live_status, ItemStatus::Completed);
        assert_eq!(link.detail.as_deref(), Some("src/math.ts"));
        assert!(link.detail_is_path);
        assert!(!link.status_label_beside_title);

        let mut failed = task(ItemStatus::Failed);
        failed.result = Some("Child task ended with status failed.".into());
        let state = State {
            tasks: vec![failed],
            ..State::default()
        };
        let Some(LifecycleRow::Subagent(link)) = lifecycle_row(&state, &subagent, false) else {
            panic!("subagent");
        };
        assert_eq!((link.detail, link.failed), (None, true));
        assert_eq!(link.status_label, "Failed");
        assert_eq!(
            subagent_status_visual(ItemStatus::Interrupted).label,
            "Stopped"
        );
        assert_eq!(
            subagent_status_visual(ItemStatus::Pending).dot,
            SubagentDot::Info
        );
    }

    #[test]
    fn draws_a_subagent_notification_as_its_card_with_the_reported_outcome() {
        let mut working = task(ItemStatus::Running);
        working.result = Some("Fixed it".into());
        let state = State {
            tasks: vec![working],
            ..State::default()
        };
        let notification = |outcome, child: Option<&str>| Notification {
            source: NotificationSource::Delegated {
                task_ids: vec![NodeId::new("task-1").unwrap()],
            },
            child_thread: child.map(|child| ThreadId::new(child).unwrap()),
            outcome,
            summary: "Delegated task finished".into(),
            detail: None,
        };
        let created = at("2026-03-17T19:13:00.000Z");
        let link = subagent_notification_link(
            &state,
            &notification(NotificationOutcome::Completed, Some("child")),
            &created,
        )
        .unwrap();
        assert_eq!(link.status, Some(ItemStatus::Completed));
        assert_eq!(link.live_status, ItemStatus::Running);
        assert_eq!(link.status_label, "Finished");
        assert_eq!(link.detail.as_deref(), Some("Fixed it"));
        assert!(link.status_label_beside_title);
        assert_eq!(link.event_at, Some(created.clone()));
        let updated = subagent_notification_link(
            &state,
            &notification(NotificationOutcome::Updated, Some("child")),
            &created,
        )
        .unwrap();
        assert_eq!(
            (updated.status, updated.status_label.as_str()),
            (None, "Updated")
        );
        assert_eq!(
            subagent_notification_link(
                &state,
                &notification(NotificationOutcome::Failed, Some("other")),
                &created
            ),
            None
        );
    }
}
