//! The selected conversation's agent fleet, independent of conversation navigation.
use super::task_title;
mod timing;
use crate::localization::UiLanguage;
use crate::{
    models::{Item, ItemBody, ItemStatus, SessionStatus, TurnStatus},
    session::{ProviderKind, SessionRef},
    state::Snapshot,
};
use std::collections::{BTreeMap, HashMap, HashSet};
pub use timing::{subagent_elapsed, subagent_group};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq)]
pub struct AgentSummary {
    pub key: String,
    pub title: String,
    pub status: String,
    pub active: bool,
    pub unread: bool,
    pub model: Option<String>,
    pub detail: Option<String>,
    pub depth: u32,
}

/// A provider-owned child is read-only. Its reported execution selection is
/// independent of the parent's or the user's unsent composer draft.
pub struct ProviderSubagentPresentation {
    pub provider: Option<ProviderKind>,
    pub parent: Option<SessionRef>,
    pub model: String,
    pub effort: Option<String>,
    status: Option<TurnStatus>,
    started_at_ms: Option<f64>,
    completed_at_ms: Option<f64>,
}

impl ProviderSubagentPresentation {
    /// Without `now_seconds`, this is the stable screen-reader announcement.
    /// The visible clock uses only the latest provider-owned turn's timing.
    pub fn label(&self, language: UiLanguage, now_seconds: Option<f64>) -> String {
        let label = language.text(match self.status {
            Some(TurnStatus::Running) => "subagent.working",
            Some(TurnStatus::Completed) => "subagent.completed",
            Some(TurnStatus::Failed) => "失敗",
            Some(TurnStatus::Interrupted) => "subagent.interrupted",
            Some(TurnStatus::Unknown) | None => "subagent.starting",
        });
        if !matches!(
            self.status,
            Some(TurnStatus::Running | TurnStatus::Completed)
        ) {
            return label.into();
        }
        let timing = now_seconds
            .zip(self.started_at_ms)
            .and_then(|(now, start)| {
                let end = if self.status == Some(TurnStatus::Running) {
                    now * 1000.0
                } else {
                    self.completed_at_ms?
                };
                let elapsed = end - start;
                elapsed
                    .is_finite()
                    .then_some((elapsed.max(0.0) / 1000.0).floor().max(1.0) as u64)
            });
        let Some(seconds) = timing else {
            return label.into();
        };
        let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
        let elapsed = [
            (hours, "duration.hours", hours > 0),
            (minutes, "duration.minutes", minutes > 0),
            (seconds, "duration.seconds", seconds > 0 || minutes > 0),
        ]
        .into_iter()
        .filter(|(_, _, visible)| *visible)
        .map(|(value, key, _)| {
            crate::localization::localized_ui_format(language, key.into(), vec![value.to_string()])
        })
        .collect::<Vec<_>>()
        .join(" ");
        if self.status == Some(TurnStatus::Completed) {
            crate::localization::localized_ui_format(
                language,
                "subagent.completed.duration".into(),
                vec![elapsed],
            )
        } else {
            format!("{label} {elapsed}")
        }
    }
}

type AgentEntries = BTreeMap<String, (SessionRef, Option<SessionRef>, AgentSummary)>;

/// A child conversation named by a provider's subagent activity. A provider
/// without a conversation identity cannot offer navigation to that child.
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentPresentation {
    pub target: Option<SessionRef>,
    pub name: String,
    pub message: String,
    pub status: TurnStatus,
    pub result: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl SubagentPresentation {
    pub fn provider(&self, active: Option<ProviderKind>) -> Option<ProviderKind> {
        active.or_else(|| self.target.as_ref().and_then(|target| target.provider))
    }

    pub fn status(&self, child: Option<&crate::models::Thread>) -> TurnStatus {
        child
            .and_then(|child| child.turns.as_ref()?.last())
            .map(|turn| turn.status)
            .filter(|status| *status != TurnStatus::Unknown)
            .unwrap_or(self.status)
    }

    pub fn name(&self, child: Option<&crate::models::Thread>) -> String {
        use std::sync::LazyLock;
        static PREFIX: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?i)^Subagent:\s*").unwrap());
        static PATH: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"^/root/(?:[^/]+/)*([^/]+)/?$").unwrap());
        let name = child
            .and_then(|child| child.name.as_deref())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.name);
        let name = PREFIX.replace(name, "");
        let Some(path) = PATH.captures(&name) else {
            return name.into_owned();
        };
        let label = path[1].replace('_', " ");
        let label = label
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .unwrap()
                    .to_uppercase()
                    .chain(chars)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        if label.is_empty() {
            name.into_owned()
        } else {
            label
        }
    }

    /// T3's lifecycle row prefers progress while active, and the result once
    /// settled. A generic provider termination message is not useful detail.
    pub fn detail(&self, status: TurnStatus) -> Option<String> {
        use std::sync::LazyLock;
        static GENERIC_END: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?i)^Child task ended with status\b").unwrap());
        static LINKS: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap());
        static BULLETS: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?m)^[ \t]*[-*][ \t]+").unwrap());
        let raw = preferred_detail(status, &self.message, self.result.as_deref())?;
        if GENERIC_END.is_match(raw) {
            return None;
        }
        let text = LINKS.replace_all(raw, "$1").replace('`', "");
        let text = BULLETS
            .replace_all(&text, "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        (!text.is_empty()).then_some(text)
    }

    /// The hover card keeps authored punctuation and caps its preview at 280
    /// UTF-16 units, matching the source without splitting a Unicode scalar.
    pub fn preview(&self, status: TurnStatus) -> Option<String> {
        let raw = preferred_detail(status, &self.message, self.result.as_deref())?;
        let chars = raw
            .split_whitespace()
            .enumerate()
            .flat_map(|(index, word)| (index > 0).then_some(' ').into_iter().chain(word.chars()));
        let mut preview = String::new();
        let mut units = 0;
        for ch in chars {
            units += ch.len_utf16();
            if units > 280 {
                return Some(format!("{}…", preview.trim_end()));
            }
            preview.push(ch);
        }
        Some(preview)
    }
}

fn preferred_detail<'a>(
    status: TurnStatus,
    progress: &'a str,
    result: Option<&'a str>,
) -> Option<&'a str> {
    let progress = Some(progress.trim()).filter(|s| !s.is_empty());
    let result = result.map(str::trim).filter(|s| !s.is_empty());
    if matches!(
        status,
        TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
    ) {
        result.or(progress)
    } else {
        progress.or(result)
    }
}

/// A reported model starts a selection; an effort-only report updates that
/// selection. Wait/status records with neither field preserve it.
pub(super) fn subagent_selection<'a>(
    previous_model: Option<&'a str>,
    previous_effort: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let effort = effort.map(str::trim).filter(|effort| !effort.is_empty());
    (
        model.or(previous_model),
        if model.is_some() {
            effort
        } else {
            effort.or(previous_effort)
        },
    )
}

pub(super) fn subagent_rows(language: UiLanguage, item: &Item) -> Vec<SubagentPresentation> {
    let ItemBody::Subagent {
        receivers,
        states,
        prompt,
        result,
        model,
        effort,
        ..
    } = item.body()
    else {
        return Vec::new();
    };
    let mut targets: Vec<_> = receivers
        .iter()
        .chain(states.iter().map(|state| &state.session))
        .collect();
    let mut seen = HashSet::new();
    targets.retain(|target| seen.insert((*target).clone()));
    let (model, effort) = subagent_selection(None, None, model.as_deref(), effort.as_deref());
    let row = |target: Option<&SessionRef>| {
        let state = states.iter().find(|state| Some(&state.session) == target);
        SubagentPresentation {
            target: target.cloned(),
            name: prompt
                .as_deref()
                .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                .unwrap_or_else(|| language.text("work.subagent"))
                .to_owned(),
            message: state
                .and_then(|state| state.message.clone())
                .unwrap_or_default(),
            status: state
                .map(|state| state.status)
                .unwrap_or(if target.is_some() {
                    TurnStatus::Unknown
                } else {
                    match item.status {
                        ItemStatus::Running => TurnStatus::Running,
                        ItemStatus::Completed => TurnStatus::Completed,
                        ItemStatus::Failed => TurnStatus::Failed,
                        _ => TurnStatus::Unknown,
                    }
                }),
            result: result
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        }
    };
    if targets.is_empty() {
        vec![row(None)]
    } else {
        targets
            .into_iter()
            .map(|target| row(Some(target)))
            .collect()
    }
}

impl Snapshot {
    /// A view's shared elapsed clock runs only for work in that conversation
    /// or its cached direct children, independent of connection availability.
    pub fn conversation_clock_active(&self, session: &SessionRef) -> bool {
        let active = |thread: &crate::models::Thread| {
            thread.active_turn_id().is_some()
                || thread.workspace_setup.as_ref().is_some_and(|setup| {
                    setup.phase == agent_protocol::workspace::SetupPhase::Running
                })
        };
        self.thread_metadata(session).is_some_and(active)
            || self
                .sessions
                .conversations
                .values()
                .any(|child| child.parent_id.as_ref() == Some(session) && active(child))
    }

    pub fn provider_subagent(&self, session: &SessionRef) -> Option<ProviderSubagentPresentation> {
        let thread = self.thread_metadata(session)?;
        if thread.can_accept_direct_input != Some(false) {
            return None;
        }
        let provider = thread.active_provider.or(session.provider);
        // Later wait/progress items may not repeat the spawn's model or effort.
        // Read only this child's mentions in its own parent's transcript.
        let (model, effort) = thread
            .parent_id
            .as_ref()
            .and_then(|parent| self.sessions.conversations.get(parent))
            .into_iter()
            .flat_map(|parent| parent.turns.iter().flatten())
            .flat_map(|turn| turn.items.iter().flatten())
            .filter_map(|item| match item.body() {
                ItemBody::Subagent {
                    receivers,
                    states,
                    model,
                    effort,
                    ..
                } if receivers.contains(session)
                    || states.iter().any(|state| &state.session == session) =>
                {
                    Some((model.as_deref(), effort.as_deref()))
                }
                _ => None,
            })
            .fold(
                (None, None),
                |(previous_model, previous_effort), (model, effort)| {
                    subagent_selection(previous_model, previous_effort, model, effort)
                },
            );
        let catalog = self.catalog.models.iter().find(|entry| {
            Some(entry.model.provider) == provider && Some(entry.model.id.as_str()) == model
        });
        let effort = effort
            .filter(|_| {
                catalog
                    .and_then(|model| model.settings.as_ref())
                    .is_some_and(|settings| !settings.supported_reasoning_efforts.is_empty())
            })
            .map(|effort| {
                super::model_settings::reasoning_effort_label(self.catalog.ui_language, effort)
            });
        let turn = thread.turns.as_ref().and_then(|turns| turns.last());
        Some(ProviderSubagentPresentation {
            provider,
            parent: thread.parent_id.clone(),
            model: catalog
                .map(|model| model.display_name.as_str())
                .or(model)
                .unwrap_or_default()
                .into(),
            effort,
            status: turn.map(|turn| turn.status),
            started_at_ms: turn.and_then(|turn| {
                turn.started_at_ms
                    .map(|ms| ms as f64)
                    .or_else(|| turn.started_at.map(|seconds| seconds * 1000.0))
            }),
            completed_at_ms: turn.and_then(|turn| turn.completed_at_ms.map(|ms| ms as f64)),
        })
    }

    /// Approval banners belong to the selected thread's direct children. Keep
    /// the child's request identity so responding never targets the parent.
    pub fn child_approvals(
        &self,
        parent: &SessionRef,
    ) -> Vec<(SessionRef, String, super::conversation::Request)> {
        let mut approvals = Vec::new();
        for (session, thread) in self.sessions.conversations.iter() {
            if thread.parent_id.as_ref() != Some(parent) {
                continue;
            }
            let title = thread
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .or(thread.preview.as_deref())
                .unwrap_or(session.id.as_str());
            for request in thread.requests.values() {
                if request.body.is_approval() {
                    approvals.push((
                        session.clone(),
                        title.to_owned(),
                        super::conversation::request(self.catalog.ui_language, request),
                    ));
                }
            }
        }
        approvals.sort_by(|left, right| {
            left.0
                .to_string()
                .cmp(&right.0.to_string())
                .then_with(|| left.2.id.cmp(&right.2.id))
        });
        approvals
    }

    pub fn agent_panel(&self, pane: &crate::state::PaneState) -> Vec<AgentSummary> {
        let Some(root) = pane.navigation.thread_id.as_ref() else {
            return Vec::new();
        };
        let mut entries = AgentEntries::new();
        for thread in self.sessions.conversations.values() {
            let (Some(id), Some(parent)) = (&thread.id, &thread.parent_id) else {
                continue;
            };
            let observed = self.lists.activity.active.get(id).copied();
            let row = session_agent(
                self.catalog.ui_language,
                id,
                task_title(
                    self.catalog.ui_language,
                    thread.name.as_deref(),
                    thread.preview.as_deref(),
                ),
                match thread.status {
                    SessionStatus::Running => TurnStatus::Running,
                    _ => TurnStatus::Unknown,
                },
                observed.or_else(|| (thread.status == SessionStatus::Idle).then_some(false)),
                self.lists.activity.unread.contains(id),
            );
            entries.insert(row.key.clone(), (parent.clone(), Some(id.clone()), row));
        }
        entries = canonical_agents(
            &self.sessions.projections,
            &self.lists.activity.active,
            &self.lists.activity.unread,
            self.catalog.ui_language,
            entries,
        );
        for (owner, item, provider) in self.conversation_items() {
            if self.sessions.projections.contains_key(owner) {
                continue;
            }
            let ItemBody::Subagent {
                tool,
                prompt,
                model,
                receivers,
                states,
                result,
                ..
            } = item.body()
            else {
                continue;
            };
            let detail = result
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .or(prompt.as_deref())
                .map(str::to_owned);
            if receivers.is_empty() {
                if provider != ProviderKind::Claude {
                    continue;
                }
                let row = local_agent(
                    self.catalog.ui_language,
                    owner,
                    &item.id,
                    item.status,
                    prompt.as_deref(),
                    model.clone(),
                    detail,
                );
                entries.insert(row.key.clone(), (owner.clone(), None, row));
            } else {
                for receiver in receivers {
                    if receiver.provider != Some(provider) {
                        continue;
                    }
                    let key = session_key(receiver);
                    if !entries.contains_key(&key) && tool != "spawnAgent" {
                        continue;
                    }
                    let entry = entries.entry(key).or_insert_with(|| {
                        let row = session_agent(
                            self.catalog.ui_language,
                            receiver,
                            &receiver.id,
                            TurnStatus::Unknown,
                            self.lists.activity.active.get(receiver).copied(),
                            self.lists.activity.unread.contains(receiver),
                        );
                        (owner.clone(), Some(receiver.clone()), row)
                    });
                    if &entry.0 != owner {
                        continue;
                    }
                    if model.is_some() {
                        entry.2.model = model.clone();
                    }
                    if let Some(state) = states.iter().find(|state| &state.session == receiver) {
                        let (status, active) = agent_status(
                            state.status,
                            self.lists.activity.active.get(receiver).copied(),
                        );
                        entry.2.status = self.catalog.ui_language.text(status).into();
                        entry.2.active = active;
                        entry.2.detail = state.message.clone().or_else(|| detail.clone());
                    } else if detail.is_some() {
                        entry.2.detail = detail.clone();
                    }
                }
            }
        }
        scoped_agents(root, entries)
    }
}

pub(super) fn empty() -> std::sync::Arc<Vec<AgentSummary>> {
    use std::sync::{Arc, LazyLock};
    static EMPTY: LazyLock<Arc<Vec<AgentSummary>>> = LazyLock::new(Arc::default);
    EMPTY.clone()
}

pub(super) fn project(
    snapshot: &Snapshot,
    pane: &crate::state::PaneState,
    previous: Option<&super::view::Presentation>,
) -> std::sync::Arc<Vec<AgentSummary>> {
    use crate::state::Same;
    use std::sync::Arc;
    if pane.observed_agents.is_none() || pane.observed_agents != pane.navigation.thread_id {
        return empty();
    }
    if let Some(previous) = previous
        && snapshot.catalog.ui_language == previous.snapshot.catalog.ui_language
        && pane.observed_agents == previous.pane().observed_agents
        && snapshot
            .sessions
            .conversations
            .same(&previous.snapshot.sessions.conversations)
        && snapshot
            .sessions
            .projections
            .same(&previous.snapshot.sessions.projections)
        && snapshot
            .lists
            .activity
            .same(&previous.snapshot.lists.activity)
    {
        return previous.agents.clone();
    }
    let agents = snapshot.agent_panel(pane);
    if let Some(previous) = previous
        && agents == *previous.agents
    {
        return previous.agents.clone();
    }
    Arc::new(agents)
}

fn canonical_agents(
    projections: &BTreeMap<
        SessionRef,
        std::sync::Arc<agent_protocol::orchestration::projection::ThreadSnapshot>,
    >,
    active: &BTreeMap<SessionRef, bool>,
    unread: &std::collections::BTreeSet<SessionRef>,
    language: UiLanguage,
    mut entries: AgentEntries,
) -> AgentEntries {
    use agent_protocol::orchestration as model;
    for (owner, snapshot) in projections {
        for task in &snapshot.projection.subagents {
            let item=snapshot.projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::Subagent {subagent_id,..} if subagent_id==&task.id));
            let (prompt, result) = item
                .and_then(|item| match &item.body {
                    model::TurnItemBody::Subagent { prompt, result, .. } => {
                        Some((Some(prompt.as_str()), result.clone()))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let detail = result.or_else(|| prompt.map(str::to_owned));
            let (status, item_status) = match task.status {
                model::SubagentStatus::Pending
                | model::SubagentStatus::Running
                | model::SubagentStatus::Waiting => (TurnStatus::Running, ItemStatus::Running),
                model::SubagentStatus::Completed => (TurnStatus::Completed, ItemStatus::Completed),
                model::SubagentStatus::Failed => (TurnStatus::Failed, ItemStatus::Failed),
                model::SubagentStatus::Cancelled | model::SubagentStatus::Interrupted => {
                    (TurnStatus::Interrupted, ItemStatus::Interrupted)
                }
                model::SubagentStatus::Idle => (TurnStatus::Unknown, ItemStatus::Unknown),
            };
            let (child, mut row) = if let Some(child) = task.child_thread_id.as_deref() {
                let Ok(child) = model::shell::public_session_ref(child) else {
                    continue;
                };
                let title = task_title(language, task.title.as_deref(), prompt);
                let row = session_agent(
                    language,
                    &child,
                    title,
                    status,
                    active.get(&child).copied(),
                    unread.contains(&child),
                );
                (Some(child), row)
            } else {
                (
                    None,
                    local_agent(
                        language,
                        owner,
                        &item
                            .map_or_else(|| task.id.clone(), |item| item.id.clone())
                            .into(),
                        item_status,
                        prompt,
                        None,
                        None,
                    ),
                )
            };
            row.model = task.model.clone();
            row.detail = detail;
            if let Some((known_parent, _, known)) = entries.get(&row.key) {
                if known_parent != owner {
                    continue;
                }
                row.title = known.title.clone();
            }
            entries.insert(row.key.clone(), (owner.clone(), child, row));
        }
    }
    entries
}

impl Snapshot {
    fn conversation_items(&self) -> impl Iterator<Item = (&SessionRef, &Item, ProviderKind)> {
        self.sessions
            .conversations
            .iter()
            .flat_map(|(owner, thread)| {
                thread.turns.iter().flatten().flat_map(move |turn| {
                    turn.items.iter().flatten().filter_map(move |item| {
                        Some((
                            owner,
                            item.as_ref(),
                            turn.provider
                                .or(thread.active_provider)
                                .or(owner.provider)?,
                        ))
                    })
                })
            })
    }
}

fn session_key(id: &SessionRef) -> String {
    format!("session:{id}")
}

fn session_agent(
    language: UiLanguage,
    id: &SessionRef,
    title: &str,
    status: TurnStatus,
    observed_active: Option<bool>,
    unread: bool,
) -> AgentSummary {
    let (status, active) = agent_status(status, observed_active);
    AgentSummary {
        key: session_key(id),
        title: title.into(),
        status: language.text(status).into(),
        active,
        unread,
        model: None,
        detail: None,
        depth: 0,
    }
}

fn local_agent(
    language: UiLanguage,
    owner: &SessionRef,
    item_id: &agent_protocol::ids::ItemId,
    item_status: ItemStatus,
    prompt: Option<&str>,
    model: Option<String>,
    detail: Option<String>,
) -> AgentSummary {
    let key = format!("item:{owner}:{item_id}");
    let (status, active) = agent_status(
        match item_status {
            ItemStatus::Running => TurnStatus::Running,
            ItemStatus::Completed => TurnStatus::Completed,
            ItemStatus::Failed => TurnStatus::Failed,
            ItemStatus::Interrupted | ItemStatus::Declined => TurnStatus::Interrupted,
            ItemStatus::Unknown => TurnStatus::Unknown,
        },
        None,
    );
    AgentSummary {
        key,
        title: prompt
            .map(crate::models::compact_title)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| language.text("サブエージェント").into()),
        status: language.text(status).into(),
        active,
        unread: false,
        model,
        detail,
        depth: 0,
    }
}

fn agent_status(status: TurnStatus, observed_active: Option<bool>) -> (&'static str, bool) {
    let active = observed_active.unwrap_or(status == TurnStatus::Running);
    (
        if active {
            "実行中"
        } else {
            match status {
                TurnStatus::Completed => "完了",
                TurnStatus::Failed => "失敗",
                TurnStatus::Interrupted => "停止",
                TurnStatus::Running => "待機",
                TurnStatus::Unknown if observed_active == Some(false) => "待機",
                TurnStatus::Unknown => "未確認",
            }
        },
        active,
    )
}

fn scoped_agents(root: &SessionRef, entries: AgentEntries) -> Vec<AgentSummary> {
    let mut children: HashMap<_, Vec<_>> = HashMap::new();
    for (_, (parent, session, row)) in entries {
        children.entry(parent).or_default().push((session, row));
    }
    let mut pending: Vec<_> = children
        .remove(root)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|(session, row)| (session, row, 0))
        .collect();
    let mut seen = HashSet::from([root.clone()]);
    let mut rows = Vec::new();
    while let Some((session, mut row, depth)) = pending.pop() {
        if let Some(session) = session {
            if !seen.insert(session.clone()) {
                continue;
            }
            pending.extend(
                children
                    .remove(&session)
                    .unwrap_or_default()
                    .into_iter()
                    .rev()
                    .map(|(session, row)| (session, row, depth + 1)),
            );
        }
        row.depth = depth;
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Item, ItemContent, Thread, Turn};
    use proptest::prelude::*;
    use std::sync::Arc;

    #[test]
    fn subagent_provider_uses_live_driver_then_the_qualified_child_address() {
        let mut agent = SubagentPresentation {
            target: Some(SessionRef::new(ProviderKind::Claude, "child".into()).unwrap()),
            name: String::new(),
            message: String::new(),
            status: TurnStatus::Unknown,
            result: None,
            model: None,
            effort: None,
        };
        assert_eq!(agent.provider(None), Some(ProviderKind::Claude));
        assert_eq!(
            agent.provider(Some(ProviderKind::Codex)),
            Some(ProviderKind::Codex)
        );
        agent.target = None;
        assert_eq!(agent.provider(None), None);
        assert_eq!(
            agent.provider(Some(ProviderKind::DeepSeek)),
            Some(ProviderKind::DeepSeek)
        );
    }

    #[test]
    fn provider_child_elapsed_uses_whole_seconds_and_keeps_its_announcement_stable() {
        let mut agent = ProviderSubagentPresentation {
            provider: None,
            parent: None,
            model: String::new(),
            effort: None,
            status: Some(TurnStatus::Running),
            started_at_ms: Some(10_000.0),
            completed_at_ms: None,
        };
        for (now, expected) in [
            (9.0, "Working 1s"),
            (10.0, "Working 1s"),
            (12.9, "Working 2s"),
            (70.0, "Working 1m 0s"),
            (3610.0, "Working 1h"),
        ] {
            assert_eq!(agent.label(UiLanguage::English, Some(now)), expected);
            assert_eq!(agent.label(UiLanguage::English, None), "Working");
        }
        assert_eq!(agent.label(UiLanguage::English, Some(f64::NAN)), "Working");
        agent.status = Some(TurnStatus::Completed);
        agent.completed_at_ms = Some(12_900.0);
        assert_eq!(
            agent.label(UiLanguage::English, Some(100.0)),
            "Completed in 2s"
        );
        assert_eq!(
            agent.label(UiLanguage::English, Some(200.0)),
            "Completed in 2s"
        );
        assert_eq!(agent.label(UiLanguage::English, None), "Completed");
        agent.completed_at_ms = None;
        assert_eq!(agent.label(UiLanguage::English, Some(200.0)), "Completed");
        for (status, label) in [
            (TurnStatus::Failed, "Failed"),
            (TurnStatus::Interrupted, "Interrupted"),
            (TurnStatus::Unknown, "Starting"),
        ] {
            agent.status = Some(status);
            assert_eq!(agent.label(UiLanguage::English, Some(200.0)), label);
        }
        agent.status = Some(TurnStatus::Running);
        agent.started_at_ms = Some(f64::INFINITY);
        assert_eq!(agent.label(UiLanguage::English, Some(200.0)), "Working");
        agent.started_at_ms = Some(-f64::MAX);
        assert_eq!(
            agent.label(UiLanguage::English, Some(f64::MAX / 1000.0)),
            "Working"
        );
        agent.started_at_ms = None;
        assert_eq!(agent.label(UiLanguage::English, Some(200.0)), "Working");
        agent.status = None;
        assert_eq!(agent.label(UiLanguage::English, Some(200.0)), "Starting");
    }

    #[test]
    fn conversation_clock_follows_selected_work_and_direct_children_even_offline() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"}, "parentId":{"provider":"codex","id":"root"}, "turns":[{"id":"live","status":"running"}]},
            {"id":{"provider":"codex","id":"other"}, "turns":[{"id":"other-live","status":"running"}]}
        ]));
        assert!(!state.connection.connected);
        assert!(state.conversation_clock_active(&session("root")));
        assert!(state.conversation_clock_active(&session("child")));
        assert!(!state.conversation_clock_active(&session("missing")));
        Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&session("child"))
                .unwrap(),
        )
        .turns = None;
        assert!(
            !state.conversation_clock_active(&session("root")),
            "unrelated work must not tick this conversation"
        );
        assert!(state.conversation_clock_active(&session("other")));
    }

    #[test]
    fn background_setup_ticks_only_its_conversation_and_parent_until_it_finishes() {
        use agent_protocol::workspace::{SetupPhase, WorkspaceSetup};
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"other"}}
        ]));
        for (phase, expected) in [
            (SetupPhase::Running, true),
            (SetupPhase::Done, false),
            (SetupPhase::Failed, false),
            (SetupPhase::Cancelled, false),
        ] {
            let child = Arc::make_mut(
                Arc::make_mut(&mut state.sessions.conversations)
                    .get_mut(&session("child"))
                    .unwrap(),
            );
            let mut setup = WorkspaceSetup::new(None, 1000);
            setup.phase = phase;
            child.workspace_setup = Some(setup);
            assert_eq!(state.conversation_clock_active(&session("root")), expected);
            assert_eq!(state.conversation_clock_active(&session("child")), expected);
            assert!(!state.conversation_clock_active(&session("other")));
        }
    }

    #[test]
    fn subagent_elapsed_formats_minutes_and_hours_as_the_source_agent_clock() {
        for (ms, expected) in [
            (59_999, "59s"),
            (60_000, "1m 00s"),
            (61_000, "1m 01s"),
            (3_599_999, "59m 59s"),
            (3_600_000, "1h 00m"),
            (3_661_999, "1h 01m"),
        ] {
            let child = Thread {
                turns: Some(vec![Arc::new(Turn {
                    duration_ms: Some(ms),
                    ..Default::default()
                })]),
                ..Default::default()
            };
            assert_eq!(
                subagent_elapsed(UiLanguage::English, Some(&child), 99.0).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn provider_children_state_only_mentions_do_not_match_another_child() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"}, "parentId":{"provider":"codex","id":"root"},
                "canAcceptDirectInput":false}
        ]));
        for (target, expected_model) in [("child", "review-model"), ("other", "")] {
            put_items(
                &mut state,
                session("root"),
                vec![item(
                    "state-only",
                    ItemStatus::Completed,
                    "wait",
                    vec![],
                    vec![crate::models::SubagentState {
                        session: session(target),
                        status: TurnStatus::Completed,
                        native_status: None,
                        message: None,
                    }],
                )],
            );
            assert_eq!(
                state.provider_subagent(&session("child")).unwrap().model,
                expected_model
            );
        }
    }

    #[test]
    fn provider_children_use_their_reported_selection_and_preserve_missing_metadata() {
        use serde_json::json;
        let child = session("child");
        let mut state = snapshot(json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"}, "parentId":{"provider":"codex","id":"root"},
                "canAcceptDirectInput":false, "turns":[{"id":"run","status":"running"}]}
        ]));
        state.catalog.ui_language = UiLanguage::English;
        let reported = |id: &str, target: SessionRef, model: Option<&str>, effort: Option<&str>| {
            Arc::new(Item::new(
                id.into(),
                ItemStatus::Completed,
                ItemBody::Subagent {
                    tool: "task".into(),
                    prompt: None,
                    model: model.map(str::to_owned),
                    effort: effort.map(str::to_owned),
                    sender: None,
                    receivers: vec![target],
                    states: vec![],
                    agent_id: None,
                    result: None,
                },
            ))
        };
        let catalog: Arc<Vec<crate::models::Model>> = Arc::new(serde_json::from_value(json!([
            {"id":"review-model","model":{"provider":"codex","id":"review-model"}, "displayName":"Review Model",
                "settings":{"defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"high"}]}},
            {"id":"other-model","model":{"provider":"codex","id":"other-model"}, "displayName":"Other Model",
                "settings":{"defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"high"}]}}
        ])).unwrap());
        state.catalog.models = catalog.clone();
        let spawn = reported(
            "spawn",
            child.clone(),
            Some(" review-model "),
            Some(" high "),
        );
        put_items(
            &mut state,
            session("root"),
            vec![
                spawn.clone(),
                reported("wait", child.clone(), None, None),
                reported("unrelated", session("unrelated"), Some("other-model"), None),
            ],
        );
        let shown = state.provider_subagent(&child).unwrap();
        assert_eq!(shown.parent, Some(session("root")));
        assert_eq!(shown.model, "Review Model");
        assert_eq!(shown.effort.as_deref(), Some("High"));
        assert_eq!(shown.label(UiLanguage::English, None), "Working");
        put_items(
            &mut state,
            session("root"),
            vec![
                spawn.clone(),
                reported("effort-update", child.clone(), None, Some("low")),
            ],
        );
        assert_eq!(
            state.provider_subagent(&child).unwrap().effort.as_deref(),
            Some("Low")
        );
        state.catalog.models = Arc::new(vec![]);
        let unknown = state.provider_subagent(&child).unwrap();
        assert_eq!(unknown.model, "review-model");
        assert!(unknown.effort.is_none());
        state.catalog.models = catalog;
        put_items(
            &mut state,
            session("root"),
            vec![
                spawn,
                reported("new-task", child.clone(), Some("other-model"), None),
            ],
        );
        let next = state.provider_subagent(&child).unwrap();
        assert_eq!(next.model, "Other Model");
        assert!(
            next.effort.is_none(),
            "a new selection must not inherit the previous task's effort"
        );
        for (status, label) in [
            (TurnStatus::Completed, "Completed"),
            (TurnStatus::Failed, "Failed"),
            (TurnStatus::Interrupted, "Interrupted"),
            (TurnStatus::Unknown, "Starting"),
        ] {
            let thread = Arc::make_mut(
                Arc::make_mut(&mut state.sessions.conversations)
                    .get_mut(&child)
                    .unwrap(),
            );
            Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]).status = status;
            assert_eq!(
                state
                    .provider_subagent(&child)
                    .unwrap()
                    .label(UiLanguage::English, None),
                label
            );
        }
        for direct_input in [None, Some(true)] {
            Arc::make_mut(
                Arc::make_mut(&mut state.sessions.conversations)
                    .get_mut(&child)
                    .unwrap(),
            )
            .can_accept_direct_input = direct_input;
            assert!(state.provider_subagent(&child).is_none());
        }
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&child)
                .unwrap(),
        );
        thread.can_accept_direct_input = Some(false);
        thread.parent_id = None;
        thread.turns = None;
        let unknown = state.provider_subagent(&child).unwrap();
        assert!(unknown.parent.is_none());
        assert!(unknown.model.is_empty());
        assert!(unknown.effort.is_none());
        assert_eq!(unknown.label(UiLanguage::English, None), "Starting");
    }

    #[test]
    fn subagent_names_and_hover_previews_follow_source_formatting_and_unicode_bounds() {
        let mut agent = SubagentPresentation {
            target: None,
            name: "Subagent: fallback".into(),
            message: "- Reading `auth.rs`\n [API](https://example.test/api)".into(),
            status: TurnStatus::Running,
            result: None,
            model: None,
            effort: None,
        };
        assert_eq!(agent.name(None), "fallback");
        for (name, expected) in [
            ("Subagent: /root/task/api_review/", "Api Review"),
            ("subagent: Custom title", "Custom title"),
            ("/root/task/日本語_review", "日本語 Review"),
            ("/root/task/__", "/root/task/__"),
            ("/root//task", "/root//task"),
            ("/workspace/api_review", "/workspace/api_review"),
            ("  ", "fallback"),
        ] {
            let child = Thread {
                name: Some(name.into()),
                ..Default::default()
            };
            assert_eq!(agent.name(Some(&child)), expected);
        }
        assert_eq!(
            agent.preview(TurnStatus::Running).as_deref(),
            Some("- Reading `auth.rs` [API](https://example.test/api)")
        );
        for (text, expected) in [
            ("a".repeat(280), "a".repeat(280)),
            (
                format!("{}😀tail", "a".repeat(279)),
                format!("{}…", "a".repeat(279)),
            ),
            (
                format!("{}😀tail", "a".repeat(278)),
                format!("{}😀…", "a".repeat(278)),
            ),
            (" x \n".repeat(100), vec!["x"; 100].join(" ")),
        ] {
            agent.message = text;
            assert_eq!(
                agent.preview(TurnStatus::Running).as_deref(),
                Some(expected.as_str())
            );
        }
        agent.message.clear();
        assert!(agent.preview(TurnStatus::Completed).is_none());
    }

    #[test]
    fn subagent_detail_prefers_progress_while_active_and_results_once_settled() {
        let mut agent = SubagentPresentation {
            target: None,
            name: "Review".into(),
            message: "- Reading `auth.rs`\n * [API](https://example.test/api)".into(),
            status: TurnStatus::Running,
            result: Some("  Result\n ready  ".into()),
            model: None,
            effort: None,
        };
        for status in [TurnStatus::Running, TurnStatus::Unknown] {
            assert_eq!(agent.detail(status).as_deref(), Some("Reading auth.rs API"));
        }
        for status in [
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Interrupted,
        ] {
            assert_eq!(agent.detail(status).as_deref(), Some("Result ready"));
        }
        agent.message = "\n ".into();
        assert_eq!(
            agent.detail(TurnStatus::Running).as_deref(),
            Some("Result ready")
        );
        agent.result = Some("Child task ended with status failed".into());
        agent.message = "Useful progress".into();
        assert!(agent.detail(TurnStatus::Failed).is_none());
        agent.result = Some("  ".into());
        assert_eq!(
            agent.detail(TurnStatus::Completed).as_deref(),
            Some("Useful progress")
        );
        agent.message = "child TASK ended with STATUS completed".into();
        assert!(agent.detail(TurnStatus::Unknown).is_none());
        agent.message = "Child task ended with statuscheck".into();
        assert_eq!(
            agent.detail(TurnStatus::Running).as_deref(),
            Some("Child task ended with statuscheck")
        );
        agent.message = "- ` `".into();
        assert!(agent.detail(TurnStatus::Running).is_none());
    }

    #[test]
    fn child_approvals_exclude_other_threads_and_keep_delivery_and_order() {
        use agent_protocol::{
            requests::{ApprovalKind, Request, RequestBody, RequestTarget},
            session::RequestDelivery,
        };
        let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
        let parent = session("parent");
        let mut snapshot = Snapshot::default();
        let request = |id: &str, body, delivery| {
            Arc::new(Request {
                id: id.into(),
                target: RequestTarget::Session,
                delivery,
                body,
            })
        };
        let body = || RequestBody::Approval {
            kind: ApprovalKind::Command,
            description: "Run tests".into(),
            details: "cargo test".into(),
            choices: vec![],
        };
        let requests = [
            request("z", body(), RequestDelivery::Awaiting),
            request("a", body(), RequestDelivery::Unknown),
            request(
                "m",
                RequestBody::Permission {
                    description: "Read directory".into(),
                    details: "/repo".into(),
                    choices: vec![],
                },
                RequestDelivery::Awaiting,
            ),
            request(
                "question",
                RequestBody::Question { questions: vec![] },
                RequestDelivery::Awaiting,
            ),
        ]
        .into_iter()
        .map(|request| (request.id.clone(), request))
        .collect();
        let child = Thread {
            parent_id: Some(parent.clone()),
            name: Some("  ".into()),
            preview: Some("Review".into()),
            requests,
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .insert(session("child"), Arc::new(child.clone()));
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            parent.clone(),
            Arc::new(Thread {
                parent_id: None,
                ..child.clone()
            }),
        );
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session("other"),
            Arc::new(Thread {
                parent_id: Some(session("unrelated")),
                ..child
            }),
        );
        let approvals = snapshot.child_approvals(&parent);
        assert_eq!(approvals.len(), 3);
        assert!(
            approvals
                .iter()
                .all(|(id, title, _)| *id == session("child") && title == "Review")
        );
        assert_eq!(approvals[0].2.id.as_str(), "a");
        assert!(!approvals[0].2.can_respond);
        assert_eq!(approvals[1].2.id.as_str(), "m");
        assert!(approvals[1].2.can_respond);
        assert_eq!(approvals[2].2.id.as_str(), "z");
        assert!(approvals[2].2.can_respond);
    }

    #[test]
    fn elapsed_uses_child_wall_time_and_freezes_when_the_child_finishes() {
        let mut child = Thread {
            turns: Some(vec![
                Arc::new(Turn {
                    status: TurnStatus::Completed,
                    started_at_ms: Some(10_000),
                    completed_at_ms: Some(12_000),
                    ..Default::default()
                }),
                Arc::new(Turn {
                    status: TurnStatus::Running,
                    started_at: Some(20.0),
                    ..Default::default()
                }),
            ]),
            ..Default::default()
        };
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 25.0).as_deref(),
            Some("15s")
        );
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 26.0).as_deref(),
            Some("16s")
        );
        let turn = Arc::make_mut(&mut child.turns.as_mut().unwrap()[1]);
        turn.status = TurnStatus::Completed;
        turn.completed_at_ms = Some(27_000);
        assert_eq!(
            subagent_elapsed(UiLanguage::English, Some(&child), 99.0).as_deref(),
            Some("17s")
        );
        child.history_has_more = Some(true);
        assert!(subagent_elapsed(UiLanguage::English, Some(&child), 99.0).is_none());
        assert!(subagent_elapsed(UiLanguage::English, Some(&Thread::default()), 99.0).is_none());
        assert!(subagent_elapsed(UiLanguage::English, None, 99.0).is_none());
    }

    #[test]
    fn subagent_timing_keeps_unknown_data_unknown_and_clamps_clock_skew() {
        let child = |turn: Turn| Thread {
            turns: Some(vec![Arc::new(turn)]),
            ..Default::default()
        };
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    started_at_ms: Some(10_000),
                    duration_ms: Some(2_500),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("2s")
        );
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    duration_ms: Some(2_500),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("2s")
        );
        assert_eq!(
            subagent_elapsed(
                UiLanguage::English,
                Some(&child(Turn {
                    started_at_ms: Some(30_000),
                    completed_at_ms: Some(20_000),
                    ..Default::default()
                })),
                100.0
            )
            .as_deref(),
            Some("0s")
        );
        assert!(
            subagent_elapsed(UiLanguage::English, Some(&child(Turn::default())), 100.0).is_none()
        );
        let agent = SubagentPresentation {
            target: None,
            name: "Review".into(),
            message: String::new(),
            status: TurnStatus::Completed,
            result: None,
            model: None,
            effort: None,
        };
        assert_eq!(
            agent.status(Some(&child(Turn {
                status: TurnStatus::Running,
                ..Default::default()
            }))),
            TurnStatus::Running
        );
        assert_eq!(
            agent.status(Some(&child(Turn::default()))),
            TurnStatus::Completed
        );
        assert_eq!(
            agent.name(Some(&Thread {
                name: Some("Reviewer".into()),
                ..Default::default()
            })),
            "Reviewer"
        );
        assert_eq!(
            agent.name(Some(&Thread {
                name: Some("  ".into()),
                ..Default::default()
            })),
            "Review"
        );
    }

    fn session(id: &str) -> SessionRef {
        SessionRef {
            provider: Some(ProviderKind::Codex),
            id: id.into(),
        }
    }

    fn snapshot(data: serde_json::Value) -> Snapshot {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
            Some(session("root"));
        let data: Vec<Thread> = serde_json::from_value(data).unwrap();
        snapshot.sessions.conversations = Arc::new(
            data.iter()
                .filter_map(|thread| Some((thread.id.clone()?, Arc::new(thread.clone()))))
                .collect(),
        );
        snapshot.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
            "data":data.into_iter().filter(|thread| thread.parent_id.is_none()).collect::<Vec<_>>(),
            "projects":[],  "hasMore":false,"hasMoreProjects":false,"limit":5, }))
            .unwrap(),
        ));
        snapshot
    }

    #[test]
    fn projected_fleet_tracks_observation_and_activity_without_rebuilding_for_input() {
        use crate::presentation::view::Presentation;
        use crate::state::Draft;
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"}
        ]));
        let hidden = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert!(hidden.agents.is_empty());
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(session("root"));
        let opened = Presentation::project(Arc::new(state.clone()), "", Some(&hidden)).unwrap();
        assert!(opened.changes(hidden).agents);
        assert_eq!(opened.agents.len(), 1);
        assert!(opened.agents[0].active);
        let mut foreign = state.clone();
        Arc::make_mut(foreign.panes.primary_mut()).observed_agents = Some(session("other"));
        let foreign = Presentation::project(Arc::new(foreign), "", Some(&opened)).unwrap();
        assert!(
            foreign.agents.is_empty(),
            "another conversation's observation cannot expose this fleet"
        );
        Arc::make_mut(&mut state.input.drafts).insert(
            state.panes.primary().navigation.draft_key.clone(),
            Arc::new(Draft {
                text: "typed".into(),
                ..Default::default()
            }),
        );
        let typed = Presentation::project(Arc::new(state.clone()), "", Some(&opened)).unwrap();
        assert!(Arc::ptr_eq(&opened.agents, &typed.agents));
        Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&session("child"))
                .unwrap(),
        )
        .preview = Some("A streaming preview".into());
        let streamed = Presentation::project(Arc::new(state.clone()), "", Some(&typed)).unwrap();
        assert!(Arc::ptr_eq(&typed.agents, &streamed.agents));
        assert!(!streamed.changes(typed.clone()).agents);
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        let stopped = Presentation::project(Arc::new(state.clone()), "", Some(&typed)).unwrap();
        assert!(stopped.changes(typed).agents);
        assert!(!stopped.agents[0].active);
        Arc::make_mut(state.panes.primary_mut()).observed_agents = None;
        let closed = Presentation::current(Arc::new(state.clone()), &stopped).unwrap();
        assert!(
            closed.agents.is_empty(),
            "closing must not wait for the worker"
        );
        let projected = Presentation::project(Arc::new(state), "", Some(&closed)).unwrap();
        assert!(!projected.changes(closed).agents);
    }

    #[test]
    fn language_changes_refresh_agent_copy_and_keep_names_and_identity() {
        use crate::presentation::view::Presentation;
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"named"},"parentId":{"provider":"codex","id":"root"},"name":"設定\n{0}","status":"running"},
            {"id":{"provider":"codex","id":"unnamed"},"parentId":{"provider":"codex","id":"root"},"status":"idle"}
        ]));
        Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(session("root"));
        let japanese = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert_eq!(japanese.agents[0].status, "実行中");
        assert_eq!(japanese.agents[1].title, "無題のタスク");
        state.catalog.ui_language = UiLanguage::English;
        let english = Presentation::project(Arc::new(state.clone()), "", Some(&japanese)).unwrap();
        assert!(english.changes(japanese.clone()).agents);
        assert_eq!(english.agents[0].title, "設定\n{0}");
        assert_eq!(english.agents[0].status, "Running");
        assert_eq!(english.agents[1].title, "Untitled task");
        assert_eq!(english.agents[1].status, "Idle");
        assert_eq!(english.agents[0].key, japanese.agents[0].key);
        assert_eq!(english.agents[1].key, japanese.agents[1].key);
        assert_eq!(japanese.agents[0].status, "実行中");
        let same = Presentation::project(Arc::new(state), "", Some(&english)).unwrap();
        assert!(Arc::ptr_eq(&same.agents, &english.agents));
    }

    #[test]
    fn agents_are_scoped_to_the_selected_parent_and_keep_stable_identity() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"root"}},
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"},
            {"id":{"provider":"codex","id":"grandchild"},"parentId":{"provider":"codex","id":"child"},"name":"Check"},
            {"id":{"provider":"codex","id":"other"},"parentId":{"provider":"codex","id":"unrelated"}},
            {"id":{"provider":"codex","id":"fork"}},
            {"id":{"provider":"claude","id":"child"},"parentId":{"provider":"claude","id":"root"}},
            {"id":{"provider":"codex","id":"cross"},"parentId":{"provider":"claude","id":"root"}},
            {"id":{"provider":"codex","id":"self"},"parentId":{"provider":"codex","id":"self"}}
        ]));
        let before = state.agent_panel(state.panes.primary());
        assert_eq!(
            before
                .iter()
                .map(|row| (row.title.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [("Review", 0), ("Check", 1)]
        );
        assert!(before[0].active);
        assert_eq!(before[1].status, "未確認");
        assert_eq!(
            state.panes.primary().navigation.thread_id,
            Some(session("root"))
        );
        for thread in Arc::make_mut(&mut state.sessions.conversations).values_mut() {
            Arc::make_mut(thread).updated_at = Some(999.);
        }
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        let after = state.agent_panel(state.panes.primary());
        assert_eq!(
            before.iter().map(|row| &row.key).collect::<Vec<_>>(),
            after.iter().map(|row| &row.key).collect::<Vec<_>>()
        );
        assert!(!after[0].active);
        assert_eq!(after[0].status, "待機");
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(session("fork"));
        assert!(state.agent_panel(state.panes.primary()).is_empty());
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id = None;
        assert!(state.agent_panel(state.panes.primary()).is_empty());
    }

    fn item(
        id: &str,
        status: ItemStatus,
        tool: &str,
        receivers: Vec<SessionRef>,
        states: Vec<crate::models::SubagentState>,
    ) -> Arc<Item> {
        Arc::new(Item {
            attempt: None,
            author: None,
            id: id.into(),
            status,
            client_input_id: None,
            body: ItemContent::Inline {
                body: Box::new(ItemBody::Subagent {
                    tool: tool.into(),
                    prompt: Some("Check the change".into()),
                    model: Some("review-model".into()),
                    effort: None,
                    sender: None,
                    receivers,
                    states,
                    agent_id: None,
                    result: None,
                }),
            },
        })
    }

    fn put_items(state: &mut Snapshot, owner: SessionRef, items: Vec<Arc<Item>>) {
        Arc::make_mut(&mut state.sessions.conversations).insert(
            owner,
            Arc::new(Thread {
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    items: Some(items),
                    ..Default::default()
                })]),
                ..Default::default()
            }),
        );
    }

    #[test]
    fn native_events_enrich_one_agent_without_reparenting_it() {
        let mut state = snapshot(serde_json::json!([
            {"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"root"},"name":"Review","status":"running"}
        ]));
        put_items(
            &mut state,
            session("root"),
            vec![item(
                "spawn",
                ItemStatus::Completed,
                "spawnAgent",
                vec![session("child")],
                vec![crate::models::SubagentState {
                    session: session("child"),
                    status: TurnStatus::Completed,
                    native_status: None,
                    message: Some("Finished review".into()),
                }],
            )],
        );
        put_items(
            &mut state,
            session("unrelated"),
            vec![item(
                "send",
                ItemStatus::Running,
                "sendInput",
                vec![session("child")],
                vec![],
            )],
        );
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].title, "Review");
        assert_eq!(agents[0].model.as_deref(), Some("review-model"));
        assert_eq!(agents[0].detail.as_deref(), Some("Finished review"));
        assert_eq!(agents[0].status, "実行中");
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), false);
        assert_eq!(state.agent_panel(state.panes.primary())[0].status, "完了");
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(session("unrelated"));
        assert!(state.agent_panel(state.panes.primary()).is_empty());
    }

    #[test]
    fn spawn_is_visible_before_list_refresh_but_wait_does_not_invent_agents() {
        let mut state = snapshot(serde_json::json!([]));
        put_items(
            &mut state,
            session("root"),
            vec![
                item(
                    "spawn",
                    ItemStatus::Completed,
                    "spawnAgent",
                    vec![session("child")],
                    vec![],
                ),
                item(
                    "wait",
                    ItemStatus::Running,
                    "wait",
                    vec![session("other")],
                    vec![],
                ),
                item("empty", ItemStatus::Running, "wait", vec![], vec![]),
                item(
                    "foreign",
                    ItemStatus::Completed,
                    "spawnAgent",
                    vec![SessionRef {
                        provider: Some(ProviderKind::Claude),
                        id: "child".into(),
                    }],
                    vec![],
                ),
            ],
        );
        Arc::make_mut(&mut state.lists.activity)
            .active
            .insert(session("child"), true);
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert!(agents[0].active);
    }

    #[test]
    fn claude_activity_updates_the_same_row_without_a_native_child_session() {
        let owner = SessionRef {
            provider: Some(ProviderKind::Claude),
            id: "root".into(),
        };
        let mut state = snapshot(serde_json::json!([]));
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
            Some(owner.clone());
        put_items(
            &mut state,
            owner.clone(),
            vec![item("task", ItemStatus::Running, "Task", vec![], vec![])],
        );
        let running = state.agent_panel(state.panes.primary());
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].title, "Check the change");
        assert!(running[0].active);
        put_items(
            &mut state,
            owner,
            vec![item("task", ItemStatus::Completed, "Task", vec![], vec![])],
        );
        let completed = state.agent_panel(state.panes.primary());
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].key, running[0].key);
        assert_eq!(completed[0].status, "完了");
        assert!(!completed[0].active);
        state.catalog.ui_language = UiLanguage::English;
        let english = state.agent_panel(state.panes.primary());
        assert_eq!(english[0].status, "Done");
        assert_eq!(english[0].title, "Check the change");
        assert_eq!(english[0].key, completed[0].key);
        assert!(state.thread_list().unwrap().threads.is_empty());
    }

    #[rstest::rstest]
    #[case(TurnStatus::Running, None, "実行中", true)]
    #[case(TurnStatus::Running, Some(false), "待機", false)]
    #[case(TurnStatus::Completed, None, "完了", false)]
    #[case(TurnStatus::Failed, None, "失敗", false)]
    #[case(TurnStatus::Interrupted, None, "停止", false)]
    #[case(TurnStatus::Unknown, None, "未確認", false)]
    #[case(TurnStatus::Unknown, Some(false), "待機", false)]
    #[case(TurnStatus::Completed, Some(true), "実行中", true)]
    fn status_uses_live_observations(
        #[case] status: TurnStatus,
        #[case] observed: Option<bool>,
        #[case] label: &str,
        #[case] active: bool,
    ) {
        assert_eq!(agent_status(status, observed), (label, active));
    }

    #[rstest::rstest]
    #[case(SessionStatus::Running, "実行中", true)]
    #[case(SessionStatus::Idle, "待機", false)]
    #[case(SessionStatus::Unavailable, "未確認", false)]
    #[case(SessionStatus::Unknown, "未確認", false)]
    fn native_status_is_visible_without_parent_events(
        #[case] status: SessionStatus,
        #[case] label: &str,
        #[case] active: bool,
    ) {
        let mut state = snapshot(serde_json::json!([]));
        Arc::make_mut(&mut state.sessions.conversations).insert(
            session("child"),
            Arc::new(Thread {
                id: Some(session("child")),
                parent_id: Some(session("root")),
                status,
                ..Default::default()
            }),
        );
        let agents = state.agent_panel(state.panes.primary());
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, label);
        assert_eq!(agents[0].active, active);
    }

    proptest! {
        #[test]
        fn arbitrary_native_graphs_show_only_unique_reachable_agents(parents in prop::collection::vec(0usize..25, 1..25)) {
            let mut state = snapshot(serde_json::json!([]));
            Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id = Some(session("0"));
            state.sessions.conversations = Arc::new(parents.iter().enumerate().map(|(index,parent)| {
                let id = session(&index.to_string());
                (id.clone(), Arc::new(Thread {
                    id:Some(id), parent_id:Some(session(&parent.to_string())), ..Default::default()
                }))
            }).collect());
            let rows = state.agent_panel(state.panes.primary());
            let expected:HashSet<_> = (1..parents.len()).filter(|&index| {
                let mut cursor=index;
                let mut visited=HashSet::new();
                while cursor != 0 {
                    if cursor >= parents.len() || !visited.insert(cursor) { return false; }
                    cursor=parents[cursor];
                }
                true
            }).map(|index| format!("session:{}", session(&index.to_string()))).collect();
            let actual:HashSet<_> = rows.iter().map(|row| row.key.clone()).collect();
            prop_assert_eq!(rows.len(),actual.len());
            prop_assert_eq!(actual,expected);
        }
    }
}
