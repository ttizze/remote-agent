//! Pane-owned expansion choices are immutable inputs to shared row projection.
use super::{ConversationRow, ConversationRowContent, RenderedConversation, TurnRow, WorkProgress};
use crate::panels::PanelScope;
use agent_protocol::{ids::TurnId, session::ProviderKind};
use std::{collections::HashMap, sync::Arc};

mod search;
#[cfg(test)]
mod steer_tests;
#[cfg(test)]
mod tests;
mod timeline;
use timeline::project_timeline;

/// The owner publishes the next expansion choices and performs any requested load.
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationExpansionChange {
    pub expansion: HashMap<String, String>,
    pub load: Option<crate::state::Intent>,
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationRowOrigin {
    Turn {
        key: String,
        id: TurnId,
        provider: Option<ProviderKind>,
        started_at: Option<f64>,
    },
    Queued,
    Session,
}
impl ConversationRowOrigin {
    fn label(&self) -> &'static str {
        match self {
            Self::Turn { .. } => "turn",
            Self::Queued => "queued",
            Self::Session => "session",
        }
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
#[cfg_attr(feature = "bindings", uniffi::export(Eq, Hash))]
pub struct VisibleConversationRow {
    pub id: String,
    pub data: ConversationRow,
    pub origin: ConversationRowOrigin,
    pub last: bool,
    pub expanded: bool,
    pub progress: Option<WorkProgress>,
    timeline: Option<TurnRow>,
}

// Native wrappers can be recreated while the immutable Rust row is reused.
impl PartialEq for VisibleConversationRow {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for VisibleConversationRow {}
impl std::hash::Hash for VisibleConversationRow {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(self, state);
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl VisibleConversationRow {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    pub fn data(&self) -> ConversationRow {
        self.data.clone()
    }
    pub fn origin(&self) -> ConversationRowOrigin {
        self.origin.clone()
    }
    pub fn expanded(&self) -> bool {
        self.expanded
    }
    pub fn timeline(&self) -> Option<TurnRow> {
        self.timeline.clone()
    }
    pub fn progress_label(
        &self,
        language: crate::localization::UiLanguage,
        now_seconds: f64,
    ) -> Option<String> {
        self.progress
            .as_ref()
            .map(|progress| progress.label(language, now_seconds))
    }

    pub fn progress_is_compacting(&self) -> bool {
        self.progress
            .as_ref()
            .is_some_and(WorkProgress::is_compacting)
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConversationRows {
    style: super::TimelineStyle,
    source: Arc<RenderedConversation>,
    queue: Option<Arc<crate::presentation::queue::QueuePresentation>>,
    scope: PanelScope,
    expansion: HashMap<String, String>,
    pub rows: Vec<Arc<VisibleConversationRow>>,
    turn_end: usize,
    queued_end: usize,
    source_item_count: u32,
}

type PreviousRows<'a> = HashMap<&'a str, &'a Arc<VisibleConversationRow>>;

fn turn_origin(
    key: String,
    id: TurnId,
    provider: Option<ProviderKind>,
    parent_provider: Option<ProviderKind>,
    started_at: Option<f64>,
) -> ConversationRowOrigin {
    ConversationRowOrigin::Turn {
        key,
        id,
        provider: provider.or(parent_provider),
        started_at,
    }
}

/// Preparation stays immediately after the first user input when provider
/// activity arrives. An admission still shown among queued input keeps its
/// session row; queued_rows continues to contain only user messages.
fn position_workspace_setup<T>(
    rows: &mut Vec<T>,
    native_end: usize,
    is_user: impl Fn(&T) -> bool,
    is_setup: impl Fn(&T) -> bool,
) -> bool {
    let Some(after_user) = rows[..native_end]
        .iter()
        .position(is_user)
        .map(|index| index + 1)
    else {
        return false;
    };
    let Some(from) = rows.iter().position(is_setup) else {
        return false;
    };
    if from < native_end {
        return false;
    }
    let setup = rows.remove(from);
    rows.insert(after_user, setup);
    true
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConversationRows {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn project(
        style: super::TimelineStyle,
        source: Arc<RenderedConversation>,
        scope: PanelScope,
        queue: Option<Arc<crate::presentation::queue::QueuePresentation>>,
        mut expansion: HashMap<String, String>,
        previous: Option<Arc<Self>>,
    ) -> Arc<Self> {
        let queue = queue.filter(|queue| {
            !queue.messages.is_empty()
                && queue.session.is_some()
                && queue.session == source.source.id
        });
        if let Some(previous) = &previous
            && Arc::ptr_eq(&source, &previous.source)
            && style == previous.style
            && queue == previous.queue
            && scope == previous.scope
            && expansion == previous.expansion
        {
            return previous.clone();
        }
        let before: PreviousRows<'_> = previous
            .as_ref()
            .filter(|previous| previous.scope == scope)
            .into_iter()
            .flat_map(|previous| &previous.rows)
            .map(|row| (row.id.as_str(), row))
            .collect();
        let prefix = serde_json::to_string(&(&scope.host, &scope.storage, &scope.draft))
            .expect("conversation scope contains strings");
        let mut rows = Vec::new();
        let source_item_count = source
            .turns
            .iter()
            .flat_map(|turn| &turn.rows)
            .chain(&source.session_rows)
            .filter(|row| is_source_item(&row.content))
            .count() as u32;
        for turn in &source.turns {
            let origin = turn_origin(
                turn.key(),
                turn.source.id.clone(),
                turn.source.provider,
                source.source.id.as_ref().and_then(|id| id.provider),
                turn.source.started_at,
            );
            rows.extend(project_timeline(
                style, turn, &origin, &prefix, &expansion, &before,
            ));
        }
        let mut turn_end = rows.len();
        for item in &source.queued {
            let data = ConversationRow {
                id: item.data.id.clone(),
                content: ConversationRowContent::User { item: item.clone() },
                timestamp_ms: item.data.timestamp_ms,
                steer: false,
                queued: false,
                fresh_context: false,
                edit_turn_id: None,
            };
            let row = project_row(
                &prefix,
                &ConversationRowOrigin::Queued,
                RowInput {
                    data: &data,
                    expanded: false,
                    progress: None,
                    timeline: None,
                },
                true,
                &before,
            );
            rows.push(row);
        }
        let mut queued_end = rows.len();
        rows.extend(project_session_rows(
            style,
            &source.session_rows,
            &prefix,
            queue.is_none(),
            &before,
        ));
        if let Some(queue) = &queue {
            let row = project_queue_row(queue, &prefix, &expansion, &before);
            if row.expanded {
                expansion.insert(row.id.clone(), "open".into());
            }
            rows.push(row);
        }
        if position_workspace_setup(
            &mut rows,
            turn_end,
            |row| matches!(row.data.content, ConversationRowContent::User { .. }),
            |row| matches!(&row.data.content, ConversationRowContent::Preparation { preparation } if !preparation.steps.is_empty()),
        ) {
            turn_end += 1;
            queued_end += 1;
        }
        let queue_key = rows
            .last()
            .filter(|row| matches!(row.data.content, ConversationRowContent::Queue { .. }))
            .map(|row| &row.id);
        let queue_prefix = row_id(&prefix, &ConversationRowOrigin::Session, "queued-messages:");
        expansion.retain(|key, _| !key.starts_with(&queue_prefix) || Some(key) == queue_key);
        Arc::new(Self {
            style,
            source,
            queue,
            scope,
            expansion,
            rows,
            turn_end,
            queued_end,
            source_item_count,
        })
    }

    /// Use actual mounted controls to open the source's outer fold, then its group on the next projection.
    pub fn citation_reveal(
        &self,
        citation: agent_protocol::context::AssistantCitation,
        mut expansion: HashMap<String, String>,
    ) -> Option<HashMap<String, String>> {
        let plan = self.source.citation_expansion(
            self.scope.host.clone(),
            self.scope.storage.clone(),
            citation,
        )?;
        for row in &self.rows {
            if !matches!(&row.origin, ConversationRowOrigin::Turn { key, .. } if key == &plan.turn_key)
            {
                continue;
            }
            if let Some(activity) = &plan.activity
                && row.data.id == activity.id
            {
                expansion.insert(row.id.clone(), activity.status.clone());
            }
            if plan.groups.contains(&row.data.id) {
                expansion.insert(row.id.clone(), "open".into());
            }
        }
        Some(expansion)
    }

    pub fn citation_row(
        &self,
        citation: agent_protocol::context::AssistantCitation,
    ) -> Option<String> {
        let raw = crate::composer::context::navigation::citation_row(
            &self.source,
            &self.scope,
            &citation,
        )?;
        self.rows
            .iter()
            .find(|row| row.data.id == raw)
            .map(|row| row.id.clone())
    }

    pub fn rows(&self) -> Vec<Arc<VisibleConversationRow>> {
        self.rows.clone()
    }
    pub fn source(&self) -> Arc<RenderedConversation> {
        self.source.clone()
    }
    pub fn scope(&self) -> PanelScope {
        self.scope.clone()
    }
    /// The pane owns choices; projection retires only queue episodes it no longer renders.
    /// Publish these choices together with the frame before projecting again.
    pub fn expansion(&self) -> HashMap<String, String> {
        self.expansion.clone()
    }

    /// Hidden work remains searchable with the same identities as its eventual rows.
    pub fn searchable_rows(&self) -> Vec<Arc<VisibleConversationRow>> {
        search::messages(self.style, &self.source, &self.scope)
            .into_iter()
            .map(|(row, _)| row)
            .collect()
    }

    pub fn reveal_message(
        &self,
        id: String,
        mut expansion: HashMap<String, String>,
    ) -> Option<ConversationExpansionChange> {
        let (_, choices) = search::messages(self.style, &self.source, &self.scope)
            .into_iter()
            .find(|(row, _)| row.id == id)?;
        expansion.extend(choices);
        Some(ConversationExpansionChange {
            expansion,
            load: None,
        })
    }
    pub fn turn_rows(&self) -> Vec<Arc<VisibleConversationRow>> {
        self.rows[..self.turn_end].to_vec()
    }
    pub fn queued_rows(&self) -> Vec<Arc<VisibleConversationRow>> {
        self.rows[self.turn_end..self.queued_end].to_vec()
    }
    pub fn session_rows(&self) -> Vec<Arc<VisibleConversationRow>> {
        self.rows[self.queued_end..].to_vec()
    }
    /// Return the native owner's next expansion choice without changing the frame.
    pub fn toggle_disclosure(
        &self,
        id: String,
        mut expansion: HashMap<String, String>,
    ) -> Option<ConversationExpansionChange> {
        let row = self.rows.iter().find(|row| row.id == id)?;
        let status = match &row.timeline {
            Some(TurnRow::Group { .. } | TurnRow::Live { .. } | TurnRow::Subagents { .. }) => {
                "open".to_owned()
            }
            _ if matches!(&row.data.content, ConversationRowContent::Queue { queue } if queue.can_expand()) => {
                "open".to_owned()
            }
            _ => {
                let ConversationRowContent::ActivityHeader { activity } = &row.data.content else {
                    return None;
                };
                if !activity.activity_can_collapse {
                    return None;
                }
                activity.status.clone()
            }
        };
        // The owner may have accepted another click before publishing a new frame.
        let expanded = super::activity_is_expanded(&status, expansion.get(&id).cloned());
        if expanded {
            expansion.remove(&id);
        } else {
            expansion.insert(id.clone(), status);
        }
        let load = (!expanded).then(|| self.activity_load(id)).flatten();
        Some(ConversationExpansionChange { expansion, load })
    }

    /// Retry only a deferred attempt addressed by a currently visible scoped control.
    pub fn activity_load(&self, id: String) -> Option<crate::state::Intent> {
        let row = self.rows.iter().find(|row| row.id == id)?;
        let Some(TurnRow::Group { attempt, .. }) = &row.timeline else {
            return None;
        };
        super::attempts::group_load(&self.source.source, *attempt)
    }
    /// Accessibility counts the unfiltered source, independently of presentation policy.
    pub fn source_item_count(&self) -> u32 {
        self.source_item_count
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
}

struct RowInput<'a> {
    data: &'a ConversationRow,
    expanded: bool,
    progress: Option<WorkProgress>,
    timeline: Option<TurnRow>,
}

// Session rows are requests or preparations outside a provider turn. Turn
// visibility, grouping and progress always come from the shared T3 timeline.
fn project_session_rows(
    style: super::TimelineStyle,
    source: &[ConversationRow],
    prefix: &str,
    last_section: bool,
    previous: &PreviousRows<'_>,
) -> Vec<Arc<VisibleConversationRow>> {
    let visible: Vec<_> = source
        .iter()
        .filter(|row| style.background_footer || !matches!(&row.content, ConversationRowContent::Background { .. }))
        .filter(|row| !matches!(&row.content, ConversationRowContent::PendingRequest { request } if request.approval.is_none()))
        .collect();
    let last = visible.len().checked_sub(1);
    visible
        .into_iter()
        .enumerate()
        .map(|(index, data)| {
            project_row(
                prefix,
                &ConversationRowOrigin::Session,
                RowInput {
                    data,
                    expanded: false,
                    progress: None,
                    timeline: None,
                },
                last_section && Some(index) == last,
                previous,
            )
        })
        .collect()
}

fn project_queue_row(
    queue: &Arc<crate::presentation::queue::QueuePresentation>,
    prefix: &str,
    expansion: &HashMap<String, String>,
    previous: &PreviousRows<'_>,
) -> Arc<VisibleConversationRow> {
    // A continuous queue retains its disclosure across item delivery and cell
    // reuse. After an observed empty queue, a new first input starts a new ID.
    let prior = previous.values().find_map(|row| {
        matches!(&row.data.content, ConversationRowContent::Queue { queue: old } if old.session == queue.session)
            .then_some(*row)
    });
    let id = prior
        .map(|row| row.data.id.clone())
        .unwrap_or_else(|| format!("queued-messages:{}", queue.messages[0].id));
    let previous_edit = prior.and_then(|row| match &row.data.content {
        ConversationRowContent::Queue { queue } => queue.editing.as_ref(),
        _ => None,
    });
    let expanded = queue.can_expand()
        && (queue.reveal_edit(previous_edit)
            || expansion
                .get(&row_id(prefix, &ConversationRowOrigin::Session, &id))
                .is_some_and(|value| value == "open"));
    let data = ConversationRow {
        id,
        timestamp_ms: None,
        steer: false,
        queued: false,
        fresh_context: false,
        edit_turn_id: None,
        content: ConversationRowContent::Queue {
            queue: queue.clone(),
        },
    };
    project_row(
        prefix,
        &ConversationRowOrigin::Session,
        RowInput {
            data: &data,
            expanded,
            progress: None,
            timeline: None,
        },
        true,
        previous,
    )
}

fn row_id(prefix: &str, origin: &ConversationRowOrigin, id: &str) -> String {
    format!("{prefix}:{}:{id}", origin.label())
}

fn project_row(
    prefix: &str,
    origin: &ConversationRowOrigin,
    input: RowInput<'_>,
    last: bool,
    previous: &PreviousRows<'_>,
) -> Arc<VisibleConversationRow> {
    let RowInput {
        data,
        expanded,
        progress,
        timeline,
    } = input;
    let id = row_id(prefix, origin, &data.id);
    if let Some(previous) = previous.get(id.as_str()).copied()
        && previous.origin == *origin
        && previous.data == *data
        && previous.last == last
        && previous.expanded == expanded
        && previous.progress == progress
        && previous.timeline == timeline
    {
        return previous.clone();
    }
    Arc::new(VisibleConversationRow {
        id,
        data: data.clone(),
        origin: origin.clone(),
        last,
        expanded,
        progress,
        timeline,
    })
}

fn is_source_item(content: &ConversationRowContent) -> bool {
    matches!(
        content,
        ConversationRowContent::User { .. }
            | ConversationRowContent::Activity { .. }
            | ConversationRowContent::Response { .. }
            | ConversationRowContent::PendingRequest { .. }
            | ConversationRowContent::Error { .. }
    )
}
