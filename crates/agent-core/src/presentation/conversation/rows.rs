//! Pane-owned expansion choices are immutable inputs to shared row projection.
use super::{ConversationRow, ConversationRowContent, RenderedConversation, WorkProgress};
use crate::panels::PanelScope;
use agent_protocol::{ids::TurnId, session::ProviderKind};
use std::{collections::HashMap, sync::Arc};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProgressRows {
    Always,
    Hidden,
    UnlessHeader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationRowPolicy {
    pub requests: bool,
    pub progress: ProgressRows,
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
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConversationRows {
    source: Arc<RenderedConversation>,
    scope: PanelScope,
    expansion: HashMap<String, String>,
    policy: ConversationRowPolicy,
    pub rows: Vec<Arc<VisibleConversationRow>>,
    turn_end: usize,
    queued_end: usize,
    source_item_count: u32,
}

type PreviousRows<'a> = HashMap<(&'static str, &'a str), &'a Arc<VisibleConversationRow>>;

/// Preparation stays immediately after the first user input when provider
/// activity arrives. An admission still shown among queued input keeps its
/// session row; queued_rows continues to contain only user messages.
pub fn position_workspace_setup<T>(
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
        source: Arc<RenderedConversation>,
        scope: PanelScope,
        expansion: HashMap<String, String>,
        policy: ConversationRowPolicy,
        previous: Option<Arc<Self>>,
    ) -> Arc<Self> {
        if let Some(previous) = &previous
            && Arc::ptr_eq(&source, &previous.source)
            && scope == previous.scope
            && expansion == previous.expansion
            && policy == previous.policy
        {
            return previous.clone();
        }
        let before: PreviousRows<'_> = previous
            .as_ref()
            .filter(|previous| previous.scope == scope)
            .into_iter()
            .flat_map(|previous| &previous.rows)
            .map(|row| ((row.origin.label(), row.data.id.as_str()), row))
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
            let origin = ConversationRowOrigin::Turn {
                key: turn.key(),
                id: turn.source.id.clone(),
                provider: turn
                    .source
                    .provider
                    .or_else(|| source.source.id.as_ref().and_then(|id| id.provider)),
                started_at: turn.source.started_at,
            };
            rows.extend(project_rows(
                &turn.rows,
                &origin,
                &prefix,
                &expansion,
                policy,
                &turn.work_progress(true),
                &before,
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
                edit_turn_id: None,
            };
            let row = project_row(
                &prefix,
                &ConversationRowOrigin::Queued,
                &data,
                true,
                false,
                None,
                before.get(&("queued", data.id.as_str())).copied(),
            );
            rows.push(row);
        }
        let mut queued_end = rows.len();
        rows.extend(project_rows(
            &source.session_rows,
            &ConversationRowOrigin::Session,
            &prefix,
            &expansion,
            policy,
            &WorkProgress {
                started_at: None,
                action: None,
            },
            &before,
        ));
        if position_workspace_setup(
            &mut rows,
            turn_end,
            |row| matches!(row.data.content, ConversationRowContent::User { .. }),
            |row| matches!(&row.data.content, ConversationRowContent::Preparation { preparation } if !preparation.steps.is_empty()),
        ) {
            turn_end += 1;
            queued_end += 1;
        }
        Arc::new(Self {
            source,
            scope,
            expansion,
            policy,
            rows,
            turn_end,
            queued_end,
            source_item_count,
        })
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
    pub fn toggle_activity(
        &self,
        id: String,
        mut expansion: HashMap<String, String>,
    ) -> Option<HashMap<String, String>> {
        let row = self.rows.iter().find(|row| row.id == id)?;
        let ConversationRowContent::ActivityHeader { activity } = &row.data.content else {
            return None;
        };
        if !activity.activity_can_collapse {
            return None;
        }
        // The owner may have accepted another click before publishing a new frame.
        let expanded = super::activity_is_expanded(&activity.status, expansion.get(&id).cloned());
        if expanded {
            expansion.remove(&id);
        } else {
            expansion.insert(id, activity.status.clone());
        }
        Some(expansion)
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
}

fn project_rows(
    source: &[ConversationRow],
    origin: &ConversationRowOrigin,
    prefix: &str,
    expansion: &HashMap<String, String>,
    policy: ConversationRowPolicy,
    progress: &WorkProgress,
    previous: &PreviousRows<'_>,
) -> Vec<Arc<VisibleConversationRow>> {
    let mut expanded = false;
    let mut progress_shown = false;
    let inputs: Vec<_> = source
        .iter()
        .filter_map(|data| {
            let mut header_expanded = false;
            let mut row_progress = None;
            match &data.content {
                ConversationRowContent::ActivityHeader { activity } => {
                    let id = format!("{prefix}:{}:{}", origin.label(), data.id);
                    expanded =
                        super::activity_is_expanded(&activity.status, expansion.get(&id).cloned());
                    header_expanded = expanded;
                    if activity.is_in_progress {
                        row_progress = Some(WorkProgress {
                            started_at: progress.started_at,
                            action: (!expanded).then(|| progress.action.clone()).flatten(),
                        });
                    }
                }
                ConversationRowContent::InProgress { .. } => {
                    row_progress = Some(WorkProgress {
                        started_at: progress.started_at,
                        action: None,
                    });
                }
                _ => (),
            }
            let visible = match &data.content {
                ConversationRowContent::Activity { .. } => expanded,
                ConversationRowContent::PendingRequest { .. } => policy.requests,
                ConversationRowContent::InProgress { .. } => match policy.progress {
                    ProgressRows::Always => true,
                    ProgressRows::Hidden => false,
                    ProgressRows::UnlessHeader => !progress_shown,
                },
                _ => true,
            };
            if visible {
                progress_shown = matches!(&data.content,
                ConversationRowContent::ActivityHeader { activity } if activity.is_in_progress);
            }
            // Hidden source rows remain in RenderedConversation; do not allocate
            // native render values until the owner expands their activity.
            visible.then_some(RowInput {
                data,
                expanded: header_expanded,
                progress: row_progress,
            })
        })
        .collect();
    let last = inputs.len().checked_sub(1);
    inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            project_row(
                prefix,
                origin,
                input.data,
                Some(index) == last,
                input.expanded,
                input.progress,
                previous
                    .get(&(origin.label(), input.data.id.as_str()))
                    .copied(),
            )
        })
        .collect()
}

fn project_row(
    prefix: &str,
    origin: &ConversationRowOrigin,
    data: &ConversationRow,
    last: bool,
    expanded: bool,
    progress: Option<WorkProgress>,
    previous: Option<&Arc<VisibleConversationRow>>,
) -> Arc<VisibleConversationRow> {
    if let Some(previous) = previous
        && previous.origin == *origin
        && previous.data == *data
        && previous.last == last
        && previous.expanded == expanded
        && previous.progress == progress
    {
        return previous.clone();
    }
    Arc::new(VisibleConversationRow {
        id: format!("{prefix}:{}:{}", origin.label(), data.id),
        data: data.clone(),
        origin: origin.clone(),
        last,
        expanded,
        progress,
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
