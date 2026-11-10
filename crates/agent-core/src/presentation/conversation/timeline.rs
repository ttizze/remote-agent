//! A turn as T3 Code's timeline shows it. Once the turn completes, its work
//! folds under "Worked for …"; while it runs, a working header leads it and
//! its latest work is the live row; a failed or interrupted turn stays open.
//! Open work reads as the commentary between it, groups of tools under their
//! summary ("Ran 2 commands") and a lone tool on its own row.
use super::{
    ActivityPresentation, ConversationRow, ConversationRowContent, ItemSource, LiveActivity,
    RenderedItem, RenderedTurn, activity_is_expanded,
};
use crate::localization::{UiLanguage, localized_ui_format};
use crate::models::{ItemBody, TurnStatus};
use std::{collections::HashSet, sync::Arc};

/// One row of a turn on T3 Code's timeline.
#[derive(Clone, PartialEq)]
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
        open: bool,
    },
    /// A tool's row, alone or a member of an open group.
    Work { row: ConversationRow, label: String },
    /// The running turn's latest work.
    Live { row: ConversationRow, label: String },
    /// A running turn with nothing live.
    Thinking,
    /// The turn waits for the user's answer to a request, which T3 Code's
    /// live row names: what an approval is for, or that input is requested.
    Requested { label: String, approval: bool },
    /// The provider stream dropped and is being retried, which the error's
    /// title says.
    Retrying { label: String },
    /// A plan the agent proposed, as T3 Code's plan card: its title (the
    /// plan's first heading, else Proposed plan) over its Markdown.
    Plan {
        id: String,
        title: String,
        text: String,
    },
    /// A subagent the turn started, as T3 Code's subagent row: its name and
    /// its latest word.
    Subagent {
        id: String,
        agent: crate::presentation::agents::SubagentPresentation,
    },
    /// A divider across the work, as T3 Code's context compaction.
    Divider { id: String, label: String },
    /// The actions (fork, copy, time) of the settled turn's last answer when
    /// work ends the turn below it: as T3 Code's, they follow that work and
    /// stay in view.
    Actions(ConversationRow),
    /// The turn was interrupted.
    Interrupted,
}

/// The turn's rows: folds open by `expanded` (a fold's ID and the status it
/// was opened at, as [`activity_is_expanded`]) and groups open by `groups`.
pub fn turn_rows(
    turn: &RenderedTurn,
    expanded: &std::collections::HashMap<String, String>,
    groups: &HashSet<String>,
) -> Vec<TurnRow> {
    let language = turn.language;
    let status = turn.source.status;
    let running = status == TurnStatus::Running;
    // As T3 Code, failed and interrupted work stays in view.
    let folds = status == TurnStatus::Completed;
    let live = turn.live_activity();
    let mut out = Vec::new();
    let mut work: Vec<&ConversationRow> = Vec::new();
    let mut hidden = false;
    let flush = |work: &mut Vec<&ConversationRow>, out: &mut Vec<TurnRow>, last: bool| {
        let rows = std::mem::take(work);
        let live = last
            .then_some(live.as_ref())
            .flatten()
            .and_then(|live| match live {
                LiveActivity::Row(id) => rows.iter().rev().find(|row| &row.id == id),
                LiveActivity::Thinking => None,
            });
        if let Some(row) = live {
            let label =
                activity_item(row).map_or_else(String::new, |item| live_label(language, item));
            out.push(TurnRow::Live {
                row: (*row).clone(),
                label,
            });
            return;
        }
        pieces(language, &rows, status == TurnStatus::Failed, groups, out);
    };
    for row in &turn.rows {
        match &row.content {
            ConversationRowContent::ActivityHeader { activity } => {
                flush(&mut work, &mut out, false);
                hidden = false;
                if folds && activity.activity_can_collapse {
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
                flush(&mut work, &mut out, false);
                if !hidden {
                    out.push(TurnRow::Commentary(item.clone()));
                }
            }
            ConversationRowContent::Activity { item, .. } => match place(language, item) {
                Place::Work if !hidden => work.push(row),
                Place::Alone(rows) => {
                    flush(&mut work, &mut out, false);
                    if !hidden {
                        out.extend(rows);
                    }
                }
                Place::Work | Place::Hidden => {}
            },
            ConversationRowContent::InProgress { .. } => {
                flush(&mut work, &mut out, true);
                if live == Some(LiveActivity::Thinking) {
                    out.push(TurnRow::Thinking);
                }
            }
            // As T3 Code, a request waiting for its answer and a retry of
            // the provider are the live work, the work before them its group.
            ConversationRowContent::PendingRequest { request } => {
                work.clear();
                out.push(TurnRow::Requested {
                    label: request_label(language, request),
                    approval: matches!(
                        request.request_body,
                        agent_protocol::requests::RequestBody::Approval { .. }
                            | agent_protocol::requests::RequestBody::Permission { .. }
                    ),
                });
            }
            ConversationRowContent::Error { error } if error.is_reconnecting => {
                work.clear();
                out.push(TurnRow::Retrying {
                    label: error.title.clone(),
                });
            }
            _ => {
                flush(&mut work, &mut out, false);
                hidden = false;
                out.push(TurnRow::Row(row.clone()));
            }
        }
    }
    flush(&mut work, &mut out, false);
    if running {
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
        // A retry is the live work.
        if out
            .iter()
            .any(|row| matches!(row, TurnRow::Retrying { .. }))
        {
            out.retain(|row| !matches!(row, TurnRow::Live { .. } | TurnRow::Thinking));
        }
    }
    let mut out = merge_subagents(out);
    trailing_actions(&mut out);
    if status == TurnStatus::Interrupted {
        out.push(TurnRow::Interrupted);
    }
    out
}

/// A child keeps its first row's position while later activity updates it.
fn merge_subagents(rows: Vec<TurnRow>) -> Vec<TurnRow> {
    let mut out = Vec::new();
    let mut positions = std::collections::HashMap::new();
    for row in rows {
        if let TurnRow::Subagent { agent, .. } = &row
            && let Some(target) = &agent.target
        {
            if let Some(&index) = positions.get(target) {
                if let TurnRow::Subagent { agent: old, .. } = &mut out[index] {
                    if !agent.message.is_empty() {
                        old.message = agent.message.clone();
                    }
                    if agent.status != TurnStatus::Unknown {
                        old.status = agent.status;
                    }
                }
                continue;
            }
            positions.insert(target.clone(), out.len());
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
            TurnRow::Group { .. } | TurnRow::Work { .. } => true,
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
    use agent_protocol::requests::RequestBody;
    match &request.request_body {
        RequestBody::Approval { .. } | RequestBody::Permission { .. }
            if !request.body.trim().is_empty() =>
        {
            request.body.trim().to_owned()
        }
        RequestBody::Approval { .. } | RequestBody::Permission { .. } => request.title.clone(),
        _ => language.text("入力を求めています").into(),
    }
}

/// Where T3 Code's timeline shows a work item.
enum Place {
    /// Among the work, in its group or alone.
    Work,
    /// Apart from the work, which it ends: a plan, a subagent, a divider.
    Alone(Vec<TurnRow>),
    /// Not at all: work without an outcome to show (an image looked at, a
    /// sleep, review mode entered or left).
    Hidden,
}

fn place(language: UiLanguage, item: &RenderedItem) -> Place {
    let ItemSource::Native(native) = &item.source else {
        return Place::Work;
    };
    match native.body() {
        ItemBody::Plan { text } => Place::Alone(vec![TurnRow::Plan {
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
        ItemBody::ImageView { .. } | ItemBody::Sleep {} | ItemBody::Review { .. } => Place::Hidden,
        _ => Place::Work,
    }
}

/// A plan's title, as T3 Code's: its first heading.
fn plan_title(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix('#'))
        .map(|heading| heading.trim_start_matches('#').trim().to_owned())
        .filter(|heading| !heading.is_empty())
}

/// The suggested name and complete Markdown exported by a plan's actions.
pub fn plan_export(text: &str) -> (String, String) {
    let title = plan_title(text).unwrap_or_default();
    let mut name = String::new();
    for character in title.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character);
        } else if !"`'\".,!?()[]{}".contains(character) && !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    (
        format!("{}.md", if name.is_empty() { "plan" } else { name }),
        format!("{}\n", text.trim_end()),
    )
}

fn activity_item(row: &ConversationRow) -> Option<&Arc<RenderedItem>> {
    match &row.content {
        ConversationRowContent::Activity { item, .. } => Some(item),
        _ => None,
    }
}

fn is_commentary(item: &RenderedItem) -> bool {
    matches!(item.data.kind.as_str(), "commentary" | "agent")
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
    let work = |row: &ConversationRow| TurnRow::Work {
        row: row.clone(),
        label: activity_item(row).map_or_else(String::new, |item| work_label(language, item)),
    };
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
                id,
                open,
            });
            if open {
                out.extend(rows.iter().map(|row| work(row)));
            }
        }
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
pub fn live_label(language: UiLanguage, item: &RenderedItem) -> String {
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
pub fn group_summary(language: UiLanguage, items: &[&Arc<RenderedItem>]) -> String {
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
