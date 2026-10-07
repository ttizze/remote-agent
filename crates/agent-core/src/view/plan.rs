//! The task list of the working run, the latest proposed plan and whether the
//! composer offers to implement or refine it.
use crate::commands::build::proposed_plan_title;
use crate::state::Snapshot;
use crate::view::requests::pending_questions;
use agent_domain::{InteractionMode, ItemKind, Plan, PlanId, PlanKind, RunId, RunStatus, State};

pub const PLAN_READY: &str = "Plan ready";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlanStepView {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivePlanView {
    pub created_at_ms: i64,
    pub run_id: String,
    pub steps: Vec<PlanStepView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProposedPlanView {
    pub id: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub run_id: String,
    pub markdown: String,
    pub title: Option<String>,
    /// Not yet implemented by a later run.
    pub actionable: bool,
}

/// The composer's task progress for the working run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TasksProgressView {
    pub step: String,
    pub completed_steps: u32,
    pub total_steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlanFollowUpBanner {
    pub label: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlanView {
    /// The task list of the working run, or of the latest run with one.
    pub active_plan: Option<ActivePlanView>,
    pub tasks: Option<TasksProgressView>,
    /// The latest proposed plan once the latest run settled.
    pub active_proposed_plan: Option<ProposedPlanView>,
    /// The composer offers Implement / Refine instead of an ordinary send.
    pub show_plan_follow_up_prompt: bool,
    pub banner: Option<PlanFollowUpBanner>,
}

/// Providers report `inProgress` (Codex) or `in_progress` (Claude).
fn step_status(status: &str) -> PlanStepStatus {
    match status {
        "completed" => PlanStepStatus::Completed,
        "inProgress" | "in_progress" | "running" => PlanStepStatus::InProgress,
        _ => PlanStepStatus::Pending,
    }
}

/// When the plan's newest item last changed, else when the thread did.
fn plan_item_time(state: &State, plan: &PlanId) -> i64 {
    state
        .items
        .iter()
        .rev()
        .find(|item| {
            matches!(&item.kind, ItemKind::ProposedPlan { plan: id } | ItemKind::TodoList { plan: id } if id == plan)
        })
        .map(|item| item.completed_at.as_ref().unwrap_or(&item.started_at).millis())
        .or_else(|| state.thread.as_ref().map(|thread| thread.updated_at.millis()))
        .unwrap_or_default()
}

/// Bounded snapshots keep a proposed plan's text on its item only.
pub fn proposed_plan_markdown(state: &State, plan: &Plan) -> String {
    if !plan.markdown.is_empty() {
        return plan.markdown.clone();
    }
    state
        .items
        .iter()
        .find(|item| matches!(&item.kind, ItemKind::ProposedPlan { plan: id } if id == &plan.id))
        .map(|item| item.text.clone())
        .unwrap_or_default()
}

/// The task list of `latest_run`, else the newest one.
pub fn active_plan_state(state: &State, latest_run: Option<&RunId>) -> Option<ActivePlanView> {
    let plans: Vec<&Plan> = state
        .plans
        .iter()
        .filter(|plan| plan.kind == PlanKind::Todo)
        .collect();
    let plan = plans
        .iter()
        .rev()
        .find(|plan| Some(&plan.run) == latest_run)
        .or(plans.last())?;
    if plan.steps.is_empty() {
        return None;
    }
    Some(ActivePlanView {
        created_at_ms: plan_item_time(state, &plan.id),
        run_id: plan.run.to_string(),
        steps: plan
            .steps
            .iter()
            .map(|step| PlanStepView {
                step: step.text.clone(),
                status: step_status(&step.status),
            })
            .collect(),
    })
}

/// The newest proposed plan of `latest_run`, else of the thread.
pub fn latest_proposed_plan(state: &State, latest_run: Option<&RunId>) -> Option<ProposedPlanView> {
    let plans: Vec<&Plan> = state
        .plans
        .iter()
        .filter(|plan| plan.kind == PlanKind::Proposed)
        .collect();
    let of_run: Vec<&Plan> = match latest_run {
        Some(run) => plans
            .iter()
            .copied()
            .filter(|plan| &plan.run == run)
            .collect(),
        None => plans.clone(),
    };
    let candidates = if of_run.is_empty() { plans } else { of_run };
    let plan = candidates.into_iter().max_by(|left, right| {
        plan_item_time(state, &left.id)
            .cmp(&plan_item_time(state, &right.id))
            .then_with(|| left.id.cmp(&right.id))
    })?;
    let updated_at = plan_item_time(state, &plan.id);
    let markdown = proposed_plan_markdown(state, plan);
    Some(ProposedPlanView {
        id: plan.id.to_string(),
        created_at_ms: updated_at,
        updated_at_ms: updated_at,
        run_id: plan.run.to_string(),
        title: proposed_plan_title(&markdown),
        markdown,
        actionable: plan.implemented_by.is_none(),
    })
}

pub fn has_actionable_proposed_plan(plan: Option<&ProposedPlanView>) -> bool {
    plan.is_some_and(|plan| plan.actionable)
}

/// A run is settled once it stopped and is no longer the active run.
pub fn is_latest_run_settled(
    latest: Option<(&RunId, RunStatus)>,
    active_run: Option<&RunId>,
) -> bool {
    let Some((run, status)) = latest else {
        return false;
    };
    status.terminal() && active_run != Some(run)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanFollowUpGates {
    pub pending_user_input_count: usize,
    pub interaction_mode: InteractionMode,
    pub latest_turn_settled: bool,
    pub has_actionable_proposed_plan: bool,
    pub has_composer_attachments: bool,
}

pub fn should_show_plan_follow_up_prompt(gates: PlanFollowUpGates) -> bool {
    gates.pending_user_input_count == 0
        && gates.interaction_mode == InteractionMode::Plan
        && gates.latest_turn_settled
        && gates.has_actionable_proposed_plan
        && !gates.has_composer_attachments
}

fn run_status(state: &State, id: &RunId) -> Option<RunStatus> {
    state
        .runs
        .iter()
        .find(|run| &run.id == id)
        .map(|run| run.status)
}

/// `interaction_mode` and `has_composer_attachments` come from the
/// thread's composer draft.
pub fn plan_view(
    state: &State,
    interaction_mode: InteractionMode,
    has_composer_attachments: bool,
) -> PlanView {
    let shell = agent_domain::shell(state);
    let latest = shell.as_ref().and_then(|shell| shell.latest_run.as_ref());
    let active = shell.as_ref().and_then(|shell| shell.active_run.as_ref());
    let with_status = |run: Option<&RunId>| {
        let run = run?;
        Some((run.clone(), run_status(state, run)?))
    };
    let latest = with_status(latest);
    let activity = state
        .active_run()
        .map(|run| (run.id.clone(), run.status))
        .or_else(|| latest.clone());
    let latest_settled =
        is_latest_run_settled(latest.as_ref().map(|(id, status)| (id, *status)), active);
    let active_plan = active_plan_state(state, activity.as_ref().map(|(id, _)| id));
    let activity_settled =
        is_latest_run_settled(activity.as_ref().map(|(id, status)| (id, *status)), active);
    let tasks = active_plan
        .as_ref()
        .filter(|plan| {
            !activity_settled
                && activity
                    .as_ref()
                    .is_some_and(|(id, _)| plan.run_id == id.as_str())
        })
        .and_then(|plan| {
            let last = plan.steps.last()?;
            let step = [PlanStepStatus::InProgress, PlanStepStatus::Pending]
                .iter()
                .find_map(|status| plan.steps.iter().find(|step| step.status == *status))
                .unwrap_or(last);
            Some(TasksProgressView {
                step: step.step.clone(),
                completed_steps: plan
                    .steps
                    .iter()
                    .filter(|step| step.status == PlanStepStatus::Completed)
                    .count() as u32,
                total_steps: plan.steps.len() as u32,
            })
        });
    let active_proposed_plan = latest_settled
        .then(|| latest_proposed_plan(state, latest.as_ref().map(|(id, _)| id)))
        .flatten();
    let show_plan_follow_up_prompt = should_show_plan_follow_up_prompt(PlanFollowUpGates {
        pending_user_input_count: pending_questions(state).len(),
        interaction_mode,
        latest_turn_settled: latest_settled,
        has_actionable_proposed_plan: has_actionable_proposed_plan(active_proposed_plan.as_ref()),
        has_composer_attachments,
    });
    PlanView {
        banner: active_proposed_plan
            .as_ref()
            .filter(|_| show_plan_follow_up_prompt)
            .map(|plan| PlanFollowUpBanner {
                label: PLAN_READY.into(),
                title: plan.title.clone(),
            }),
        active_plan,
        tasks,
        active_proposed_plan,
        show_plan_follow_up_prompt,
    }
}

/// The selected thread's plan state with its composer draft.
pub fn selected_plan_view(snapshot: &Snapshot) -> Option<PlanView> {
    let state = snapshot.selected_state()?;
    let draft = snapshot.current_draft();
    Some(plan_view(
        state,
        draft.interaction_mode,
        !draft.attachments.is_empty(),
    ))
}

#[cfg(test)]
mod tests;
