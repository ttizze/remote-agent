//! Thread lineage: fork parents and children, delegated agents and context
//! transfers between threads, and the thread details panel that lists them.
use super::agents::{
    self, MetadataInput, StatusTone, ThreadWorkspace, format_elapsed,
    format_subagent_display_title, prompt_title, status_tone, subagent_detail_preview,
    subagent_elapsed_ms,
};
use crate::commands::outbox::Request;
use crate::commands::workflows::latest_merge_back_run;
use crate::state::Snapshot;
use agent_domain::{
    Command, ContextDeliveryStatus, Driver, ItemStatus, RunStatus, State, Task, ThreadId,
    ThreadShell, Transfer,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RelationshipKind {
    Fork,
    Subagent,
    Transfer,
}

/// A related thread's run status, a delegated task's status, or a context
/// transfer's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RelationshipStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
    Pending,
    ResolvedPortable,
    Consumed,
    Superseded,
}
impl From<RunStatus> for RelationshipStatus {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Preparing => Self::Preparing,
            RunStatus::Queued => Self::Queued,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Completed => Self::Completed,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::RolledBack => Self::RolledBack,
        }
    }
}
impl From<ItemStatus> for RelationshipStatus {
    fn from(status: ItemStatus) -> Self {
        match status {
            ItemStatus::Pending => Self::Pending,
            ItemStatus::Running => Self::Running,
            ItemStatus::Waiting => Self::Waiting,
            ItemStatus::Completed => Self::Completed,
            ItemStatus::Interrupted => Self::Interrupted,
            ItemStatus::Failed => Self::Failed,
            ItemStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Not yet delivered is pending; delivered by the target's turn is consumed.
fn transfer_status(transfer: &Transfer) -> RelationshipStatus {
    if transfer.superseded {
        return RelationshipStatus::Superseded;
    }
    match transfer.delivery.as_ref().map(|delivery| delivery.status) {
        None => RelationshipStatus::Pending,
        Some(ContextDeliveryStatus::Pending) => RelationshipStatus::ResolvedPortable,
        Some(
            ContextDeliveryStatus::NativeFork
            | ContextDeliveryStatus::Injected
            | ContextDeliveryStatus::Inline,
        ) => RelationshipStatus::Consumed,
    }
}

pub fn relationship_status_label(status: Option<RelationshipStatus>) -> String {
    use RelationshipStatus::*;
    match status {
        Some(Preparing | Starting) => "Starting",
        Some(Running) => "Running",
        Some(Pending | Queued) => "Queued",
        Some(Waiting) => "Waiting",
        Some(Completed) => "Done",
        Some(Failed) => "Failed",
        Some(Cancelled | Interrupted) => "Stopped",
        Some(RolledBack) => "Reverted",
        Some(ResolvedPortable) => "Resolved (portable)",
        Some(Consumed) => "Consumed",
        Some(Superseded) => "Superseded",
        None => "Unknown",
    }
    .into()
}

/// The status dot on a relationship icon.
pub fn relationship_status_tone(status: Option<RelationshipStatus>) -> StatusTone {
    use RelationshipStatus::*;
    match status {
        Some(Running | Pending | Waiting) => StatusTone::Working,
        Some(Failed) => StatusTone::Failed,
        Some(Completed) => StatusTone::Completed,
        _ => StatusTone::Inactive,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipEdge {
    pub source: ThreadId,
    pub target: ThreadId,
    pub kind: RelationshipKind,
    pub status: Option<RelationshipStatus>,
}

/// Related threads by id; a thread without a known row is missing.
#[derive(Debug, Clone, Default)]
pub struct RelationshipGraph<'a> {
    pub nodes: BTreeMap<ThreadId, Option<&'a ThreadShell>>,
    pub edges: Vec<RelationshipEdge>,
}
impl<'a> RelationshipGraph<'a> {
    pub fn thread(&self, id: &ThreadId) -> Option<&'a ThreadShell> {
        self.nodes.get(id).copied().flatten()
    }
    pub fn missing(&self, id: &ThreadId) -> bool {
        matches!(self.nodes.get(id), Some(None))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipRow {
    pub thread: ThreadId,
    pub from: ThreadId,
    pub depth: u32,
    pub edge: RelationshipEdge,
}

/// A fork's merge-back target: the thread it was forked from.
pub fn merge_back_target(state: &State) -> Option<&ThreadId> {
    let thread = state.thread.as_ref()?;
    thread.fork_boundary.and(thread.parent.as_ref())
}

fn row_status(shell: &ThreadShell) -> Option<RelationshipStatus> {
    shell.activity_run_status.or(shell.status).map(Into::into)
}

/// Rows are ordered from most to least authoritative (live rows before the
/// archive, which may hold a stale copy); the first row of an id wins.
pub fn relationship_graph<'a>(
    rows: impl IntoIterator<Item = &'a ThreadShell>,
    projection: Option<&State>,
) -> RelationshipGraph<'a> {
    let mut threads: Vec<&ThreadShell> = vec![];
    let mut graph = RelationshipGraph::default();
    for row in rows {
        if !graph.nodes.contains_key(&row.id) {
            graph.nodes.insert(row.id.clone(), Some(row));
            threads.push(row);
        }
    }
    let mut keys: BTreeMap<(ThreadId, ThreadId, RelationshipKind), usize> = BTreeMap::new();
    let mut add = |graph: &mut RelationshipGraph<'a>, edge: RelationshipEdge| {
        for id in [&edge.source, &edge.target] {
            graph.nodes.entry(id.clone()).or_insert(None);
        }
        let key = (edge.source.clone(), edge.target.clone(), edge.kind);
        match keys.get(&key) {
            Some(index) => graph.edges[*index] = edge,
            None => {
                keys.insert(key, graph.edges.len());
                graph.edges.push(edge);
            }
        }
    };
    for thread in &threads {
        let Some(parent) = &thread.parent else {
            continue;
        };
        add(
            &mut graph,
            RelationshipEdge {
                source: parent.clone(),
                target: thread.id.clone(),
                kind: if thread.fork_boundary.is_some() {
                    RelationshipKind::Fork
                } else {
                    RelationshipKind::Subagent
                },
                status: row_status(thread),
            },
        );
    }
    if let Some(state) = projection
        && let Some(owner) = &state.thread
    {
        for task in &state.tasks {
            // A delegated task settles with its first run, but the parent can
            // keep sending the child follow-ups: a live run on the child
            // outranks the settled status.
            let live = graph
                .thread(&task.child_thread)
                .and_then(|child| child.activity_run_status);
            add(
                &mut graph,
                RelationshipEdge {
                    source: owner.id.clone(),
                    target: task.child_thread.clone(),
                    kind: RelationshipKind::Subagent,
                    status: Some(live.map_or(task.status.into(), Into::into)),
                },
            );
        }
        for transfer in &state.transfers {
            if transfer.source == transfer.target {
                continue;
            }
            add(
                &mut graph,
                RelationshipEdge {
                    source: transfer.source.clone(),
                    target: transfer.target.clone(),
                    kind: RelationshipKind::Transfer,
                    status: Some(transfer_status(transfer)),
                },
            );
        }
    }
    graph
}

fn other_end<'e>(edge: &'e RelationshipEdge, thread: &ThreadId) -> Option<&'e ThreadId> {
    if &edge.source == thread {
        Some(&edge.target)
    } else if &edge.target == thread {
        Some(&edge.source)
    } else {
        None
    }
}

pub fn related_thread_ids(graph: &RelationshipGraph, thread: &ThreadId) -> Vec<ThreadId> {
    let mut ids: Vec<ThreadId> = vec![];
    for edge in &graph.edges {
        for (end, other) in [(&edge.source, &edge.target), (&edge.target, &edge.source)] {
            if end == thread && !ids.contains(other) {
                ids.push(other.clone());
            }
        }
    }
    ids
}

/// Every thread reachable from `thread`, breadth first, each once.
pub fn walk_thread_relationships(
    graph: &RelationshipGraph,
    thread: &ThreadId,
) -> Vec<RelationshipRow> {
    let mut visited = vec![thread.clone()];
    let mut pending = vec![(thread.clone(), 0)];
    let mut rows = vec![];
    let mut index = 0;
    while let Some((current, depth)) = pending.get(index).cloned() {
        index += 1;
        for edge in &graph.edges {
            let Some(related) = other_end(edge, &current) else {
                continue;
            };
            if visited.contains(related) {
                continue;
            }
            visited.push(related.clone());
            rows.push(RelationshipRow {
                thread: related.clone(),
                from: current.clone(),
                depth: depth + 1,
                edge: edge.clone(),
            });
            pending.push((related.clone(), depth + 1));
        }
    }
    rows
}

/// The threads one edge away, each through its first edge.
pub fn immediate_thread_relationships(
    graph: &RelationshipGraph,
    thread: &ThreadId,
) -> Vec<RelationshipRow> {
    let mut rows: Vec<RelationshipRow> = vec![];
    for edge in &graph.edges {
        let Some(related) = other_end(edge, thread) else {
            continue;
        };
        if rows.iter().any(|row| &row.thread == related) {
            continue;
        }
        rows.push(RelationshipRow {
            thread: related.clone(),
            from: thread.clone(),
            depth: 1,
            edge: edge.clone(),
        });
    }
    rows
}

/// The edge reaches `current` from its parent thread or owning agent.
pub fn is_parent_relationship(edge: &RelationshipEdge, current: &ThreadId) -> bool {
    edge.kind != RelationshipKind::Transfer && &edge.target == current
}

/// A parent row shows the parent's own activity, not the child's edge status.
pub fn relationship_row_status(
    graph: &RelationshipGraph,
    row: &RelationshipRow,
) -> Option<RelationshipStatus> {
    if row.edge.kind == RelationshipKind::Transfer || row.thread == row.edge.target {
        return row.edge.status;
    }
    graph.thread(&row.thread).and_then(row_status)
}

/// The lineage panel's order: the parent first, a distinct merge-back target
/// second, then newest created first (immutable, so rows never move as
/// related threads change). Missing threads sink; ties break by id.
pub fn order_lineage_rows(
    graph: &RelationshipGraph,
    mut rows: Vec<RelationshipRow>,
    current: &ThreadId,
    merge_target: Option<&ThreadId>,
) -> Vec<RelationshipRow> {
    let pin = |row: &RelationshipRow| {
        if is_parent_relationship(&row.edge, current) {
            0
        } else if Some(&row.thread) == merge_target {
            1
        } else {
            2
        }
    };
    let created = |row: &RelationshipRow| graph.thread(&row.thread).map(|t| t.created_at.millis());
    rows.sort_by(|left, right| {
        pin(left)
            .cmp(&pin(right))
            .then_with(|| match (created(left), created(right)) {
                (Some(left), Some(right)) => right.cmp(&left),
                (left, right) => left.is_none().cmp(&right.is_none()),
            })
            .then_with(|| left.thread.cmp(&right.thread))
    });
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LineageGroupKind {
    Related,
    Active,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LineageIcon {
    Parent,
    Agent,
    Fork,
}

/// The agent behind a subagent row, for its elapsed time and hover card.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LineageAgent {
    pub model_label: String,
    pub status_label: String,
    pub tone: StatusTone,
    pub elapsed: Option<String>,
    pub workspace: Vec<agents::AgentWorkspaceEntry>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MergeBackControl {
    pub enabled: bool,
    pub busy: bool,
    pub label: String,
    pub tooltip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LineageRow {
    pub thread_id: String,
    pub title: String,
    pub relationship_label: String,
    /// The tooltip of a row without an agent.
    pub hint: String,
    pub status: Option<RelationshipStatus>,
    pub status_label: String,
    pub status_tone: StatusTone,
    pub icon: LineageIcon,
    /// The provider glyph that replaces the icon of a subagent row.
    pub driver: Option<Driver>,
    /// The thread is unavailable and cannot be opened.
    pub missing: bool,
    pub agent: Option<LineageAgent>,
    /// Present on the merge-back target's row.
    pub merge_back: Option<MergeBackControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LineageGroup {
    pub kind: LineageGroupKind,
    pub label: Option<String>,
    pub expanded_by_default: bool,
    /// `N failed` beside the header.
    pub failed_label: Option<String>,
    pub rows: Vec<LineageRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LineagePanel {
    pub title: String,
    pub running_count: u32,
    /// Only groups with rows.
    pub groups: Vec<LineageGroup>,
    /// Offers "Disconnect agent session".
    pub can_detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LineageWindow {
    pub visible_count: u32,
    pub hidden_count: u32,
    pub show_more_label: Option<String>,
    /// The visible count after "Show more".
    pub next_visible_count: u32,
}

const LINEAGE_INITIAL_COUNT: u32 = 6;
const LINEAGE_PAGE_COUNT: u32 = 12;

/// Rows a group shows: six at first, then twelve more per "Show more".
pub fn lineage_window(total: u32, visible_count: Option<u32>) -> LineageWindow {
    let visible = visible_count.unwrap_or(LINEAGE_INITIAL_COUNT).min(total);
    let hidden = total - visible;
    LineageWindow {
        visible_count: visible,
        hidden_count: hidden,
        show_more_label: (hidden > 0)
            .then(|| format!("Show {} more", hidden.min(LINEAGE_PAGE_COUNT))),
        next_visible_count: visible_count.unwrap_or(LINEAGE_INITIAL_COUNT) + LINEAGE_PAGE_COUNT,
    }
}

/// A collapsed group's header names its row count.
pub fn lineage_group_title(label: &str, count: u32, expanded: bool) -> String {
    if expanded {
        label.into()
    } else {
        format!("{label} ({count})")
    }
}

fn relationship_label(edge: &RelationshipEdge, current: &ThreadId) -> &'static str {
    let outgoing = &edge.source == current;
    match edge.kind {
        RelationshipKind::Transfer => "Context transfer",
        RelationshipKind::Subagent if outgoing => "Subagent",
        RelationshipKind::Subagent => "Parent agent",
        RelationshipKind::Fork if outgoing => "Fork",
        RelationshipKind::Fork => "Parent thread",
    }
}

fn status_word(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Pending => "Pending",
        ItemStatus::Running => "Running",
        ItemStatus::Waiting => "Waiting",
        ItemStatus::Completed => "Completed",
        ItemStatus::Interrupted => "Interrupted",
        ItemStatus::Failed => "Failed",
        ItemStatus::Cancelled => "Cancelled",
    }
}

/// The agent as the row presents it: while the child thread runs a follow-up,
/// the row follows that run instead of the settled task.
struct RowAgent<'a> {
    task: &'a Task,
    status: ItemStatus,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    following_up: bool,
}

fn row_agent<'a>(task: &'a Task, child: Option<&ThreadShell>) -> RowAgent<'a> {
    match child.and_then(|child| child.activity_run_status.map(|s| (child, s))) {
        Some((child, live)) => RowAgent {
            task,
            status: match live {
                RunStatus::Running => ItemStatus::Running,
                RunStatus::Waiting => ItemStatus::Waiting,
                _ => ItemStatus::Pending,
            },
            started_at_ms: child.activity_run_started_at.as_ref().map(|at| at.millis()),
            completed_at_ms: None,
            following_up: true,
        },
        None => RowAgent {
            task,
            status: task.status,
            started_at_ms: Some(task.started_at.millis()),
            completed_at_ms: task.completed_at.as_ref().map(|at| at.millis()),
            following_up: false,
        },
    }
}

/// An agent's metadata from its task, its thread's row and the current
/// thread's workspace.
type AgentMetadata<'a> = dyn Fn(&Task, Option<&ThreadShell>, Option<ThreadWorkspace<'a>>) -> agents::SubagentMetadata
    + 'a;

/// Inputs of the lineage panel besides the rows of known threads.
struct PanelContext<'a> {
    current: &'a ThreadId,
    projection: Option<&'a State>,
    now_ms: i64,
    merge_in_flight: bool,
    agent: &'a AgentMetadata<'a>,
}

fn panel<'a>(
    rows: impl IntoIterator<Item = &'a ThreadShell>,
    context: PanelContext<'a>,
) -> Option<LineagePanel> {
    let current = context.current;
    let graph = relationship_graph(rows, context.projection);
    let merge_target = context.projection.and_then(merge_back_target);
    let rows = order_lineage_rows(
        &graph,
        immediate_thread_relationships(&graph, current),
        current,
        merge_target,
    );
    let merge_run = context.projection.and_then(latest_merge_back_run);
    let parent_title = merge_target
        .and_then(|target| graph.thread(target))
        .map(|thread| thread.title.clone());
    let current_workspace = context
        .projection
        .and_then(|state| state.thread.as_ref())
        .map(ThreadWorkspace::of_thread)
        .or_else(|| graph.thread(current).map(ThreadWorkspace::of_shell));
    let tasks_by_child: BTreeMap<&ThreadId, &Task> = context
        .projection
        .map(|state| {
            state
                .tasks
                .iter()
                .map(|task| (&task.child_thread, task))
                .collect()
        })
        .unwrap_or_default();

    let mut groups: [(LineageGroupKind, Vec<LineageRow>, u32); 3] = [
        (LineageGroupKind::Related, vec![], 0),
        (LineageGroupKind::Active, vec![], 0),
        (LineageGroupKind::Previous, vec![], 0),
    ];
    let mut running = 0;
    for row in &rows {
        let edge = &row.edge;
        let parent = is_parent_relationship(edge, current);
        let subagent = edge.kind == RelationshipKind::Subagent;
        let group = if !subagent || parent {
            0
        } else if matches!(
            edge.status,
            Some(
                RelationshipStatus::Completed
                    | RelationshipStatus::Failed
                    | RelationshipStatus::Cancelled
                    | RelationshipStatus::Interrupted
            )
        ) {
            2
        } else {
            1
        };
        if group == 1 && edge.status == Some(RelationshipStatus::Running) {
            running += 1;
        }
        if edge.status == Some(RelationshipStatus::Failed) {
            groups[group].2 += 1;
        }
        let thread = graph.thread(&row.thread);
        let status = relationship_row_status(&graph, row);
        let relationship = relationship_label(edge, current);
        let agent = (subagent && !parent)
            .then(|| tasks_by_child.get(&row.thread))
            .flatten()
            .map(|task| row_agent(task, thread));
        let title = thread
            .map(|thread| thread.title.clone())
            .or_else(|| {
                agent.as_ref().map(|agent| {
                    agent
                        .task
                        .title
                        .clone()
                        .unwrap_or_else(|| prompt_title(&agent.task.prompt))
                })
            })
            .unwrap_or_else(|| row.thread.to_string());
        let title = if subagent {
            format_subagent_display_title(&title)
        } else {
            title
        };
        let missing = graph.missing(&row.thread);
        let driver = (subagent && !parent)
            .then(|| {
                agent
                    .as_ref()
                    .and_then(|agent| {
                        context
                            .projection
                            .and_then(|state| agents::task_selection(state, agent.task, thread))
                    })
                    .or_else(|| thread.map(|thread| &thread.selection))
                    .map(|selection| selection.driver)
            })
            .flatten();
        let agent = agent.map(|agent| {
            let metadata = (context.agent)(agent.task, thread, current_workspace);
            let (result, progress) = if agent.following_up {
                (None, None)
            } else {
                (agent.task.result.as_deref(), agent.task.progress.as_deref())
            };
            LineageAgent {
                model_label: metadata.model_label,
                status_label: status_word(agent.status).into(),
                tone: status_tone(agent.status),
                elapsed: subagent_elapsed_ms(
                    agent.status,
                    agent.started_at_ms,
                    agent.completed_at_ms,
                    context.now_ms,
                )
                .map(format_elapsed),
                workspace: metadata.workspace,
                preview: subagent_detail_preview(agent.status, result, progress),
            }
        });
        let merge_back = (Some(&row.thread) == merge_target).then(|| MergeBackControl {
            enabled: merge_run.is_some() && !context.merge_in_flight,
            busy: context.merge_in_flight,
            label: parent_title.as_ref().map_or_else(
                || "Merge back to source conversation".into(),
                |title| format!("Merge back to {title}"),
            ),
            tooltip: match (&parent_title, merge_run) {
                (_, None) => "Complete a run in this fork before merging it back".into(),
                (Some(title), Some(_)) => format!("Merge this conversation back into {title}"),
                (None, Some(_)) => "Merge this conversation back into its source".into(),
            },
        });
        groups[group].1.push(LineageRow {
            thread_id: row.thread.to_string(),
            title,
            relationship_label: relationship.into(),
            hint: if missing {
                "This related thread is unavailable".into()
            } else {
                format!("Open {} in this chat", relationship.to_lowercase())
            },
            status,
            status_label: relationship_status_label(status),
            status_tone: relationship_status_tone(status),
            icon: if parent {
                LineageIcon::Parent
            } else if subagent {
                LineageIcon::Agent
            } else {
                LineageIcon::Fork
            },
            driver,
            missing,
            agent,
            merge_back,
        });
    }
    if rows.is_empty() && running == 0 {
        return None;
    }
    Some(LineagePanel {
        title: if running > 0 {
            format!("Lineage · {running} running")
        } else {
            "Lineage".into()
        },
        running_count: running,
        groups: groups
            .into_iter()
            .filter(|(_, rows, _)| !rows.is_empty())
            .map(|(kind, rows, failed)| LineageGroup {
                kind,
                label: (kind == LineageGroupKind::Previous).then(|| "Previous agents".into()),
                expanded_by_default: kind != LineageGroupKind::Previous,
                failed_label: (failed > 0).then(|| format!("{failed} failed")),
                rows,
            })
            .collect(),
        can_detach: context
            .projection
            .is_some_and(|state| !state.native_sessions.is_empty()),
    })
}

/// The thread details panel's Lineage section; `None` when the thread has no
/// related threads. Agent rows' elapsed time counts to `now_ms`.
pub fn lineage_panel(snapshot: &Snapshot, thread: &ThreadId, now_ms: i64) -> Option<LineagePanel> {
    let live = snapshot.shell_view();
    let archived = snapshot
        .archived
        .as_ref()
        .and_then(|cache| cache.snapshot.as_ref());
    let rows = live
        .as_deref()
        .into_iter()
        .chain(archived)
        .flat_map(|shell| &shell.threads);
    let projection = snapshot.thread_state(thread);
    let merge_in_flight = snapshot.outbox.entries.iter().any(|entry| {
        &entry.thread == thread
            && matches!(&entry.request, Request::Dispatch(dispatch)
                if matches!(dispatch.command, Command::MergeBack { .. }))
    });
    let agent = |task: &Task, child: Option<&ThreadShell>, parent: Option<ThreadWorkspace>| {
        let selection = projection.and_then(|state| agents::task_selection(state, task, child));
        let catalog = selection.map(|s| agents::catalog(&snapshot.models, s.driver));
        agents::subagent_metadata(MetadataInput {
            model: task.model.as_deref(),
            provider: selection
                .zip(catalog.as_deref())
                .map(|(s, c)| (s.driver, c)),
            parent_thread: parent,
            child_thread: child.map(ThreadWorkspace::of_shell),
            parent_project: parent.and_then(|p| agents::project(snapshot, p.project)),
            child_project: child.and_then(|c| agents::project(snapshot, &c.project)),
        })
    };
    panel(
        rows,
        PanelContext {
            current: thread,
            projection,
            now_ms,
            merge_in_flight,
            agent: &agent,
        },
    )
}

/// Merge back is offered once the fork has a finished run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MergeBackAction {
    pub target_thread_id: String,
    pub run_id: String,
}

pub fn merge_back_action(state: &State) -> Option<MergeBackAction> {
    Some(MergeBackAction {
        target_thread_id: merge_back_target(state)?.to_string(),
        run_id: latest_merge_back_run(state)?.id.to_string(),
    })
}

#[cfg(test)]
mod tests;
