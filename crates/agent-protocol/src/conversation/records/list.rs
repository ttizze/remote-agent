use super::*;
use crate::sync::{Content, Key};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeList {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
    pub subscription: uuid::Uuid,
    pub cached: Option<crate::sync::Cursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedList {
    #[serde(skip)]
    pub subscription_id: uuid::Uuid,
    pub initial: Message,
    pub provider_errors: serde_json::Map<String, serde_json::Value>,
}

pub fn validate(message: &Message) -> Result<(), crate::sync::ApplyError> {
    validate_message(message)?;
    let rows = match message {
        Message::Snapshot {
            window: None, rows, ..
        } => rows,
        Message::Changes { rows, removed, .. } => {
            if removed.iter().any(|row| {
                !matches!(
                    row.key,
                    crate::sync::Key::Thread(_)
                        | crate::sync::Key::Project(_)
                        | crate::sync::Key::Catalog(_)
                )
            }) {
                return Err(crate::sync::ApplyError::Identity);
            }
            rows
        }
        _ => return Err(crate::sync::ApplyError::Window),
    };
    if rows.iter().any(|row| {
        !matches!(
            row.key,
            crate::sync::Key::Thread(_)
                | crate::sync::Key::Project(_)
                | crate::sync::Key::Catalog(_)
        )
    }) {
        return Err(crate::sync::ApplyError::Identity);
    }
    Ok(())
}

impl crate::operations::RpcMethod for SubscribeList {
    crate::operations::rpc_contract!(SubscribeList);
    fn params(&self) -> Result<Self, crate::error::PeerError> {
        Ok(self.clone())
    }
    fn subscription(output: &mut Self::Output, id: uuid::Uuid) {
        output.subscription_id = id;
    }
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        validate(&output.initial).map_err(|_| "invalid list snapshot")
    }
}

/// All system surfaces derive their compact facts from the same saved headers.
pub fn activity<'a>(
    headers: impl IntoIterator<Item = (&'a SessionRef, &'a Header)>,
) -> crate::live_activity::TaskActivityState {
    use crate::{
        live_activity::{TaskActivityState, TaskActivitySummary},
        models::SessionStatus,
    };
    let mut state = TaskActivityState {
        display: TaskActivitySummary::default().display(),
        statuses: Vec::new(),
        waiting: Vec::new(),
        background: Vec::new(),
        unread: Vec::new(),
        attention: Vec::new(),
    };
    let mut phases = Vec::new();
    for (id, header) in headers {
        if header.details.parent.is_some() {
            continue;
        }
        let facts = header.details.activity.facts();
        phases.push(facts.phase().0);
        state.statuses.push((id.clone(), facts.session_status()));
        if facts.waiting {
            state.waiting.push(id.clone());
        }
        if let Some(background) = facts.background {
            state.background.push((id.clone(), background));
        }
        if let Some(attention) = facts.attention() {
            state.attention.push((id.clone(), attention));
        }
        if facts.session_status() == SessionStatus::Idle
            && let Some(completion) = &header.details.activity.completion
            && header.details.read_through.as_ref() != Some(completion)
        {
            state.unread.push((id.clone(), completion.clone()));
        }
    }
    let mut summary = TaskActivitySummary::from_statuses(phases);
    summary.finished = state.unread.len().try_into().unwrap_or(u32::MAX);
    state.display = summary.display();
    state
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchList {
    pub term: String,
    pub limit: Option<u32>,
}

/// A failed provider has settled too: its error remains visible beside healthy rows.
pub fn catalog_ready(replica: &Replica) -> bool {
    !replica.rows().values().any(|row| {
        let (crate::sync::Content::Full(record) | crate::sync::Content::Summary(record)) =
            &row.content;
        matches!(record.as_ref(), Record::Catalog(Catalog::Loading))
    })
}

/// Derive activity directly from the canonical list rows.
pub fn rows_activity<'a>(
    rows: impl Iterator<Item = &'a Row>,
) -> crate::live_activity::TaskActivityState {
    activity(rows.filter_map(|row| {
        let record = match &row.content {
            Content::Full(record) | Content::Summary(record) => record,
        };
        match (&row.key, record.as_ref()) {
            (Key::Thread(id), Record::Thread(header)) => Some((id, header)),
            _ => None,
        }
    }))
}
