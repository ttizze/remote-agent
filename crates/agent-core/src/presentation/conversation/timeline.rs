//! A turn as T3 Code's timeline shows it. Once the turn completes, its work
//! folds under "Worked for …"; while it runs, a working header leads it and
//! its latest work is the live row. Bex keeps its existing disclosure for
//! failed and interrupted work, with the terminal outcome outside the fold.
//! Open work reads as the commentary between it, groups of tools under their
//! summary ("Ran 2 commands") and a lone tool on its own row.
use super::proposed_plan::plan_title;
use super::{
    ActivityPresentation, ConversationRow, ConversationRowContent, ItemSource, LiveActivity,
    RenderedItem, RenderedTurn, activity_is_expanded,
};
use crate::localization::{UiLanguage, localized_ui_format};
use crate::models::{ItemBody, TurnStatus};
use std::{collections::HashSet, sync::Arc};

/// Existing surfaces choose their visual contracts independently. Projection,
/// admission and row identity remain shared for every combination.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TimelineStyle {
    pub terminal_work: WorkDisclosure,
    pub plans: PlanPresentation,
    pub group_subagents: bool,
    pub background_footer: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkDisclosure {
    Completed,
    Settled,
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlanPresentation {
    Card,
    Message,
}
impl TimelineStyle {
    pub const T3: Self = Self {
        terminal_work: WorkDisclosure::Completed,
        plans: PlanPresentation::Card,
        group_subagents: true,
        background_footer: false,
    };
    pub const NATIVE: Self = Self {
        terminal_work: WorkDisclosure::Settled,
        plans: PlanPresentation::Message,
        group_subagents: false,
        background_footer: true,
    };
}

/// One row of a turn on T3 Code's timeline.
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TurnRow {
    /// A row of the turn's projection as it is: the user's message, the
    /// answer, an error, a notification.
    Row(ConversationRow),
    /// The completed turn's fold, its work shown while `open`.
    Fold {
        activity: ActivityPresentation,
        open: bool,
    },
    /// The running turn's header ("Working for 3s").
    Working,
    /// Commentary between the work.
    Commentary(Arc<RenderedItem>),
    /// Work under one toggle; its members follow it while `open`.
    Group {
        id: String,
        summary: String,
        /// What its tools are, as an item's kind (reasoning alone is
        /// `reasoning`), or `mixed`.
        kind: String,
        attempt: Option<uuid::Uuid>,
        open: bool,
    },
    /// A tool's row, alone or a member of an open group.
    Work { row: ConversationRow, label: String },
    /// The running group's current work. Its toggle opens all group members.
    Live {
        row: ConversationRow,
        label: String,
        group_id: String,
        kind: String,
        open: bool,
        /// The latest thought in this group, above the status only while closed.
        thought: Option<String>,
    },
    /// A running turn with nothing live.
    Thinking,
    /// The turn waits for the user's answer to a request, which T3 Code's
    /// live row names: what an approval is for, or that input is requested.
    Requested {
        label: String,
        request: super::Request,
    },
    /// The provider stream dropped and is being retried, which the error's
    /// title says.
    Retrying { label: String },
    /// A subagent the turn started, as T3 Code's subagent row: its name and
    /// its latest word.
    Subagent {
        id: String,
        agent: crate::presentation::agents::SubagentPresentation,
        attempt: Option<uuid::Uuid>,
    },
    /// Adjacent children in the same provider turn share a disclosure.
    Subagents {
        id: String,
        label: String,
        members: Vec<SubagentMember>,
        open: bool,
    },
    /// A divider across the work, as T3 Code's context compaction.
    Divider { id: String, label: String },
    Plan {
        id: String,
        title: String,
        text: String,
    },
    /// The actions (fork, copy, time) of the settled turn's last answer when
    /// work ends the turn below it: as T3 Code's, they follow that work and
    /// stay in view.
    Actions(ConversationRow),
    /// The turn was interrupted.
    Interrupted,
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentMember {
    pub id: String,
    pub agent: crate::presentation::agents::SubagentPresentation,
}

/// A disclosure which must be open to show a selected message. The renderer
/// owns the expansion state and applies these values idempotently.
#[derive(Clone)]
pub(super) struct MessageDisclosure {
    pub id: String,
    pub status: Option<String>,
}

fn work_is_foldable(style: TimelineStyle, status: TurnStatus, can_collapse: bool) -> bool {
    can_collapse
        && match style.terminal_work {
            WorkDisclosure::Completed => status == TurnStatus::Completed,
            WorkDisclosure::Settled => matches!(
                status,
                TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
            ),
        }
}

/// Search ordinary messages independently of the current work disclosures.
/// Reuse the timeline's classification, including superseded attempts; the
/// result also tells its owner exactly which disclosures hide each message.
pub(super) fn turn_message_search_rows(
    style: TimelineStyle,
    turn: &RenderedTurn,
) -> Vec<(TurnRow, Vec<MessageDisclosure>)> {
    let attempts = super::attempts::attempt_folds(&turn.source);
    let groups = attempts
        .iter()
        .map(|fold| format!("attempt:{}", fold.id))
        .collect();
    let mut expanded = std::collections::HashMap::new();
    let mut paths = std::collections::HashMap::new();
    let mut parent = None;
    for row in &turn.rows {
        match &row.content {
            ConversationRowContent::ActivityHeader { activity } => {
                parent =
                    work_is_foldable(style, turn.source.status, activity.activity_can_collapse)
                        .then(|| MessageDisclosure {
                            id: activity.id.clone(),
                            status: Some(activity.status.clone()),
                        });
                if let Some(parent) = &parent {
                    expanded.insert(parent.id.clone(), activity.status.clone());
                }
            }
            ConversationRowContent::Activity { item, .. } if is_commentary(item) => {
                let mut path: Vec<_> = parent.iter().cloned().collect();
                if let ItemSource::Native(native) = &item.source {
                    for fold in &attempts {
                        if fold.items.contains(&native.id) {
                            path.push(MessageDisclosure {
                                id: format!("attempt:{}", fold.id),
                                status: None,
                            });
                        }
                    }
                }
                paths.insert(item.data.id.clone(), path);
            }
            _ => {}
        }
    }
    turn_rows(style, turn, &expanded, &groups)
        .into_iter()
        .filter_map(|row| match &row {
            TurnRow::Commentary(item) => {
                let path = paths.remove(&item.data.id).unwrap_or_default();
                Some((row, path))
            }
            TurnRow::Row(ConversationRow {
                content:
                    ConversationRowContent::User { .. } | ConversationRowContent::Response { .. },
                ..
            }) => Some((row, Vec::new())),
            _ => None,
        })
        .collect()
}

/// The turn's rows: folds open by `expanded` (a fold's ID and the status it
/// was opened at, as [`activity_is_expanded`]) and groups open by `groups`.
pub(super) fn turn_rows(
    style: TimelineStyle,
    turn: &RenderedTurn,
    expanded: &std::collections::HashMap<String, String>,
    groups: &HashSet<String>,
) -> Vec<TurnRow> {
    let language = turn.language;
    let status = turn.source.status;
    let running = status == TurnStatus::Running;
    let live = turn.live_activity();
    let show_running_work = running
        && turn
            .requests
            .iter()
            .all(|request| super::approval_input(&request.body).is_some());
    let mut out = Vec::new();
    let mut work: Vec<&ConversationRow> = Vec::new();
    let mut hidden = false;
    let attempt_folds = super::attempts::attempt_folds(&turn.source);
    let folded_items: std::collections::HashMap<_, _> = attempt_folds
        .iter()
        .flat_map(|fold| fold.items.iter().map(move |item| (item.as_str(), fold)))
        .collect();
    // Core already identified a unique live row. A notice outside the work
    // must not retire its preceding command merely by splitting the layout.
    let flush = |work: &mut Vec<&ConversationRow>, out: &mut Vec<TurnRow>| {
        let rows = std::mem::take(work);
        let live = current_work(&rows, show_running_work, live.as_ref());
        if let Some(row) = live {
            out.extend(live_group(language, &rows, row, groups));
            return;
        }
        pieces(language, &rows, status == TurnStatus::Failed, groups, out);
    };
    for row in &turn.rows {
        if let Some(ItemSource::Native(native)) = activity_item(row).map(|item| &item.source)
            && let Some(fold) = folded_items.get(native.id.as_str())
        {
            let id = fold.group_id();
            let open = groups.contains(&id);
            if fold.items.first() == Some(&native.id) && !hidden {
                flush(&mut work, &mut out);
                out.push(TurnRow::Group {
                    id,
                    summary: language
                        .text(if fold.by_steer {
                            "Cut off by a steer"
                        } else {
                            "Superseded attempt"
                        })
                        .into(),
                    kind: "attempt".into(),
                    attempt: Some(fold.id),
                    open,
                });
            }
            if !open {
                continue;
            }
        }
        if style.plans == PlanPresentation::Message
            && let Some(item) = activity_item(row)
            && let ItemSource::Native(native) = &item.source
            && matches!(native.body(), ItemBody::Plan { .. })
        {
            flush(&mut work, &mut out);
            out.push(TurnRow::Row(plan_message(turn, row, item)));
            continue;
        }
        match &row.content {
            ConversationRowContent::ActivityHeader { activity } => {
                flush(&mut work, &mut out);
                hidden = false;
                if work_is_foldable(style, status, activity.activity_can_collapse) {
                    let open =
                        activity_is_expanded(&activity.status, expanded.get(&activity.id).cloned());
                    hidden = !open;
                    out.push(TurnRow::Fold {
                        activity: activity.clone(),
                        open,
                    });
                }
            }
            ConversationRowContent::Activity { item, .. } if is_commentary(item) => {
                flush(&mut work, &mut out);
                if !hidden {
                    out.push(TurnRow::Commentary(item.clone()));
                }
            }
            ConversationRowContent::Activity { item, .. } => match place(language, item) {
                Place::OutsideWork(rows) => {
                    flush(&mut work, &mut out);
                    out.extend(rows);
                }
                Place::Work if !hidden => work.push(row),
                Place::Alone(rows) => {
                    flush(&mut work, &mut out);
                    if !hidden {
                        out.extend(rows);
                    }
                }
                Place::Work | Place::Hidden => {}
            },
            ConversationRowContent::InProgress { .. } => {
                flush(&mut work, &mut out);
                if live == Some(LiveActivity::Thinking) {
                    out.push(TurnRow::Thinking);
                }
            }
            // As T3 Code, a request waiting for its answer and a retry of
            // the provider are the live work, the work before them its group.
            ConversationRowContent::PendingRequest { request } if request.approval.is_none() => {
                flush(&mut work, &mut out);
                out.push(TurnRow::Requested {
                    label: request_label(language, request),
                    request: (**request).clone(),
                });
            }
            ConversationRowContent::Error { error } if error.is_reconnecting => {
                work.clear();
                out.push(TurnRow::Retrying {
                    label: error.title.clone(),
                });
            }
            _ => {
                flush(&mut work, &mut out);
                hidden = false;
                out.push(TurnRow::Row(row.clone()));
            }
        }
    }
    flush(&mut work, &mut out);
    finish_turn(out, status, language, groups, style)
}

fn plan_message(
    turn: &RenderedTurn,
    row: &ConversationRow,
    item: &Arc<RenderedItem>,
) -> ConversationRow {
    let show_actions = super::shows_actions(turn.source.status, &item.data.kind, true);
    let terminal = turn.rows.iter().rev().find_map(|row| match &row.content {
        ConversationRowContent::Response { item, .. } => Some(item),
        ConversationRowContent::Activity { item, .. }
            if matches!(&item.source, ItemSource::Native(native) if matches!(native.body(), ItemBody::Plan { .. })) => Some(item),
        _ => None,
    }).is_some_and(|last| Arc::ptr_eq(last, item));
    ConversationRow {
        content: ConversationRowContent::Response {
            item: item.clone(),
            show_actions,
            fork_turn_id: (turn.supports_fork && show_actions && terminal)
                .then(|| turn.source.id.clone()),
        },
        ..row.clone()
    }
}

/// Turn-wide composition follows row projection: the header's stable position,
/// one live status, merged child cards and the answer's trailing actions.
fn finish_turn(
    mut out: Vec<TurnRow>,
    status: TurnStatus,
    language: UiLanguage,
    groups: &HashSet<String>,
    style: TimelineStyle,
) -> Vec<TurnRow> {
    if status == TurnStatus::Running {
        // As T3 Code, the header stays below the message that started the
        // turn, however the work below it grows.
        let at = out
            .iter()
            .rposition(|row| {
                matches!(row, TurnRow::Row(row) if matches!(row.content,
                    ConversationRowContent::User { .. } | ConversationRowContent::Notification { .. }))
            })
            .map_or(0, |index| index + 1);
        out.insert(at, TurnRow::Working);
        let retrying = out
            .iter()
            .any(|row| matches!(row, TurnRow::Retrying { .. }));
        let has_work = out.iter().any(|row| matches!(row, TurnRow::Live { .. }));
        out.retain(|row| match row {
            TurnRow::Live { .. } => !retrying,
            TurnRow::Thinking => !retrying && !has_work,
            _ => true,
        });
    }
    let mut out = merge_subagents(out);
    if style.group_subagents {
        out = group_subagents(out, language, groups);
    }
    trailing_actions(&mut out);
    if status == TurnStatus::Interrupted {
        out.push(TurnRow::Interrupted);
    }
    let mut previous_steer = false;
    for row in &mut out {
        let steer = matches!(row, TurnRow::Row(row) if row.steer);
        if let TurnRow::Row(row) = row {
            row.steer = steer && !previous_steer;
        }
        previous_steer = steer;
    }
    out
}

fn group_subagents(
    rows: Vec<TurnRow>,
    language: UiLanguage,
    groups: &HashSet<String>,
) -> Vec<TurnRow> {
    let mut rows = rows.into_iter().peekable();
    let mut out = Vec::new();
    while let Some(row) = rows.next() {
        let TurnRow::Subagent { id, agent, attempt } = row else {
            out.push(row);
            continue;
        };
        let mut members = vec![SubagentMember { id, agent }];
        while matches!(rows.peek(), Some(TurnRow::Subagent { attempt: next, .. }) if *next == attempt)
        {
            if let Some(TurnRow::Subagent { id, agent, .. }) = rows.next() {
                members.push(SubagentMember { id, agent });
            }
        }
        if members.len() == 1 {
            let SubagentMember { id, agent } = members.pop().unwrap();
            out.push(TurnRow::Subagent { id, agent, attempt });
        } else {
            let id = format!("subagent-group:{}", members[0].id);
            let open = groups.contains(&id);
            out.push(TurnRow::Subagents {
                id,
                label: localized_ui_format(
                    language,
                    "subagent.group.count".into(),
                    vec![members.len().to_string()],
                ),
                members: members.clone(),
                open,
            });
            if open {
                out.extend(members.into_iter().map(|member| TurnRow::Subagent {
                    id: member.id,
                    agent: member.agent,
                    attempt,
                }));
            }
        }
    }
    out
}

/// Earlier groups can still contain running tools while the tail is an approval
/// or commentary. Completed work only stays live at the active response's tail.
fn current_work<'a>(
    rows: &[&'a ConversationRow],
    show_running: bool,
    tail: Option<&LiveActivity>,
) -> Option<&'a ConversationRow> {
    if show_running
        && let Some(row) = rows.iter().rev().find(|row| {
            activity_item(row).is_some_and(|item| {
                visible(item)
                    && (item.data.kind != "reasoning"
                        || matches!(tail, Some(LiveActivity::Row(id)) if &row.id == id))
                    && matches!(&item.source, ItemSource::Native(native) if native.status == crate::models::ItemStatus::Running)
            })
        })
    {
        return Some(row);
    }
    match tail {
        Some(LiveActivity::Row(id)) => rows.iter().rev().find(|row| &row.id == id).copied(),
        _ => None,
    }
}

fn live_group(
    language: UiLanguage,
    rows: &[&ConversationRow],
    live: &ConversationRow,
    groups: &HashSet<String>,
) -> Vec<TurnRow> {
    let rows: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| activity_item(row).is_some_and(|item| visible(item)))
        .collect();
    let group_id = format!("group:{}", rows[0].id);
    let open = groups.contains(&group_id);
    let items: Vec<_> = rows.iter().filter_map(|row| activity_item(row)).collect();
    let thought = (!open)
        .then(|| {
            rows.iter().rev().find_map(|row| {
                let item = activity_item(row)?;
                let ItemSource::Native(native) = &item.source else {
                    return None;
                };
                super::super::body::reasoning_live_preview(native, item.data.preview.as_deref()?)
                    .map(|text| (&row.id, text))
            })
        })
        .flatten();
    let label = activity_item(live).map_or_else(String::new, |item| {
        if thought.as_ref().is_some_and(|(id, _)| *id == &live.id) {
            item.work_row_label(true).heading
        } else {
            live_label(language, item)
        }
    });
    let mut result = vec![TurnRow::Live {
        row: live.clone(),
        label,
        group_id,
        kind: group_kind(&items),
        open,
        thought: thought.map(|(_, text)| text),
    }];
    if open {
        result.extend(rows.into_iter().map(|row| work_row(language, row)));
    }
    result
}

/// A child keeps its first row's position while later activity updates it.
fn merge_subagents(rows: Vec<TurnRow>) -> Vec<TurnRow> {
    let mut out = Vec::new();
    let mut positions = std::collections::HashMap::new();
    for row in rows {
        if let TurnRow::Subagent { agent, attempt, .. } = &row
            && let Some(target) = &agent.target
        {
            if let Some(&index) = positions.get(&(target.clone(), *attempt)) {
                if let TurnRow::Subagent { agent: old, .. } = &mut out[index] {
                    if !agent.message.is_empty() {
                        old.message = agent.message.clone();
                    }
                    if agent.status != TurnStatus::Unknown {
                        old.status = agent.status;
                    }
                    if agent.result.is_some() {
                        old.result = agent.result.clone();
                    }
                    if agent.model.is_some() || agent.effort.is_some() {
                        let (model, effort) = crate::presentation::agents::subagent_selection(
                            old.model.as_deref(),
                            old.effort.as_deref(),
                            agent.model.as_deref(),
                            agent.effort.as_deref(),
                        );
                        let (model, effort) = (model.map(str::to_owned), effort.map(str::to_owned));
                        old.model = model;
                        old.effort = effort;
                    }
                }
                continue;
            }
            positions.insert((target.clone(), *attempt), out.len());
        }
        out.push(row);
    }
    out
}

/// As T3 Code's settled turn that ends in work after its last answer, the
/// answer and that work read as one response: the answer's actions move
/// below the work.
fn trailing_actions(out: &mut Vec<TurnRow>) {
    let answer = out.iter().rposition(|row| {
        matches!(
            row,
            TurnRow::Row(ConversationRow {
                content: ConversationRowContent::Response {
                    show_actions: true,
                    ..
                },
                ..
            })
        )
    });
    let Some(answer) = answer else { return };
    let work = out[answer + 1..]
        .iter()
        .take_while(|row| match row {
            TurnRow::Fold { .. } | TurnRow::Group { .. } | TurnRow::Work { .. } => true,
            TurnRow::Row(row) => matches!(row.content, ConversationRowContent::Error { .. }),
            _ => false,
        })
        .count();
    let Some(TurnRow::Row(row)) = out.get_mut(answer).filter(|_| work > 0) else {
        return;
    };
    let actions = row.clone();
    if let ConversationRowContent::Response {
        show_actions,
        fork_turn_id,
        ..
    } = &mut row.content
    {
        *show_actions = false;
        *fork_turn_id = None;
    }
    out.insert(answer + 1 + work, TurnRow::Actions(actions));
}

/// A request's live row, as T3 Code's: an approval by what it is for, other
/// requests as input requested.
fn request_label(language: UiLanguage, request: &super::Request) -> String {
    if request.request_body.is_approval() {
        if request.body.trim().is_empty() {
            request.title.clone()
        } else {
            request.body.trim().to_owned()
        }
    } else {
        language.text("入力を求めています").into()
    }
}

/// Where T3 Code's timeline shows a work item.
pub(super) enum Place {
    /// Among the work, in its group or alone.
    Work,
    /// Plans and notices remain visible when completed work collapses.
    OutsideWork(Vec<TurnRow>),
    /// A separate row within the work: a subagent or divider.
    Alone(Vec<TurnRow>),
    /// Not at all: a sleep, review mode entered or left.
    Hidden,
}

pub(super) fn place(language: UiLanguage, item: &RenderedItem) -> Place {
    let ItemSource::Native(native) = &item.source else {
        return Place::Work;
    };
    match native.body() {
        ItemBody::SystemNotice { message } => Place::OutsideWork(vec![TurnRow::Divider {
            id: item.data.id.clone(),
            label: message.clone(),
        }]),
        ItemBody::Plan { text } => Place::OutsideWork(vec![TurnRow::Plan {
            id: item.data.id.clone(),
            title: plan_title(text).unwrap_or_else(|| language.text("work.plan.proposed").into()),
            text: text.clone(),
        }]),
        ItemBody::Subagent { .. } => Place::Alone(
            crate::presentation::agents::subagent_rows(language, native)
                .into_iter()
                .enumerate()
                .map(|(index, agent)| TurnRow::Subagent {
                    id: format!("{}:{index}", item.data.id),
                    agent,
                    attempt: native.attempt,
                })
                .collect(),
        ),
        ItemBody::Compaction {} => Place::Alone(vec![TurnRow::Divider {
            id: item.data.id.clone(),
            label: language
                .text(if native.status == crate::models::ItemStatus::Running {
                    "work.compaction.running"
                } else {
                    "work.compaction.done"
                })
                .into(),
        }]),
        ItemBody::Sleep {} | ItemBody::Review { .. } => Place::Hidden,
        _ => Place::Work,
    }
}

fn activity_item(row: &ConversationRow) -> Option<&Arc<RenderedItem>> {
    match &row.content {
        ConversationRowContent::Activity { item, .. } => Some(item),
        _ => None,
    }
}

fn is_commentary(item: &RenderedItem) -> bool {
    matches!(item.data.kind.as_str(), "commentary" | "agent")
        && !matches!(&item.source, ItemSource::Native(native) if matches!(native.body(), ItemBody::Plan { .. }))
}

/// As T3 Code, reasoning shows only with a trace to show.
fn visible(item: &RenderedItem) -> bool {
    item.data.kind != "reasoning"
        || item
            .data
            .preview
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
}

/// Consecutive work: a lone tool on its own row, more under a group's toggle.
/// A failed turn's work is each on its own row, as T3 Code keeps it open.
fn pieces(
    language: UiLanguage,
    rows: &[&ConversationRow],
    failed: bool,
    groups: &HashSet<String>,
    out: &mut Vec<TurnRow>,
) {
    let rows: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| activity_item(row).is_some_and(|item| visible(item)))
        .collect();
    let work = |row: &ConversationRow| work_row(language, row);
    match rows.as_slice() {
        [] => {}
        [row] if failed || activity_item(row).is_some_and(|item| item.data.kind != "reasoning") => {
            out.push(work(row))
        }
        rows if failed => out.extend(rows.iter().map(|row| work(row))),
        rows => {
            let id = format!("group:{}", rows[0].id);
            let open = groups.contains(&id);
            let items: Vec<_> = rows.iter().filter_map(|row| activity_item(row)).collect();
            out.push(TurnRow::Group {
                summary: group_summary(language, &items),
                kind: group_kind(&items),
                attempt: None,
                id,
                open,
            });
            if open {
                out.extend(rows.iter().map(|row| work(row)));
            }
        }
    }
}

fn work_row(language: UiLanguage, row: &ConversationRow) -> TurnRow {
    TurnRow::Work {
        row: row.clone(),
        label: activity_item(row).map_or_else(String::new, |item| work_label(language, item)),
    }
}

/// What a tool did, as T3 Code's work row names it: a command as it ran, a
/// web search by its heading (its query is its detail), reasoning by its
/// trace, other work by its title.
fn work_label(language: UiLanguage, item: &RenderedItem) -> String {
    if let ItemSource::Native(native) = &item.source {
        match native.body() {
            ItemBody::CommandExecution { command, .. } => return command.trim().to_owned(),
            ItemBody::WebSearch { .. } => return language.text("work.web_search").into(),
            _ => {}
        }
    }
    let label = item.work_row_label(false);
    label.preview.unwrap_or(label.heading)
}

/// The live row, as T3 Code's: a command by its program ("Running pwd"),
/// reasoning by its trace or Thinking, other work by its title.
pub(super) fn live_label(language: UiLanguage, item: &RenderedItem) -> String {
    if let ItemSource::Native(native) = &item.source
        && let ItemBody::CommandExecution { command, .. } = native.body()
    {
        let program = program_name(command).unwrap_or_else(|| language.text("コマンド").into());
        return localized_ui_format(language, "work.live.running".into(), vec![program]);
    }
    let label = item.work_row_label(true);
    label.preview.unwrap_or(label.heading)
}

/// A command's program: its first word past variable assignments, without
/// its directory.
fn program_name(command: &str) -> Option<String> {
    let word = command
        .split_whitespace()
        .find(|word| !word.contains('=') || word.starts_with('-'))?;
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    Some(word.rsplit('/').next().unwrap_or(word).to_owned()).filter(|word| !word.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Command,
    Edit,
    Search,
    Other,
}

impl Action {
    fn of(item: &RenderedItem) -> Self {
        match (&item.source, item.data.kind.as_str()) {
            (_, "command") => Self::Command,
            (_, "fileChange") => Self::Edit,
            (ItemSource::Native(native), _)
                if matches!(native.body(), ItemBody::WebSearch { .. }) =>
            {
                Self::Search
            }
            _ => Self::Other,
        }
    }

    /// T3 Code names the actions that changed something first.
    fn priority(self) -> u8 {
        match self {
            Self::Command | Self::Edit => 0,
            Self::Search => 1,
            Self::Other => 2,
        }
    }

    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Self::Command => ("work.group.command", "work.group.commands"),
            Self::Edit => ("work.group.file", "work.group.files"),
            Self::Search => ("work.group.search", "work.group.searches"),
            Self::Other => ("work.group.tool", "work.group.tools"),
        }
    }
}

fn counted(language: UiLanguage, (one, many): (&str, &str), count: usize) -> String {
    localized_ui_format(
        language,
        if count == 1 { one } else { many }.into(),
        vec![count.to_string()],
    )
}

/// A group's summary, as T3 Code's: its tools by action, the two that
/// changed most named and the rest counted; reasoning alone is Thought.
pub(super) fn group_summary(language: UiLanguage, items: &[&Arc<RenderedItem>]) -> String {
    let tools: Vec<_> = items
        .iter()
        .filter(|item| item.data.kind != "reasoning")
        .collect();
    if tools.is_empty() {
        return match items.len() {
            1 => language.text("思考").into(),
            count => localized_ui_format(
                language,
                "work.group.thoughts".into(),
                vec![count.to_string()],
            ),
        };
    }
    // As T3 Code, an MCP server's tools are named by their integration,
    // apart from the actions counted.
    let mut sources: Vec<&str> = Vec::new();
    let tools: Vec<_> = tools
        .into_iter()
        .filter(|item| match integration(item) {
            Some(server) => {
                if !sources.contains(&server) {
                    sources.push(server);
                }
                false
            }
            None => true,
        })
        .collect();
    let mut actions: Vec<(Action, usize)> = Vec::new();
    for item in &tools {
        let action = Action::of(item);
        match actions.iter_mut().find(|(known, _)| *known == action) {
            Some((_, count)) => *count += 1,
            None => actions.push((action, 1)),
        }
    }
    let mut named: Vec<_> = actions.iter().enumerate().collect();
    named.sort_by_key(|(index, (action, _))| (action.priority(), *index));
    named.truncate(2);
    named.sort_by_key(|(index, _)| *index);
    let rest = tools.len() - named.iter().map(|(_, (_, count))| count).sum::<usize>();
    let mut labels: Vec<_> = named
        .iter()
        .map(|(_, (action, count))| counted(language, action.keys(), *count))
        .collect();
    if !sources.is_empty() {
        let names = sources.iter().map(|name| (*name).to_owned()).collect();
        labels.insert(
            0,
            localized_ui_format(
                language,
                if sources.len() == 1 {
                    "work.group.integration"
                } else {
                    "work.group.integrations"
                }
                .into(),
                vec![listed(language, names)],
            ),
        );
    }
    if rest > 0 {
        labels.push(counted(
            language,
            ("work.group.other", "work.group.others"),
            rest,
        ));
    }
    sentence(language, labels)
}

/// What a group's tools are, for its icon, as T3 Code's summary kind: their
/// one kind, reasoning when only that, otherwise mixed.
fn group_kind(items: &[&Arc<RenderedItem>]) -> String {
    let mut tools = items
        .iter()
        .map(|item| item.data.kind.as_str())
        .filter(|kind| *kind != "reasoning");
    match tools.next() {
        None => "reasoning".into(),
        Some(first) if tools.all(|kind| kind == first) => first.into(),
        Some(_) => "mixed".into(),
    }
}

/// Labels as one sentence: T3 Code's "A and b", "A, b, and c".
fn sentence(language: UiLanguage, labels: Vec<String>) -> String {
    if language == UiLanguage::Japanese {
        return labels.join("、");
    }
    let labels: Vec<_> = labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            if index == 0 {
                return label;
            }
            let mut chars = label.chars();
            chars
                .next()
                .map(|first| first.to_lowercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect();
    listed(language, labels)
}

/// Items as one list: T3 Code's "a and b", "a, b, and c".
fn listed(language: UiLanguage, items: Vec<String>) -> String {
    if language == UiLanguage::Japanese {
        return items.join("、");
    }
    match items.as_slice() {
        [first, second] => format!("{first} and {second}"),
        [init @ .., last] if !init.is_empty() => format!("{}, and {last}", init.join(", ")),
        _ => items.concat(),
    }
}

/// The MCP server whose tool `item` called, which T3 Code names as its
/// integration.
fn integration(item: &RenderedItem) -> Option<&str> {
    let ItemSource::Native(native) = &item.source else {
        return None;
    };
    match native.body() {
        ItemBody::ToolCall {
            kind: crate::models::ToolKind::Mcp,
            server: Some(server),
            ..
        } if !server.is_empty() => Some(server),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
