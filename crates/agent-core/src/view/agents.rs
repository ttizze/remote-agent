//! Delegated agents: the current turn's roster and its composer pill, agent
//! rows, and the summaries that timeline groups and lineage tooltips show.
use crate::js_text::{JS_SPACE, collapse_js_spaces, is_js_space};
use crate::models::{Model, Project};
use crate::provider::ProviderKind;
use crate::state::Snapshot;
use crate::view::time::format_duration;
use agent_domain::{
    Driver, ItemStatus, ModelSelection, RunId, State, Task, Thread, ThreadId, ThreadShell,
};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum StatusTone {
    Working,
    Completed,
    Failed,
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentPill {
    pub label: String,
    pub accessibility_label: String,
}

/// A workspace detail that differs from the parent's (`Project`, `Branch`,
/// `Worktree` or `Workspace`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentWorkspaceEntry {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentMetadata {
    pub model_label: String,
    pub workspace: Vec<AgentWorkspaceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentRow {
    pub id: String,
    pub child_thread_id: String,
    pub title: String,
    /// Live agents lead with progress; settled ones lead with their result.
    pub detail: Option<String>,
    pub status_label: String,
    pub tone: StatusTone,
    pub live: bool,
    pub elapsed: Option<String>,
    pub driver: Option<Driver>,
    pub instance_id: Option<String>,
    pub model_label: String,
    pub workspace: Vec<AgentWorkspaceEntry>,
}

/// The agents of one turn: the active run, else the run of the most recently
/// updated agent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentRoster {
    pub run_id: Option<String>,
    pub turn_active: bool,
    pub rows: Vec<AgentRow>,
    pub live_count: u32,
    pub settled_count: u32,
    /// The composer pill's agents segment; absent once the turn settled.
    pub pill: Option<AgentPill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentGroupSummary {
    pub label: String,
    pub active: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AgentSpawnSummary {
    pub live: bool,
    pub lead: String,
    pub status: String,
    pub tone: StatusTone,
}

/// A model of a provider's catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub slug: String,
    pub name: String,
}

/// The workspace fields of a thread that agent metadata compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadWorkspace<'a> {
    pub project: &'a str,
    pub worktree_path: Option<&'a str>,
    pub branch: Option<&'a str>,
}
impl<'a> ThreadWorkspace<'a> {
    pub fn of_shell(shell: &'a ThreadShell) -> Self {
        let workspace = shell.workspace.as_ref();
        Self {
            project: &shell.project,
            worktree_path: workspace.and_then(|w| w.worktree_path.as_deref()),
            branch: workspace.and_then(|w| w.branch.as_deref()),
        }
    }
    pub fn of_thread(thread: &'a Thread) -> Self {
        let workspace = thread.workspace.as_ref();
        Self {
            project: &thread.project,
            worktree_path: workspace.and_then(|w| w.worktree_path.as_deref()),
            branch: workspace.and_then(|w| w.branch.as_deref()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectWorkspace<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub workspace_root: Option<&'a str>,
}
impl<'a> ProjectWorkspace<'a> {
    pub fn of(project: &'a Project) -> Self {
        Self {
            id: &project.id,
            title: &project.name,
            workspace_root: project.roots.first().map(|root| root.path.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataInput<'a> {
    pub model: Option<&'a str>,
    pub provider: Option<(Driver, &'a [CatalogModel])>,
    pub parent_thread: Option<ThreadWorkspace<'a>>,
    pub child_thread: Option<ThreadWorkspace<'a>>,
    pub parent_project: Option<ProjectWorkspace<'a>>,
    pub child_project: Option<ProjectWorkspace<'a>>,
}

/// Pending, running and waiting work; waiting needs the user.
pub fn active_status(status: ItemStatus) -> bool {
    matches!(
        status,
        ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting
    )
}

fn task_updated_at(task: &Task) -> &agent_domain::Timestamp {
    task.completed_at.as_ref().unwrap_or(&task.started_at)
}

struct TurnTasks<'a> {
    run: Option<&'a RunId>,
    turn_active: bool,
    tasks: Vec<&'a Task>,
    live: u32,
    settled: u32,
}

fn turn_tasks(state: &State) -> Option<TurnTasks<'_>> {
    let first = state.tasks.first()?;
    let active = state.active_run();
    // With no live run the newest roster still shows: a turn that just
    // finished leaves results the user has not read yet.
    let latest = state.tasks.iter().fold(first, |latest, task| {
        if task_updated_at(task) > task_updated_at(latest) {
            task
        } else {
            latest
        }
    });
    let run = active.map(|run| &run.id).or(latest.run.as_ref());
    let mut tasks: Vec<_> = state
        .tasks
        .iter()
        .filter(|task| task.run.as_ref() == run)
        .collect();
    if tasks.is_empty() {
        return None;
    }
    tasks.sort_by(|a, b| {
        a.started_at
            .millis()
            .cmp(&b.started_at.millis())
            .then_with(|| a.id.cmp(&b.id))
    });
    let live = tasks
        .iter()
        .filter(|task| active_status(task.status))
        .count() as u32;
    let settled = tasks.iter().filter(|task| task.status.terminal()).count() as u32;
    Some(TurnTasks {
        run,
        turn_active: active.is_some(),
        tasks,
        live,
        settled,
    })
}

fn pill(turn: &TurnTasks) -> Option<AgentPill> {
    if !turn.turn_active && turn.live == 0 {
        return None;
    }
    let total = turn.tasks.len();
    Some(if turn.live > 0 {
        AgentPill {
            label: format!("{}/{total}", turn.live),
            accessibility_label: format!("{} of {total} agents working", turn.live),
        }
    } else {
        AgentPill {
            label: format!("{total} done"),
            accessibility_label: format!(
                "{total} {} done",
                if total == 1 { "agent" } else { "agents" }
            ),
        }
    })
}

/// Row details that come from outside the thread: the agent's provider and
/// the workspace differences against its parent.
#[derive(Debug, Clone, Default)]
struct RowContext {
    metadata: SubagentMetadata,
    driver: Option<Driver>,
    instance_id: Option<String>,
}

fn row_title(task: &Task) -> String {
    if let Some(title) = task.title.as_deref().map(str::trim)
        && !title.is_empty()
    {
        return format_subagent_display_title(title);
    }
    let prompt = task.prompt.trim();
    if prompt.is_empty() {
        "Subagent".into()
    } else {
        prompt_title(prompt)
    }
}

/// A prompt as a title: over 80 characters it keeps 77 and an ellipsis.
pub(crate) fn prompt_title(prompt: &str) -> String {
    if prompt.chars().count() > 80 {
        format!("{}...", prompt.chars().take(77).collect::<String>())
    } else {
        prompt.into()
    }
}

pub(crate) fn status_tone(status: ItemStatus) -> StatusTone {
    match status {
        ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting => StatusTone::Working,
        ItemStatus::Completed => StatusTone::Completed,
        ItemStatus::Failed => StatusTone::Failed,
        ItemStatus::Cancelled | ItemStatus::Interrupted => StatusTone::Inactive,
    }
}

fn row_status_label(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Pending | ItemStatus::Running => "Working",
        ItemStatus::Waiting => "Waiting",
        ItemStatus::Completed => "Completed",
        ItemStatus::Failed => "Failed",
        ItemStatus::Cancelled => "Cancelled",
        ItemStatus::Interrupted => "Interrupted",
    }
}

fn agent_row(task: &Task, now_ms: i64, context: RowContext) -> AgentRow {
    let elapsed = subagent_elapsed_ms(
        task.status,
        Some(task.started_at.millis()),
        task.completed_at.as_ref().map(|at| at.millis()),
        now_ms,
    )
    .filter(|ms| *ms != 0)
    .map(format_duration);
    AgentRow {
        id: task.id.to_string(),
        child_thread_id: task.child_thread.to_string(),
        title: row_title(task),
        detail: subagent_card_detail(
            subagent_detail_preview(
                task.status,
                task.result.as_deref(),
                task.progress.as_deref(),
            )
            .as_deref(),
        ),
        status_label: row_status_label(task.status).into(),
        tone: status_tone(task.status),
        live: active_status(task.status),
        elapsed,
        driver: context.driver,
        instance_id: context.instance_id,
        model_label: context.metadata.model_label,
        workspace: context.metadata.workspace,
    }
}

fn roster(
    state: &State,
    now_ms: i64,
    context: impl Fn(&Task) -> RowContext,
) -> Option<AgentRoster> {
    let turn = turn_tasks(state)?;
    Some(AgentRoster {
        run_id: turn.run.map(ToString::to_string),
        turn_active: turn.turn_active,
        rows: turn
            .tasks
            .iter()
            .map(|task| agent_row(task, now_ms, context(task)))
            .collect(),
        live_count: turn.live,
        settled_count: turn.settled,
        pill: pill(&turn),
    })
}

/// The thread's agent roster; `None` when the thread never spawned an agent.
/// Live rows' elapsed time counts to `now_ms`.
pub fn agent_roster(snapshot: &Snapshot, thread: &ThreadId, now_ms: i64) -> Option<AgentRoster> {
    let state = snapshot.thread_state(thread)?;
    let parent = state.thread.as_ref();
    roster(state, now_ms, |task| {
        let child = snapshot.thread_row(&task.child_thread);
        let selection = task_selection(state, task, child);
        let catalog = selection.map(|selection| catalog(&snapshot.models, selection.driver));
        RowContext {
            metadata: subagent_metadata(MetadataInput {
                model: task.model.as_deref(),
                provider: selection
                    .zip(catalog.as_deref())
                    .map(|(s, c)| (s.driver, c)),
                parent_thread: parent.map(ThreadWorkspace::of_thread),
                child_thread: child.map(ThreadWorkspace::of_shell),
                parent_project: parent.and_then(|p| project(snapshot, &p.project)),
                child_project: child.and_then(|c| project(snapshot, &c.project)),
            }),
            driver: selection.map(|s| s.driver),
            instance_id: selection.map(|s| s.instance.clone()),
        }
    })
}

/// The provider an agent runs on: its thread's, else its turn's, else its
/// parent thread's.
pub(crate) fn task_selection<'a>(
    state: &'a State,
    task: &Task,
    child: Option<&'a ThreadShell>,
) -> Option<&'a ModelSelection> {
    child
        .map(|child| &child.selection)
        .or_else(|| {
            let run = task.run.as_ref()?;
            state
                .runs
                .iter()
                .find(|candidate| &candidate.id == run)
                .map(|run| &run.selection)
        })
        .or_else(|| state.thread.as_ref().map(|thread| &thread.selection))
}

pub(crate) fn project<'a>(snapshot: &'a Snapshot, id: &str) -> Option<ProjectWorkspace<'a>> {
    snapshot
        .shell_projects()
        .iter()
        .find(|project| project.id == id)
        .map(ProjectWorkspace::of)
}

pub(crate) fn catalog(models: &[Model], driver: Driver) -> Vec<CatalogModel> {
    models
        .iter()
        .filter(|model| {
            matches!(
                (model.model.provider, driver),
                (ProviderKind::Codex, Driver::Codex) | (ProviderKind::Claude, Driver::Claude)
            )
        })
        .map(|model| CatalogModel {
            slug: model.model.id.clone(),
            name: model.display_name.clone(),
        })
        .collect()
}

/// Elapsed time of the current activation: live work counts to `now_ms`,
/// settled work stops at its completion and is unknown without one.
pub fn subagent_elapsed_ms(
    status: ItemStatus,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    now_ms: i64,
) -> Option<i64> {
    let start = started_at_ms?;
    let end = if active_status(status) {
        now_ms
    } else {
        completed_at_ms?
    };
    Some((end - start).max(0))
}

/// Elapsed time as `5s`, `2m 05s` or `1h 02m`.
pub fn format_elapsed(elapsed_ms: i64) -> String {
    let seconds = (elapsed_ms / 1_000).max(0);
    let minutes = seconds / 60;
    if minutes == 0 {
        return format!("{seconds}s");
    }
    let hours = minutes / 60;
    if hours == 0 {
        return format!("{minutes}m {:02}s", seconds % 60);
    }
    format!("{hours}h {:02}m", minutes % 60)
}

/// Summarizes one adjacent group of agent items.
pub fn subagent_group_summary(statuses: &[ItemStatus]) -> SubagentGroupSummary {
    let active = statuses.iter().any(|status| active_status(*status));
    let count = statuses.len();
    SubagentGroupSummary {
        label: format!(
            "{} {count} {}",
            if active { "Kicked off" } else { "Ran" },
            if count == 1 { "subagent" } else { "subagents" }
        ),
        active,
        failed: statuses.contains(&ItemStatus::Failed),
    }
}

/// Counts a group's states in reading order: running work first, then
/// outcomes, using the agents panel's words.
pub fn summarize_subagent_statuses(statuses: &[ItemStatus]) -> String {
    let mut counts = [(0, "working"), (0, "done"), (0, "failed"), (0, "stopped")];
    for status in statuses {
        let index = match status {
            ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting => 0,
            ItemStatus::Completed => 1,
            ItemStatus::Failed => 2,
            ItemStatus::Cancelled | ItemStatus::Interrupted => 3,
        };
        counts[index].0 += 1;
    }
    counts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Summarizes the observed agents of a spawn without treating agents missing
/// from `statuses` (of `agent_count`) as completed.
pub fn agent_spawn_summary(statuses: &[ItemStatus], agent_count: u32) -> AgentSpawnSummary {
    let count = |matches: fn(&ItemStatus) -> bool| statuses.iter().filter(|s| matches(s)).count();
    let working = count(|s| active_status(*s));
    let failed = count(|s| *s == ItemStatus::Failed);
    let stopped = count(|s| matches!(s, ItemStatus::Cancelled | ItemStatus::Interrupted));
    let live = working > 0;
    let subjects = if agent_count > 0 {
        format!(
            "{agent_count} subagent{}",
            if agent_count == 1 { "" } else { "s" }
        )
    } else {
        "subagents".into()
    };
    let lead = format!("{} {subjects}", if live { "Kicked off" } else { "Ran" });
    let status = if live {
        format!("{working} working")
    } else if failed > 0 {
        format!("{failed} failed")
    } else if stopped > 0 {
        format!("{stopped} stopped")
    } else if statuses.is_empty() || statuses.len() < agent_count as usize {
        "Status unavailable".into()
    } else {
        "✓ completed".into()
    };
    let tone = if live {
        StatusTone::Working
    } else if failed > 0 {
        StatusTone::Failed
    } else if status == "✓ completed" {
        StatusTone::Completed
    } else {
        StatusTone::Inactive
    };
    AgentSpawnSummary {
        live,
        lead,
        status,
        tone,
    }
}

static SUBAGENT_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("(?i)^Subagent:{JS_SPACE}*")).unwrap());
static TASK_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/root/(?:[^/]+/)*([^/]+)/?$").unwrap());

/// Codex task paths (`/root/…/my_worker`) read as `My Worker`; a
/// `Subagent:` prefix is dropped.
pub fn format_subagent_display_title(title: &str) -> String {
    let display = SUBAGENT_PREFIX.replace(title, "");
    let Some(path) = TASK_PATH.captures(&display) else {
        return display.into_owned();
    };
    let words: Vec<String> = path[1]
        .split(|c| c == '_' || is_js_space(c))
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect();
    if words.is_empty() {
        display.into_owned()
    } else {
        words.join(" ")
    }
}

/// Live work leads with progress, settled work with its result; whitespace
/// collapses and previews stop at 280 characters.
pub fn subagent_detail_preview(
    status: ItemStatus,
    result: Option<&str>,
    progress: Option<&str>,
) -> Option<String> {
    let result = result.map(str::trim).filter(|s| !s.is_empty());
    let progress = progress.map(str::trim).filter(|s| !s.is_empty());
    let detail = if status.terminal() {
        result.or(progress)
    } else {
        progress.or(result)
    }?;
    let compact = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if compact.chars().count() > 280 {
        let head: String = compact.chars().take(280).collect();
        format!("{}…", head.trim_end())
    } else {
        compact
    })
}

static GENERIC_CHILD_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Child task ended with status(?-u:\b)").unwrap());
static MARKDOWN_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap());
static LIST_BULLET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mR)^[ \t]*[-*][ \t]+").unwrap());

/// A detail as one line of plain text for agent cards: no list bullets, code
/// ticks or link targets. The generic status fallback of a result says
/// nothing and is dropped.
pub fn subagent_card_detail(detail: Option<&str>) -> Option<String> {
    let detail = detail.filter(|detail| !GENERIC_CHILD_END.is_match(detail))?;
    let linked = MARKDOWN_LINK.replace_all(detail, "$1");
    let unbulleted = LIST_BULLET
        .replace_all(&linked.replace('`', ""), "")
        .into_owned();
    let text = collapse_js_spaces(&unbulleted);
    (!text.is_empty()).then_some(text)
}

/// The agent's model as its catalog names it, and the workspace details that
/// differ from its parent's.
pub fn subagent_metadata(input: MetadataInput) -> SubagentMetadata {
    let model = input.model.map(str::trim);
    let slug = match input.provider {
        Some((driver, models)) => {
            model.and_then(|model| resolve_selectable_model(driver, model, models))
        }
        None => model.map(str::to_owned),
    };
    let catalog_model = input
        .provider
        .and_then(|(_, models)| models.iter().find(|m| Some(&m.slug) == slug.as_ref()));
    let model_label = match (catalog_model, model) {
        (Some(catalog), _) => catalog.name.clone(),
        (None, Some(model)) if !model.is_empty() => format_model_slug_name(model),
        _ => "Not reported".into(),
    };
    let parent_workspace = input
        .parent_thread
        .and_then(|t| t.worktree_path)
        .or_else(|| input.parent_project.and_then(|p| p.workspace_root));
    let child_workspace = input
        .child_thread
        .and_then(|t| t.worktree_path)
        .or_else(|| input.child_project.and_then(|p| p.workspace_root));
    let mut workspace = vec![];
    if let (Some(parent), Some(child)) = (input.parent_thread, input.child_project)
        && child.id != parent.project
    {
        workspace.push(AgentWorkspaceEntry {
            label: "Project".into(),
            value: child.title.into(),
        });
    }
    if let (Some(parent), Some(child)) = (parent_workspace, child_workspace)
        && parent != child
    {
        let branch = input.child_thread.and_then(|t| t.branch);
        let worktree = input.child_thread.and_then(|t| t.worktree_path);
        workspace.push(AgentWorkspaceEntry {
            label: if branch.is_some() {
                "Branch"
            } else if worktree.is_some() {
                "Worktree"
            } else {
                "Workspace"
            }
            .into(),
            value: branch.map_or_else(|| file_basename(child), str::to_owned),
        });
    }
    SubagentMetadata {
        model_label,
        workspace,
    }
}

fn file_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path.into();
    }
    trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).into()
}

const CODEX_MODEL_ALIASES: [(&str, &str); 6] = [
    ("gpt-5-codex", "gpt-5.4"),
    ("5.4", "gpt-5.4"),
    ("5.3", "gpt-5.3-codex"),
    ("gpt-5.3", "gpt-5.3-codex"),
    ("5.3-spark", "gpt-5.3-codex-spark"),
    ("gpt-5.3-spark", "gpt-5.3-codex-spark"),
];

fn resolve_selectable_model(
    driver: Driver,
    value: &str,
    models: &[CatalogModel],
) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(model) = models.iter().find(|m| m.slug == value) {
        return Some(model.slug.clone());
    }
    let lower = value.to_lowercase();
    if let Some(model) = models.iter().find(|m| m.name.to_lowercase() == lower) {
        return Some(model.slug.clone());
    }
    let normalized = match driver {
        Driver::Codex => CODEX_MODEL_ALIASES
            .iter()
            .find(|(alias, _)| *alias == value)
            .map_or(value, |(_, slug)| *slug),
        Driver::Claude => value,
    };
    models
        .iter()
        .find(|m| m.slug == normalized)
        .map(|m| m.slug.clone())
}

/// `gpt-5.3-codex` reads `GPT-5.3-Codex` and `claude-opus-4-6` reads
/// `Claude Opus 4.6`; other identities stay as they are.
pub fn format_model_slug_name(slug: &str) -> String {
    let separator = slug.rfind('/').map_or(0, |index| index + 1);
    let (prefix, name) = slug.split_at(separator);
    let lower = name.to_ascii_lowercase();
    let numbered = |family: &str| {
        lower
            .strip_prefix(family)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    if numbered("gpt") {
        let mut out = String::from(prefix);
        out.push_str("GPT");
        let mut chars = name[3..].chars().peekable();
        while let Some(c) = chars.next() {
            out.push(c);
            if c == '-'
                && let Some(next) = chars.peek().copied()
                && next.is_ascii_lowercase()
            {
                out.push(next.to_ascii_uppercase());
                chars.next();
            }
        }
        return out;
    }
    let family = [
        "claude-opus",
        "claude-sonnet",
        "claude-haiku",
        "claude-fable",
        "gemini",
        "grok",
        "composer",
    ];
    if !family.iter().any(|family| numbered(family)) {
        return slug.into();
    }
    let name = claude_version_dot(name);
    let words: Vec<String> = name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect();
    format!("{prefix}{}", words.join(" "))
}

/// `claude-opus-4-6` becomes `claude-opus-4.6` when the minor version has one
/// or two digits and ends the name or precedes `-` or `[`.
fn claude_version_dot(name: &str) -> String {
    let bytes = name.as_bytes();
    if !name
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("claude-"))
    {
        return name.into();
    }
    let mut index = 7;
    let letters = bytes[index..]
        .iter()
        .take_while(|b| b.is_ascii_alphabetic())
        .count();
    if letters == 0 || bytes.get(index + letters) != Some(&b'-') {
        return name.into();
    }
    index += letters + 1;
    let major = bytes[index..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if major == 0 || bytes.get(index + major) != Some(&b'-') {
        return name.into();
    }
    let dash = index + major;
    let minor = bytes[dash + 1..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if !(1..=2).contains(&minor) || !matches!(bytes.get(dash + 1 + minor), None | Some(b'-' | b'['))
    {
        return name.into();
    }
    format!("{}.{}", &name[..dash], &name[dash + 1..])
}

#[cfg(test)]
mod tests;
