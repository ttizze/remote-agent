//! Read the previous database once, with the Host lease held. The destination
//! schema, converted rows and version commit together; originals stay untouched.
use super::*;
use agent_protocol::{
    conversation::{
        Attempt, InputContent, InputState, PendingDelivery, PendingInput, UnknownState,
    },
    models::{ThreadResponse, Turn},
    operations::{InputQueue, Submission},
    session::{PreparationStatus, SessionPreparation},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
};

const FILES: [&str; 6] = [
    "bex-conversations.json",
    "bex-read-state.json",
    "bex-model-selections.json",
    "bex-conversations.rewind",
    "bex-projects.json",
    agent_local_host::update::RECOVERY_FILE,
];

#[derive(Deserialize)]
struct Binding {
    session: SessionRef,
    covered: Option<usize>,
}
#[derive(Deserialize)]
struct Handoff {
    session: SessionRef,
    delivery: String,
    submission: agent_protocol::session::SubmissionDelivery,
    input_id: ClientInputId,
    receipt: Option<agent_protocol::operations::SubmissionReceipt>,
}
#[derive(Deserialize)]
struct Startup {
    project_cwd: Option<String>,
    workspace: Option<agent_protocol::workspace::WorkspaceStrategy>,
    preparation: SessionPreparation,
}
#[derive(Deserialize)]
struct Conversation {
    id: SessionRef,
    active: SessionRef,
    cwd: String,
    name: Option<String>,
    preview: Option<String>,
    bindings: Vec<Binding>,
    handoffs: Vec<Handoff>,
    startup: Option<Startup>,
    workspace_setup: Option<agent_protocol::workspace::WorkspaceSetup>,
}
#[derive(Deserialize)]
struct Read {
    session: SessionRef,
    turn_id: TurnId,
    unread: bool,
}
#[derive(Deserialize)]
struct Selection {
    session: SessionRef,
    #[serde(flatten)]
    options: decision::records::Selection,
}
#[derive(Deserialize)]
struct Rewind {
    previous: Option<Conversation>,
    history: ThreadResponse,
}

pub(super) struct Previous {
    history: BTreeMap<SessionRef, ThreadResponse>,
    unimported: BTreeSet<SessionRef>,
    conversations: Vec<Conversation>,
    queues: BTreeMap<SessionRef, InputQueue>,
    reads: Vec<Read>,
    models: Vec<Selection>,
    projects: Vec<agent_protocol::models::Project>,
    workspace_scripts: BTreeMap<String, Vec<agent_protocol::models::ProjectScript>>,
    backup: tempfile::TempDir,
    recovery_pending: bool,
}

fn file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failure(error)),
    }
}

fn table(connection: &Connection, name: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(failure)
}

fn state<T: serde::de::DeserializeOwned>(
    connection: Option<&Connection>,
    path: &Path,
    name: &str,
    filename: &str,
) -> Result<Vec<T>, String> {
    if let Some(connection) = connection
        && table(connection, "host_state")?
    {
        let body: Option<Vec<u8>> = connection
            .query_row("SELECT body FROM host_state WHERE name=?1", [name], |row| {
                row.get(0)
            })
            .optional()
            .map_err(failure)?;
        if let Some(body) = body {
            return serde_json::from_slice(&body).map_err(failure);
        }
    }
    Ok(file(&path.with_file_name(filename))?.unwrap_or_default())
}

fn read_workspace_scripts(
    connection: Option<&Connection>,
) -> Result<BTreeMap<String, Vec<agent_protocol::models::ProjectScript>>, String> {
    let mut workspace_scripts = BTreeMap::new();
    if let Some(connection) = connection
        && table(connection, "host_state")?
    {
        let mut statement = connection
            .prepare("SELECT name,body FROM host_state WHERE name LIKE 'project-scripts:%'")
            .map_err(failure)?;
        let mut rows = statement.query([]).map_err(failure)?;
        while let Some(row) = rows.next().map_err(failure)? {
            let name: String = row.get(0).map_err(failure)?;
            let body: Vec<u8> = row.get(1).map_err(failure)?;
            workspace_scripts.insert(name, serde_json::from_slice(&body).map_err(failure)?);
        }
    }
    Ok(workspace_scripts)
}

pub(super) fn prepare(path: &Path) -> Result<Option<Previous>, String> {
    let source = path.with_file_name("bex-transcripts.sqlite");
    if !source.exists() && !FILES.iter().any(|name| path.with_file_name(name).exists()) {
        return Ok(None);
    }
    let connection = source
        .exists()
        .then(|| {
            Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(failure)
        })
        .transpose()?;
    let mut history = BTreeMap::new();
    let mut unimported = BTreeSet::new();
    let mut queues = BTreeMap::new();
    if let Some(connection) = &connection {
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(failure)?;
        if version > 1 {
            return Err(format!(
                "unsupported previous conversation database version: {version}"
            ));
        }
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(failure)?;
        if integrity != "ok" {
            return Err("previous conversation database failed integrity check".into());
        }
        if table(connection, "conversations")? {
            let mut statement = connection
                .prepare("SELECT provider,id,cwd,metadata FROM conversations")
                .map_err(failure)?;
            let mut entries = statement.query([]).map_err(failure)?;
            while let Some(entry) = entries.next().map_err(failure)? {
                let kind: String = entry.get(0).map_err(failure)?;
                let id = SessionRef::new(provider(&kind)?, entry.get(1).map_err(failure)?)
                    .map_err(str::to_owned)?;
                let body: Option<Vec<u8>> = entry.get(3).map_err(failure)?;
                if body.is_none() {
                    unimported.insert(id.clone());
                }
                let mut response: ThreadResponse = body
                    .map(|body| protocol::decode(&body).map_err(failure))
                    .transpose()?
                    .unwrap_or_default();
                response.thread.id = Some(id.clone());
                response.thread.cwd = response.thread.cwd.or(entry.get(2).map_err(failure)?);
                history.insert(id, response);
            }
        }
        if table(connection, "turns")? {
            let mut statement = connection
                .prepare("SELECT provider,id,body FROM turns ORDER BY provider,id,position")
                .map_err(failure)?;
            let mut entries = statement.query([]).map_err(failure)?;
            while let Some(entry) = entries.next().map_err(failure)? {
                let kind: String = entry.get(0).map_err(failure)?;
                let id = SessionRef::new(provider(&kind)?, entry.get(1).map_err(failure)?)
                    .map_err(str::to_owned)?;
                let body: Vec<u8> = entry.get(2).map_err(failure)?;
                let turn: Turn = protocol::decode(&body).map_err(failure)?;
                let response = history.entry(id.clone()).or_default();
                response.thread.id = Some(id);
                response
                    .thread
                    .turns
                    .get_or_insert_default()
                    .push(Arc::new(turn));
            }
        }
        if table(connection, "queues")? {
            let mut statement = connection
                .prepare("SELECT provider,id,body FROM queues")
                .map_err(failure)?;
            let mut entries = statement.query([]).map_err(failure)?;
            while let Some(entry) = entries.next().map_err(failure)? {
                let kind: String = entry.get(0).map_err(failure)?;
                let id = SessionRef::new(provider(&kind)?, entry.get(1).map_err(failure)?)
                    .map_err(str::to_owned)?;
                let body: Vec<u8> = entry.get(2).map_err(failure)?;
                queues.insert(id, protocol::decode(&body).map_err(failure)?);
            }
        }
    }
    let mut conversations: Vec<Conversation> =
        state(connection.as_ref(), path, "bindings", FILES[0])?;
    let committed_state = connection
        .as_ref()
        .map(|connection| table(connection, "host_state"))
        .transpose()?
        .unwrap_or(false);
    if !committed_state && let Some(rewind) = file::<Rewind>(&path.with_file_name(FILES[3]))? {
        let id = rewind
            .history
            .thread
            .id
            .clone()
            .ok_or("previous rewind has no identity")?;
        conversations.retain(|row| row.id != id);
        if let Some(previous) = rewind.previous {
            if previous.id != id {
                return Err("previous rewind identity is inconsistent".into());
            }
            conversations.push(previous);
        }
        unimported.remove(&id);
        history.insert(id, rewind.history);
    }
    let reads = state(connection.as_ref(), path, "reads", FILES[1])?;
    let models = state(connection.as_ref(), path, "models", FILES[2])?;
    let mut projects: Vec<agent_protocol::models::Project> =
        file(&path.with_file_name(FILES[4]))?.unwrap_or_default();
    let reserved = crate::projects::chat_directories(path).map_err(failure)?;
    for project in &mut projects {
        project.roots.retain(|root| {
            let path = std::fs::canonicalize(&root.path)
                .unwrap_or_else(|_| std::path::PathBuf::from(&root.path));
            !reserved.contains(dunce::simplified(&path))
        });
    }
    projects.retain(|project| !project.roots.is_empty());
    let workspace_scripts = read_workspace_scripts(connection.as_ref())?;
    let backup = backup(path, connection.as_ref())?;
    Ok(Some(Previous {
        history,
        unimported,
        conversations,
        queues,
        reads,
        models,
        projects,
        workspace_scripts,
        backup,
        recovery_pending: file::<agent_local_host::update::Recovery>(
            &path.with_file_name(agent_local_host::update::RECOVERY_FILE),
        )?
        .is_some_and(|report| {
            report.tasks.iter().any(|task| {
                task.delivery == agent_local_host::update::ContinuationDelivery::Pending
            })
        }),
    }))
}

/// Retain an independently durable copy before the destination transaction starts.
fn backup(path: &Path, connection: Option<&Connection>) -> Result<tempfile::TempDir, String> {
    let parent = path.parent().ok_or("missing storage directory")?;
    let backup = tempfile::Builder::new()
        .prefix("before-conversation-rows-")
        .tempdir_in(parent)
        .map_err(failure)?;
    if let Some(connection) = connection {
        let destination = backup.path().join("bex-transcripts.sqlite");
        connection
            .execute(
                "VACUUM INTO ?1",
                [destination.to_str().ok_or("invalid backup path")?],
            )
            .map_err(failure)?;
    }
    for name in FILES {
        match fs::copy(path.with_file_name(name), backup.path().join(name)) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(failure(error)),
        }
    }
    for entry in fs::read_dir(backup.path()).map_err(failure)? {
        fs::File::open(entry.map_err(failure)?.path())
            .and_then(|file| file.sync_all())
            .map_err(failure)?;
    }
    #[cfg(unix)]
    for directory in [backup.path(), parent] {
        fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(failure)?;
    }
    Ok(backup)
}

impl Previous {
    pub(super) fn keep(self) {
        let _ = self.backup.keep();
    }

    pub(super) fn write(&self, transaction: &Transaction<'_>, process: Uuid) -> Result<(), String> {
        let now = super::super::runtime::now();
        if self.recovery_pending {
            transaction
                .execute("UPDATE store SET updating=1", [])
                .map_err(failure)?;
        }
        let mut history = self.history.clone();
        for row in &self.conversations {
            row.active.native_provider().map_err(failure)?;
            for binding in &row.bindings {
                binding.session.native_provider().map_err(failure)?;
            }
            let response = history.entry(row.id.clone()).or_default();
            response.thread.id = Some(row.id.clone());
            response.thread.cwd = (!row.cwd.trim().is_empty()).then(|| row.cwd.clone());
            response.thread.name = row.name.clone().or(response.thread.name.take());
            response.thread.preview = row.preview.clone().or(response.thread.preview.take());
            response.thread.active_provider = row.active.provider;
        }
        for id in self.queues.keys() {
            history.entry(id.clone()).or_insert_with(|| ThreadResponse {
                thread: agent_protocol::models::Thread {
                    id: Some(id.clone()),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let sequence = Storage::advance(transaction)?.sequence;
        for project in &self.projects {
            super::projects::write(transaction, project, sequence)?;
        }
        if !self.projects.is_empty() {
            jobs::enqueue(
                transaction,
                Uuid::new_v4(),
                "projects",
                now,
                &jobs::Work::ProjectsChanged,
            )?;
        }
        for (root, scripts) in &self.workspace_scripts {
            transaction
                .execute(
                    "INSERT INTO workspace_scripts(root,body) VALUES (?1,?2)",
                    params![root, protocol::encode(scripts).map_err(failure)?],
                )
                .map_err(failure)?;
        }
        // Parents can appear later in the previous database. Insert identities
        // first, then attach the validated links in a second pass.
        for (id, response) in &history {
            let mut header = history::header(response, now);
            header.details.parent = None;
            if let Some(selection) = self
                .models
                .iter()
                .find(|selection| &selection.session == id)
            {
                header.details.selection = selection.options.clone();
            }
            if let Some(row) = self.conversations.iter().find(|row| &row.id == id)
                && (row.startup.is_some() || row.workspace_setup.is_some())
            {
                let mut setup = row.workspace_setup.clone();
                if let Some(setup) = &mut setup
                    && setup.phase == agent_protocol::workspace::SetupPhase::Running
                {
                    setup.finish(
                        agent_protocol::workspace::SetupPhase::Cancelled,
                        None,
                        now.max(0) as u64,
                    );
                }
                header.details.workspace =
                    Some(Box::new(decision::records::WorkspacePreparation {
                        project_cwd: row
                            .startup
                            .as_ref()
                            .and_then(|startup| startup.project_cwd.clone())
                            .unwrap_or_else(|| row.cwd.clone()),
                        strategy: row
                            .startup
                            .as_ref()
                            .and_then(|startup| startup.workspace.clone()),
                        generation: Uuid::new_v4(),
                        created_worktree: false,
                        visible: setup
                            .as_ref()
                            .is_some_and(|setup| setup.visible_at(now.max(0) as u64)),
                        setup,
                    }));
            }
            if self.unimported.contains(id) {
                header.details.history_read_state =
                    Some(agent_protocol::session::HistoryReadState::new(
                        agent_protocol::session::HistoryReadKind::Loading,
                        Vec::new(),
                    ));
            }
            history::insert_header(transaction, id, &header, &Counters::default(), sequence)?;
        }
        for (id, response) in &history {
            let source = self
                .conversations
                .iter()
                .find(|row| &row.id == id)
                .and_then(|row| row.bindings.first())
                .map_or(id, |binding| &binding.session);
            let key = thread_key(transaction, id)?;
            let sources = native_history_sources(response, source)?;
            history::write_history(transaction, id, response, &sources, sequence, process, now)?;
            restore_user_receipts(transaction, id, response, sequence)?;
            if self.unimported.contains(id) && !self.conversations.iter().any(|row| &row.id == id) {
                // Recorded-only native turns survive even when the later native
                // read omits them. Loading remains explicit until reimport settles.
                let state = rows::load(transaction, key, process)?;
                rows::write(
                    transaction,
                    &rows::WriteScope {
                        key,
                        id,
                        sequence,
                        at: now,
                        process,
                    },
                    &Change::Binding {
                        provider: source.native_provider().map_err(failure)?,
                        value: Some(decision::Binding::Ready {
                            source: source.id.clone(),
                            covered: state.history_end(),
                        }),
                    },
                    &state.turns,
                    &mut Vec::new(),
                    &mut Vec::new(),
                )?;
            }
            if let Some(queue) = self.queues.get(id) {
                for input in &queue.inputs {
                    Self::input(
                        transaction,
                        &rows::WriteScope {
                            key,
                            id,
                            sequence,
                            at: now,
                            process,
                        },
                        input,
                        InputState::Queued { steer: None },
                    )?;
                }
            }
        }
        for row in &self.conversations {
            row.restore(transaction, sequence, process, now)?;
        }
        for id in &self.unimported {
            let source = self
                .conversations
                .iter()
                .find(|row| &row.id == id)
                .map_or(id, |row| &row.active);
            super::imports::queue(transaction, id, source, now)?;
        }
        for (id, response) in &history {
            history::link_parent(
                transaction,
                thread_key(transaction, id)?,
                response.thread.parent_id.as_ref(),
                sequence,
            )?;
        }
        for read in &self.reads {
            let Some(key) = existing_key(transaction, &read.session)? else {
                continue;
            };
            let (mut header, counters) = rows::header(transaction, key)?;
            header.details.activity.completion = Some(read.turn_id.clone());
            header.details.read_through = (!read.unread).then(|| read.turn_id.clone());
            transaction
                .execute(
                    "UPDATE threads SET body=?1 WHERE key=?2",
                    params![codec::header(&header, &counters)?, key],
                )
                .map_err(failure)?;
        }
        Ok(())
    }

    fn input(
        transaction: &Transaction<'_>,
        scope: &rows::WriteScope<'_>,
        input: &Submission,
        status: InputState,
    ) -> Result<(), String> {
        let &rows::WriteScope {
            key,
            id,
            sequence,
            at: now,
            process,
        } = scope;
        if let Ok(operation) = Uuid::parse_str(input.client_user_message_id.as_str()) {
            let accepted = transaction.query_row("SELECT outcome FROM receipts WHERE operation_id=?1 AND provider IS ?2 AND thread_id=?3", params![operation.to_string(),id.provider.map(ProviderKind::key),id.id], |row| row.get::<_,Vec<u8>>(0)).optional().map_err(failure)?.map(|bytes| protocol::decode::<Outcome>(&bytes).map_err(failure)).transpose()?;
            if accepted == Some(Outcome::Accepted) {
                return Ok(());
            }
        }
        let mut state = rows::load(transaction, key, process)?;
        if state.inputs.contains_key(&input.client_user_message_id) {
            return Ok(());
        }
        state.counters.input += 1;
        state.thread.queue_held = true;
        let content = InputContent {
            parts: input.input.clone(),
            model: input.model.clone(),
            effort: input.effort.clone(),
            service_tier: input.service_tier.clone(),
            follow_up: input.follow_up,
        };
        let pending = PendingInput {
            id: input.client_user_message_id.clone(),
            position: state.counters.input,
            content,
            state: status,
        };
        rows::write(
            transaction,
            scope,
            &Change::Input(pending),
            &state.turns,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
        rows::write_header(
            transaction,
            &rows::WriteScope {
                key,
                id,
                sequence,
                at: now,
                process: scope.process,
            },
            &state,
            None,
            None,
        )?;
        if let Ok(operation) = Uuid::parse_str(input.client_user_message_id.as_str()) {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO receipts VALUES (?1,?2,?3,?4,?5)",
                    params![
                        operation.to_string(),
                        id.provider.map(ProviderKind::key),
                        id.id,
                        integer(sequence)?,
                        protocol::encode(Outcome::Accepted).map_err(failure)?
                    ],
                )
                .map_err(failure)?;
        }
        Ok(())
    }
}

impl Conversation {
    fn restore(
        &self,
        transaction: &Transaction<'_>,
        sequence: u64,
        process: Uuid,
        now: i64,
    ) -> Result<(), String> {
        let key = thread_key(transaction, &self.id)?;
        for binding in &self.bindings {
            rows::source(
                transaction,
                key,
                binding.session.native_provider().map_err(failure)?,
                &binding.session.id,
            )?;
        }
        let state = rows::load(transaction, key, process)?;
        for binding in &self.bindings {
            let pending = self.handoffs.iter().any(|handoff| {
                handoff.session == binding.session
                    && (handoff.delivery == "Pending"
                        || matches!(
                            handoff.submission,
                            agent_protocol::session::SubmissionDelivery::Sending
                                | agent_protocol::session::SubmissionDelivery::Unknown
                        ))
            });
            if !pending
                && (binding.session.provider != self.active.provider
                    || binding.session == self.active)
            {
                rows::write(
                    transaction,
                    &rows::WriteScope {
                        key,
                        id: &self.id,
                        sequence,
                        at: now,
                        process,
                    },
                    &Change::Binding {
                        provider: binding.session.native_provider().map_err(failure)?,
                        value: Some(decision::Binding::Ready {
                            covered: binding
                                .covered
                                .map_or(state.history_end(), |covered| covered as u64),
                            source: binding.session.id.clone(),
                        }),
                    },
                    &state.turns,
                    &mut Vec::new(),
                    &mut Vec::new(),
                )?;
            }
        }
        for handoff in &self.handoffs {
            if handoff.receipt.is_none()
                && matches!(
                    handoff.submission,
                    agent_protocol::session::SubmissionDelivery::Sending
                        | agent_protocol::session::SubmissionDelivery::Unknown
                )
                && let Ok(input) = Uuid::parse_str(handoff.input_id.as_str())
            {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO receipts VALUES (?1,?2,?3,?4,?5)",
                        params![
                            input.to_string(),
                            self.id.provider.map(ProviderKind::key),
                            self.id.id,
                            integer(sequence)?,
                            protocol::encode(Outcome::DeliveryUnknown).map_err(failure)?
                        ],
                    )
                    .map_err(failure)?;
            }
            if handoff.receipt.is_some()
                && let Ok(operation) = Uuid::parse_str(handoff.input_id.as_str())
            {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO receipts VALUES (?1,?2,?3,?4,?5)",
                        params![
                            operation.to_string(),
                            self.id.provider.map(ProviderKind::key),
                            self.id.id,
                            integer(sequence)?,
                            protocol::encode(Outcome::Accepted).map_err(failure)?
                        ],
                    )
                    .map_err(failure)?;
            }
        }
        if let Some(startup) = &self.startup {
            let preparation = &startup.preparation;
            let input = &preparation.submission;
            let mut state = rows::load(transaction, key, process)?;
            let status = match &preparation.status {
                PreparationStatus::Preparing => InputState::Preparing {
                    stage: preparation.stage,
                    action: Uuid::new_v4(),
                    mode: decision::DeliveryMode::Direct,
                },
                PreparationStatus::Sending | PreparationStatus::Unknown => {
                    state.counters.turn += 1;
                    let attempt = Attempt {
                        id: Uuid::new_v4(),
                        turn: Uuid::new_v4().to_string().into(),
                        provider: self.active.native_provider().map_err(failure)?,
                        source: self.active.id.clone(),
                    };
                    InputState::Unknown {
                        delivery: PendingDelivery {
                            action: Uuid::new_v4(),
                            attempt,
                            ordinal: state.counters.turn,
                            interrupt: None,
                            steer: false,
                            queued: false,
                        },
                        status: UnknownState::Fenced,
                    }
                }
                PreparationStatus::Failed { error } => InputState::Failed(error.clone()),
                PreparationStatus::Interrupted => {
                    InputState::Failed(agent_protocol::execution::ExecutionError {
                        message: "準備を停止しました。".into(),
                        ..Default::default()
                    })
                }
            };
            rows::write_header(
                transaction,
                &rows::WriteScope {
                    key,
                    id: &self.id,
                    sequence,
                    at: now,
                    process,
                },
                &state,
                None,
                None,
            )?;
            Previous::input(
                transaction,
                &rows::WriteScope {
                    key,
                    id: &self.id,
                    sequence,
                    at: now,
                    process,
                },
                input,
                status,
            )?;
        }
        Ok(())
    }
}

fn restore_user_receipts(
    transaction: &Transaction<'_>,
    id: &SessionRef,
    response: &ThreadResponse,
    sequence: u64,
) -> Result<(), String> {
    for item in response
        .thread
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
    {
        if matches!(
            item.body(),
            agent_protocol::items::ItemBody::UserMessage { .. }
        ) && let Some(input) = item
            .client_input_id
            .as_ref()
            .and_then(|id| Uuid::parse_str(id.as_str()).ok())
        {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO receipts VALUES (?1,?2,?3,?4,?5)",
                    params![
                        input.to_string(),
                        id.provider.map(ProviderKind::key),
                        id.id,
                        integer(sequence)?,
                        protocol::encode(Outcome::Accepted).map_err(failure)?
                    ],
                )
                .map_err(failure)?;
        }
    }

    Ok(())
}

// Qualified addresses existed only in the previous store. Decode them here;
// public IDs stay unchanged and live protocols never accept this old format.
fn native_address(id: &str, first: &SessionRef) -> Result<(SessionRef, String), String> {
    use base64::Engine;
    if let Some(encoded) = id.strip_prefix("bex:") {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(failure)?;
        let address: (SessionRef, String) = serde_json::from_slice(&bytes).map_err(failure)?;
        address.0.native_provider().map_err(failure)?;
        Ok(address)
    } else {
        Ok((first.clone(), id.into()))
    }
}

fn native_history_sources(
    response: &ThreadResponse,
    first: &SessionRef,
) -> Result<BTreeMap<TurnId, history::NativeTurn>, String> {
    let mut sources: BTreeMap<TurnId, history::NativeTurn> = BTreeMap::new();
    for turn in response.thread.turns.iter().flatten() {
        let (source, native) = native_address(turn.id.as_str(), first)?;
        let entry = sources
            .entry(turn.id.clone())
            .or_insert_with(|| history::NativeTurn {
                source: source.clone(),
                id: native.into(),
                items: BTreeMap::new(),
            });
        for item in turn.items.iter().flatten() {
            let (owner, native) = native_address(item.id.as_str(), &source)?;
            if owner != source {
                return Err("previous item belongs to another native source".into());
            }
            entry.items.insert(item.id.clone(), native.into());
        }
    }
    Ok(sources)
}

/// The old worker continues reading the same journal while the new store owns
/// recovery. Rebind its native turn once; no alternate runtime wire decoder.
pub(super) fn recover_update(connection: &Connection, path: &Path) -> Result<(), String> {
    use agent_local_host::update::{ContinuationDelivery, RECOVERY_FILE, Recovery};
    let path = path.with_file_name(RECOVERY_FILE);
    let Some(mut report) = file::<Recovery>(&path)? else {
        return Ok(());
    };
    let previous = serde_json::to_vec(&report).map_err(failure)?;
    for task in &mut report.tasks {
        if task.delivery == ContinuationDelivery::Sending {
            task.delivery = ContinuationDelivery::Failed(
                "継続の配送結果を確認できません。会話を開いて確認してください。".into(),
            );
        }
        if let Some(turn) = &task.last_turn {
            let target = &task.submission.thread_id;
            let Some(key) = existing_key(connection, target)? else {
                continue;
            };
            // New reports already name the canonical turn. Old reports name the
            // active native source's last turn, including qualified handoffs.
            if rows::turn(connection, key, turn)?.is_some() {
                continue;
            }
            let canonical: Option<String> = connection.query_row(
                "SELECT e.turn_id FROM executions e JOIN sources s ON s.id=e.source_id JOIN turns t ON t.thread=e.thread AND t.id=e.turn_id WHERE e.thread=?1 AND s.provider IS ?2 AND s.native_id=?3 AND e.native_turn_id=?4 AND t.state!='rolled_back' ORDER BY t.ordinal DESC LIMIT 1",
                params![key, task.native_session.provider.map(ProviderKind::key), task.native_session.id, turn.as_str()], |row| row.get(0)
            ).optional().map_err(failure)?;
            if let Some(canonical) = canonical {
                task.last_turn = Some(canonical.into());
            }
        }
    }
    if serde_json::to_vec(&report).map_err(failure)? != previous {
        agent_local_host::save_private_json(&path, &report).map_err(failure)?;
    }
    Ok(())
}
