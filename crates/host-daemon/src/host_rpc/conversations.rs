//! The Host owns conversation history. Provider transcripts are import sources,
//! and native session IDs stay behind this boundary.
use agent_protocol::{
    models::{Item, ModelRef, Thread, ThreadResponse, Turn},
    session::{
        HistoryPage, HistoryReadKind, HistoryReadState, ProviderKind, SessionChange, SessionRef,
    },
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[cfg(test)]
mod tests;

pub(super) struct Conversations {
    connection: Mutex<Connection>,
}

impl Conversations {
    pub(super) fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path.parent().context("conversation directory is missing")?)?;
        let path =
            dunce::canonicalize(path.parent().context("conversation directory is missing")?)?.join(
                path.file_name()
                    .context("conversation database filename is missing")?,
            );
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(
            options
                .open(&path)
                .context("cannot create conversation database")?,
        );
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("cannot open conversation database")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        // One connection owns reads and writes. Rollback journaling also avoids
        // the WAL reset defect in the workspace's bundled SQLite version.
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
        let store = Self::initialize(connection)?;
        store.recover()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(super) fn memory() -> Self {
        Self::initialize(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn initialize(connection: Connection) -> Result<Self> {
        connection.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY, provider TEXT NOT NULL, scope TEXT NOT NULL,
                native_id TEXT NOT NULL, metadata TEXT NOT NULL, model TEXT NOT NULL,
                imported INTEGER NOT NULL DEFAULT 0, started INTEGER NOT NULL DEFAULT 0,
                cursor TEXT, oldest INTEGER NOT NULL DEFAULT 1, branch TEXT, import_issue TEXT,
                manual_title INTEGER NOT NULL DEFAULT 0, queue_held INTEGER NOT NULL DEFAULT 0,
                UNIQUE(provider, scope, native_id));
             CREATE INDEX IF NOT EXISTS unfinished_imports ON conversations(provider, scope, imported);
             CREATE TABLE IF NOT EXISTS turns (
                conversation TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                position INTEGER NOT NULL, native_id TEXT NOT NULL, body TEXT NOT NULL,
                observed INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(conversation, position));
             CREATE INDEX IF NOT EXISTS turn_identity ON turns(conversation, native_id, position);
             CREATE TABLE IF NOT EXISTS items (
                conversation TEXT NOT NULL, turn_position INTEGER NOT NULL,
                position INTEGER NOT NULL, native_id TEXT NOT NULL, body TEXT NOT NULL, client_input_id TEXT,
                summary_role INTEGER NOT NULL,
                PRIMARY KEY(conversation, turn_position, position),
                FOREIGN KEY(conversation, turn_position) REFERENCES turns(conversation, position) ON DELETE CASCADE);
             CREATE INDEX IF NOT EXISTS input_echo ON items(conversation, client_input_id, turn_position);
             CREATE INDEX IF NOT EXISTS item_summary ON items(conversation, turn_position, summary_role, position);
             CREATE TABLE IF NOT EXISTS events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                body TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS import_cursors (
                conversation TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                cursor TEXT NOT NULL, PRIMARY KEY(conversation, cursor));
             CREATE TABLE IF NOT EXISTS commands (
                conversation TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                input_id TEXT NOT NULL, payload TEXT NOT NULL, execution TEXT NOT NULL,
                delivery TEXT NOT NULL, queue_position INTEGER NOT NULL, queued INTEGER NOT NULL,
                PRIMARY KEY(conversation, input_id));
             CREATE INDEX IF NOT EXISTS waiting_commands ON commands(conversation, delivery, queue_position);"
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn recover(&self) -> Result<()> {
        use agent_protocol::{
            execution::{ErrorCategory, ExecutionError, SessionStatus, TurnStatus},
            session::SubmissionDelivery,
        };
        let targets = {
            let connection = self.lock();
            let mut query = connection.prepare("SELECT metadata FROM conversations")?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str::<Thread>(&row?)?))
                .collect::<Result<Vec<_>>>()?
        };
        for thread in targets {
            let target = thread
                .id
                .context("persisted conversation identity is missing")?;
            if self.has_queued(&target)? {
                self.queue_control(&target, &agent_protocol::queue::QueueAction::Pause)?;
            }
            let running = {
                let connection = self.lock();
                let mut query = connection.prepare("SELECT position FROM turns WHERE conversation=?1 AND observed=1 AND json_extract(body, '$.status')='running' ORDER BY position")?;
                query
                    .query_map([&target.id], |row| row.get::<_, i64>(0))?
                    .map(|row| read_turn(&connection, &target.id, row?, true))
                    .collect::<Result<Vec<_>>>()?
            };
            let owned_running = !running.is_empty();
            for mut turn in running {
                turn.status = TurnStatus::Interrupted;
                turn.error = Some(ExecutionError {
                    category: ErrorCategory::Network,
                    message: "Host restarted; the previous execution can no longer be observed"
                        .into(),
                    ..Default::default()
                });
                self.apply(
                    &target,
                    [&SessionChange::Turn {
                        turn,
                        completed: true,
                    }],
                )?;
            }
            for (id, delivery) in &thread.submissions {
                if matches!(delivery, SubmissionDelivery::Sending) {
                    self.apply(
                        &target,
                        [&SessionChange::Submission {
                            id: id.clone(),
                            delivery: SubmissionDelivery::Unknown,
                        }],
                    )?;
                }
            }
            for id in thread.requests.keys() {
                self.apply(
                    &target,
                    [&SessionChange::ResolveRequest {
                        request_id: id.clone(),
                    }],
                )?;
            }
            if owned_running && thread.status == SessionStatus::Running {
                self.apply(
                    &target,
                    [&SessionChange::Status {
                        status: SessionStatus::Unavailable,
                    }],
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn bind(&self, native: &SessionRef, scope: &str) -> Result<SessionRef> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let target = bind(&tx, native, scope)?;
        tx.commit()?;
        Ok(target)
    }

    pub(super) fn native(&self, target: &SessionRef, scope: &str) -> Result<SessionRef> {
        let native = self
            .lock()
            .query_row(
                "SELECT native_id FROM conversations WHERE id=?1 AND provider=?2 AND scope=?3",
                params![target.id, serde_json::to_string(&target.provider)?, scope],
                |row| row.get(0),
            )
            .optional()?
            .context("conversation is not owned by this provider instance")?;
        Ok(SessionRef {
            provider: target.provider,
            id: native,
        })
    }

    pub(super) fn import_state(&self, target: &SessionRef) -> Result<(bool, bool, Option<String>)> {
        self.lock()
            .query_row(
                "SELECT imported, started, cursor FROM conversations WHERE id=?1",
                [&target.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(Into::into)
    }

    pub(super) fn discover_page(
        &self,
        page: &[super::agent::SessionSummary],
        scope: &str,
    ) -> Result<()> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        for summary in page {
            let Some(native) = &summary.thread.id else {
                continue;
            };
            let target = bind(&tx, native, scope)?;
            let mut metadata = summary.thread.clone();
            metadata.id = Some(target.clone());
            metadata.turns = None;
            if let Some((name, updated_at)) = manual_title(&tx, &target.id)? {
                metadata.name = Some(name);
                metadata.updated_at = Some(updated_at);
            }
            tx.execute(
                "UPDATE conversations SET metadata=?2 WHERE id=?1 AND started=0",
                params![target.id, serde_json::to_string(&metadata)?],
            )?;
            tx.execute(
                "UPDATE conversations SET branch=?2 WHERE id=?1",
                params![target.id, summary.branch],
            )?;
            tx.execute(
                "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
                params![
                    target.id,
                    serde_json::to_string(
                        &serde_json::json!({"discovered": metadata, "branch": summary.branch})
                    )?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Walk unfinished imports in insertion order without retaining the whole
    /// catalog. A later refresh retries failures; this pass visits each once.
    pub(super) fn pending_imports(
        &self,
        provider: ProviderKind,
        scope: &str,
        after: i64,
    ) -> Result<Vec<(i64, SessionRef)>> {
        let connection = self.lock();
        let mut query = connection.prepare(
            "SELECT rowid, id FROM conversations WHERE provider=?1 AND scope=?2 AND imported=0 AND rowid>?3 ORDER BY rowid LIMIT 32",
        )?;
        query
            .query_map(
                params![serde_json::to_string(&provider)?, scope, after],
                |row| {
                    Ok((
                        row.get(0)?,
                        SessionRef {
                            provider,
                            id: row.get(1)?,
                        },
                    ))
                },
            )?
            .map(|row| row.map_err(Into::into))
            .collect()
    }

    pub(super) fn import_failed(&self, target: &SessionRef, issue: &str) -> Result<()> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        ensure!(
            tx.execute(
                "UPDATE conversations SET import_issue=?2 WHERE id=?1",
                params![target.id, issue],
            )? == 1,
            "conversation is missing"
        );
        tx.execute(
            "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
            params![
                target.id,
                serde_json::to_string(&serde_json::json!({"importFailed": issue}))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Commit one bounded provider page together with its continuation. A crash
    /// cannot duplicate a page or expose a cursor before its contents exist.
    pub(super) fn import_page(
        &self,
        target: &SessionRef,
        response: Option<&ThreadResponse>,
        page: &HistoryPage,
    ) -> Result<()> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let (started, oldest): (bool, i64) = tx.query_row(
            "SELECT started, oldest FROM conversations WHERE id=?1",
            [&target.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            started == response.is_none(),
            "import page does not match persisted progress"
        );
        if let Some(cursor) = &page.next_cursor {
            ensure!(!cursor.is_empty(), "empty provider history cursor");
            tx.execute(
                "INSERT INTO import_cursors(conversation, cursor) VALUES(?1, ?2)",
                params![target.id, cursor],
            )
            .context("provider history cursor repeated")?;
        }
        let first = if started {
            oldest
                .checked_sub(i64::try_from(page.turns.len())?)
                .context("history position overflow")?
        } else {
            1
        };
        for (index, turn) in page.turns.iter().enumerate() {
            ensure!(!turn.id.is_empty(), "imported turn has no ID");
            ensure!(
                !turn.items_summary
                    && turn
                        .items
                        .as_ref()
                        .is_some_and(|items| items.iter().all(|item| !item.is_deferred())),
                "imported turn details are incomplete"
            );
            write_turn(&tx, &target.id, first + i64::try_from(index)?, turn, false)?;
        }
        if let Some(response) = response {
            let mut metadata = response.thread.clone();
            let previous: String = tx.query_row(
                "SELECT metadata FROM conversations WHERE id=?1",
                [&target.id],
                |row| row.get(0),
            )?;
            let previous: Thread = serde_json::from_str(&previous)?;
            metadata.submissions.extend(previous.submissions);
            metadata.requests.extend(previous.requests);
            if let Some(updated_at) = previous.updated_at {
                metadata.updated_at = Some(updated_at.max(metadata.updated_at.unwrap_or_default()));
            }
            if let Some((name, updated_at)) = manual_title(&tx, &target.id)? {
                metadata.name = Some(name);
                metadata.updated_at = Some(updated_at);
            }
            metadata.id = Some(target.clone());
            metadata.turns = None;
            metadata.history_cursor = None;
            metadata.history_has_more = None;
            metadata.history_limit = None;
            tx.execute(
                "UPDATE conversations SET metadata=?2, model=?3 WHERE id=?1",
                params![
                    target.id,
                    serde_json::to_string(&metadata)?,
                    serde_json::to_string(&response.model)?
                ],
            )?;
        }
        tx.execute(
            "UPDATE conversations SET imported=?2, started=1, cursor=?3, oldest=?4, import_issue=NULL WHERE id=?1",
            params![
                target.id,
                page.next_cursor.is_none(),
                page.next_cursor,
                first
            ],
        )?;
        tx.execute("INSERT INTO events(conversation, body) VALUES(?1, ?2)", params![target.id,
            serde_json::to_string(&serde_json::json!({"imported": page, "metadata": response, "firstPosition": first}))?])?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn open_thread(
        &self,
        target: &SessionRef,
        limit: usize,
        include_activity: bool,
    ) -> Result<ThreadResponse> {
        ensure!(limit > 0, "history limit is zero");
        let connection = self.lock();
        let (metadata, model, imported, import_issue, source_cursor, oldest): (String, String, bool, Option<String>, Option<String>, i64) =
            connection.query_row(
                "SELECT metadata, model, imported, import_issue, cursor, oldest FROM conversations WHERE id=?1",
                [&target.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )?;
        let mut thread: Thread = serde_json::from_str(&metadata)?;
        ensure!(
            thread.id.as_ref() == Some(target),
            "conversation identity does not match"
        );
        thread.queued_inputs = {
            let mut query = connection.prepare("SELECT execution, delivery FROM commands WHERE conversation=?1 AND queued=1 AND delivery IN ('\"queued\"','\"sending\"','\"unknown\"','\"rejected\"') ORDER BY queue_position, rowid")?;
            query
                .query_map([&target.id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .map(|row| {
                    let (input, delivery) = row?;
                    Ok(agent_protocol::queue::QueueEntry {
                        submission: serde_json::from_str(&input)?,
                        delivery: serde_json::from_str(&delivery)?,
                    })
                })
                .collect::<Result<_>>()?
        };
        thread.queue_held = connection.query_row(
            "SELECT queue_held FROM conversations WHERE id=?1",
            [&target.id],
            |row| row.get(0),
        )?;
        let (turns, mut cursor) =
            read_page(&connection, &target.id, None, limit, include_activity)?;
        if cursor.is_none() && !imported && source_cursor.is_some() {
            cursor = Some(serde_json::to_string(&(&target.id, oldest, limit))?);
        }
        thread.turns = Some(turns);
        thread.history_has_more = Some(cursor.is_some());
        thread.history_cursor = cursor;
        thread.history_limit = Some(limit as u64);
        let mut issues = thread
            .history_read_state
            .take()
            .map(|state| state.issues)
            .unwrap_or_default();
        issues.extend(import_issue);
        thread.history_read_state = Some(HistoryReadState::new(
            if !issues.is_empty() {
                HistoryReadKind::Incomplete
            } else if !imported {
                HistoryReadKind::Importing
            } else if thread.history_has_more == Some(true) {
                HistoryReadKind::Partial
            } else {
                HistoryReadKind::Complete
            },
            issues,
        ));
        Ok(ThreadResponse {
            thread,
            model: serde_json::from_str::<Option<ModelRef>>(&model)?,
        })
    }

    pub(super) fn history(
        &self,
        target: &SessionRef,
        cursor: &str,
        include_activity: bool,
    ) -> Result<HistoryPage> {
        let (id, before, limit): (String, i64, usize) =
            serde_json::from_str(cursor).context("invalid history cursor")?;
        ensure!(
            id == target.id && limit > 0 && limit <= 1000,
            "history cursor belongs to another conversation or has an invalid limit"
        );
        let connection = self.lock();
        let (turns, mut next_cursor) = read_page(
            &connection,
            &target.id,
            Some(before),
            limit,
            include_activity,
        )?;
        if next_cursor.is_none() {
            let (imported, source_cursor, oldest): (bool, Option<String>, i64) = connection
                .query_row(
                    "SELECT imported, cursor, oldest FROM conversations WHERE id=?1",
                    [&target.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
            if !imported && source_cursor.is_some() {
                next_cursor = Some(serde_json::to_string(&(&target.id, oldest, limit))?);
            }
        }
        Ok(HistoryPage { turns, next_cursor })
    }

    pub(super) fn turn(&self, target: &SessionRef, turn: &str) -> Result<Turn> {
        let connection = self.lock();
        let position =
            latest_turn(&connection, &target.id, turn)?.context("turn is not available")?;
        read_turn(&connection, &target.id, position, true)
    }

    /// A deferred live body is imported once. A concurrent stream change must
    /// not be replaced by an older body read, including an older status/header.
    pub(super) fn hydrate_item(
        &self,
        target: &SessionRef,
        turn: &agent_protocol::ids::TurnId,
        previous: &Item,
        item: &Item,
    ) -> Result<bool> {
        ensure!(
            item.id == previous.id && !item.is_deferred(),
            "hydrated item identity or body is invalid"
        );
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let position = latest_turn(&tx, &target.id, turn)?.context("turn is not available")?;
        let current = read_turn(&tx, &target.id, position, true)?;
        if current
            .items
            .iter()
            .flatten()
            .find(|item| item.id == previous.id)
            .map(AsRef::as_ref)
            != Some(previous)
        {
            return Ok(false);
        }
        apply_change(
            &tx,
            target,
            &SessionChange::Item {
                turn_id: turn.clone(),
                item: Arc::new(item.clone()),
            },
            recorded_at(),
            false,
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub(super) fn title_list(
        &self,
        areas: &[(ProviderKind, String)],
        projects: &crate::projects::state::Snapshot,
        params: &agent_protocol::models::ListQuery,
    ) -> Result<(
        agent_protocol::models::ThreadList,
        std::collections::HashMap<SessionRef, String>,
    )> {
        let mut areas = areas.to_vec();
        areas.sort();
        areas.dedup();
        let connection = self.lock();
        // Select only title fields. Receipts, requests, queues and timeline
        // bodies remain in the DB. The connection lock keeps this iterator on
        // one catalog snapshot, and the selector retains only visible rows.
        let mut query = connection.prepare(
            "SELECT json_object('id', json_extract(c.metadata, '$.id'),
                'name', json_extract(c.metadata, '$.name'),
                'cwd', json_extract(c.metadata, '$.cwd'),
                'status', json_extract(c.metadata, '$.status'),
                'preview', json_extract(c.metadata, '$.preview'),
                'updatedAt', json_extract(c.metadata, '$.updatedAt')), c.branch
             FROM conversations c JOIN json_each(?1) area
                ON c.provider=json_quote(json_extract(area.value, '$[0]'))
                AND c.scope=json_extract(area.value, '$[1]')
             ORDER BY COALESCE(CAST(json_extract(c.metadata, '$.updatedAt') AS REAL), 0) DESC,
                CAST(area.key AS INTEGER), c.id",
        )?;
        let mut rows = query.query([serde_json::to_string(&areas)?])?;
        let search = params.search_term.trim().to_lowercase();
        let mut titles = crate::projects::titles::TitleList::new(&projects.projects, params);
        let mut branches = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let mut thread: Thread = serde_json::from_str(&row.get::<_, String>(0)?)?;
            if !search.is_empty()
                && !thread
                    .name
                    .iter()
                    .chain(thread.preview.iter())
                    .any(|text| text.to_lowercase().contains(&search))
            {
                continue;
            }
            thread.project_id = projects.project_membership(thread.cwd.as_deref());
            let id = thread
                .id
                .clone()
                .context("stored title identity is missing")?;
            if titles.push(thread)
                && let Some(branch) = row.get::<_, Option<String>>(1)?
            {
                branches.insert(id, branch);
            }
            if titles.complete() {
                break;
            }
        }
        Ok((titles.finish(), branches))
    }

    /// Journal and projection are committed before the router publishes a change.
    /// Only the addressed turn is read; history outside it stays on disk.
    pub(super) fn apply<'a>(
        &self,
        target: &SessionRef,
        changes: impl IntoIterator<Item = &'a SessionChange>,
    ) -> Result<()> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let recorded_at = recorded_at();
        for change in changes {
            apply_change(&tx, target, change, recorded_at, true)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn rename(&self, target: &SessionRef, name: &str, manual: bool) -> Result<()> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 4096,
            "conversation name must contain 1 to 4096 bytes"
        );
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let (metadata, manual_title): (String, bool) = tx.query_row(
            "SELECT metadata, manual_title FROM conversations WHERE id=?1",
            [&target.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !manual && manual_title {
            return Ok(());
        }
        let mut thread: Thread = serde_json::from_str(&metadata)?;
        thread.name = Some(name.into());
        let recorded_at = recorded_at();
        thread.updated_at = Some(recorded_at);
        tx.execute(
            "UPDATE conversations SET metadata=?2, manual_title=?3 WHERE id=?1",
            params![
                target.id,
                serde_json::to_string(&thread)?,
                manual || manual_title
            ],
        )?;
        tx.execute(
            "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
            params![
                target.id,
                serde_json::to_string(&serde_json::json!({"renamed": name, "manual": manual, "recordedAt": recorded_at}))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The full command is durable before provider IO. Reusing an ID with another
    /// payload is rejected even after completion or a Host restart.
    pub(super) fn previous_command(
        &self,
        input: &agent_protocol::operations::Submission,
    ) -> Result<Option<agent_protocol::session::SubmissionDelivery>> {
        previous_command(&self.lock(), input)
    }

    pub(super) fn admit(
        &self,
        input: &agent_protocol::operations::Submission,
        delivery: agent_protocol::session::SubmissionDelivery,
    ) -> Result<Option<agent_protocol::session::SubmissionDelivery>> {
        ensure!(
            !input.client_user_message_id.is_empty() && input.client_user_message_id.len() <= 256,
            "client input ID is required and must be at most 256 bytes"
        );
        let payload = serde_json::to_string(input)?;
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        if let Some(delivery) = previous_command(&tx, input)? {
            return Ok(Some(delivery));
        }
        if delivery == agent_protocol::session::SubmissionDelivery::Queued {
            ensure!(
                tx.query_row(
                    "SELECT COUNT(*) FROM commands WHERE conversation=?1 AND delivery='\"queued\"'",
                    [&input.thread_id.id],
                    |row| row.get::<_, i64>(0),
                )? < 128,
                "input queue capacity reached"
            );
        }
        tx.execute("INSERT INTO commands(conversation, input_id, payload, execution, delivery, queue_position, queued) VALUES(?1, ?2, ?3, ?3, ?4, (SELECT COALESCE(MAX(queue_position), 0) + 1 FROM commands WHERE conversation=?1), ?5)",
            params![input.thread_id.id, input.client_user_message_id.as_str(), payload, serde_json::to_string(&delivery)?, delivery == agent_protocol::session::SubmissionDelivery::Queued])?;
        let metadata: String = tx.query_row(
            "SELECT metadata FROM conversations WHERE id=?1",
            [&input.thread_id.id],
            |row| row.get(0),
        )?;
        let thread: Thread = serde_json::from_str(&metadata)?;
        let change = SessionChange::Submission {
            id: input.client_user_message_id.clone(),
            delivery,
        };
        let mut next = change.apply(&thread)?;
        let recorded_at = recorded_at();
        next.updated_at = Some(recorded_at);
        tx.execute(
            "UPDATE conversations SET metadata=?2 WHERE id=?1",
            params![input.thread_id.id, serde_json::to_string(&next)?],
        )?;
        tx.execute(
            "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
            params![
                input.thread_id.id,
                serde_json::to_string(&serde_json::json!({"admitted": input, "change": change, "recordedAt": recorded_at}))?
            ],
        )?;
        tx.commit()?;
        Ok(None)
    }

    #[cfg(test)]
    pub(super) fn queued(
        &self,
        target: &SessionRef,
    ) -> Result<Vec<agent_protocol::operations::Submission>> {
        queued(&self.lock(), &target.id)
    }

    pub(super) fn has_queued(&self, target: &SessionRef) -> Result<bool> {
        self.lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM commands WHERE conversation=?1 AND delivery='\"queued\"')",
                [&target.id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub(super) fn queue_held(&self, target: &SessionRef) -> Result<bool> {
        self.lock()
            .query_row(
                "SELECT queue_held FROM conversations WHERE id=?1",
                [&target.id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Queue control never starts provider work. Order and editable payload are
    /// committed together; the original admission payload remains immutable.
    pub(super) fn queue_control(
        &self,
        target: &SessionRef,
        action: &agent_protocol::queue::QueueAction,
    ) -> Result<()> {
        use agent_protocol::queue::{QueueAction, move_before};
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        match action {
            QueueAction::Pause | QueueAction::Resume => {
                ensure!(
                    tx.execute(
                        "UPDATE conversations SET queue_held=?2 WHERE id=?1",
                        params![target.id, matches!(action, QueueAction::Pause)]
                    )? == 1,
                    "conversation is not available"
                );
            }
            QueueAction::Move { id, before } => {
                let mut query = tx.prepare("SELECT input_id FROM commands WHERE conversation=?1 AND delivery='\"queued\"' ORDER BY queue_position, rowid")?;
                let order = query
                    .query_map([&target.id], |row| {
                        Ok(agent_protocol::ids::ClientInputId::from(
                            row.get::<_, String>(0)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (position, id) in move_before(&order, id, before.as_ref())
                    .map_err(anyhow::Error::msg)?
                    .iter()
                    .enumerate()
                {
                    tx.execute("UPDATE commands SET queue_position=?3 WHERE conversation=?1 AND input_id=?2", params![target.id, id.as_str(), position])?;
                }
            }
            QueueAction::Cancel { id } => {
                let delivery: String = tx.query_row("SELECT delivery FROM commands WHERE conversation=?1 AND input_id=?2 AND queued=1",
                    params![target.id, id.as_str()], |row| row.get(0))?;
                let delivery: agent_protocol::session::SubmissionDelivery =
                    serde_json::from_str(&delivery)?;
                ensure!(
                    !matches!(
                        delivery,
                        agent_protocol::session::SubmissionDelivery::Sending
                            | agent_protocol::session::SubmissionDelivery::Accepted { .. }
                    ),
                    "input is already being delivered"
                );
                if delivery == agent_protocol::session::SubmissionDelivery::Queued {
                    apply_change(
                        &tx,
                        target,
                        &SessionChange::Submission {
                            id: id.clone(),
                            delivery: agent_protocol::session::SubmissionDelivery::Rejected,
                        },
                        recorded_at(),
                        true,
                    )?;
                }
                tx.execute(
                    "UPDATE commands SET queued=0 WHERE conversation=?1 AND input_id=?2",
                    params![target.id, id.as_str()],
                )?;
            }
            QueueAction::Edit { submission } => {
                ensure!(
                    &submission.thread_id == target,
                    "queued input belongs to another conversation"
                );
                ensure!(
                    submission
                        .model
                        .as_ref()
                        .is_none_or(|model| model.provider == target.provider),
                    "queued model belongs to another provider"
                );
                ensure!(
                    submission.input.iter().any(|part| match part {
                        agent_protocol::operations::Input::Text { text } => !text.trim().is_empty(),
                        agent_protocol::operations::Input::LocalImage { path }
                        | agent_protocol::operations::Input::Mention { path, .. } =>
                            !path.is_empty(),
                        agent_protocol::operations::Input::Skill { .. } => false,
                    }),
                    "queued input must contain text or attachments"
                );
                let payload = serde_json::to_string(submission)?;
                ensure!(
                    payload.len() <= 1024 * 1024,
                    "queued input must be at most 1 MiB"
                );
                let changed = tx.execute(
                    "UPDATE commands SET execution=?3 WHERE conversation=?1 AND input_id=?2 AND delivery='\"queued\"'",
                    params![target.id, submission.client_user_message_id.as_str(), payload],
                )?;
                ensure!(changed == 1, "queued input is no longer available");
            }
        }
        tx.execute(
            "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
            params![
                target.id,
                serde_json::to_string(
                    &serde_json::json!({"queue": action, "recordedAt": recorded_at()})
                )?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Claim immediately before native IO. A crash after this commit is Unknown,
    /// never an automatic second execution of the same input.
    pub(super) fn claim_queued(
        &self,
        target: &SessionRef,
    ) -> Result<Option<agent_protocol::operations::Submission>> {
        let mut connection = self.lock();
        let tx = connection.transaction()?;
        let held: bool = tx.query_row(
            "SELECT queue_held FROM conversations WHERE id=?1",
            [&target.id],
            |row| row.get(0),
        )?;
        if held {
            return Ok(None);
        }
        let payload: Option<String> = tx
            .query_row("SELECT execution FROM commands WHERE conversation=?1 AND delivery='\"queued\"' ORDER BY queue_position, rowid LIMIT 1", [&target.id], |row| row.get(0))
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let input: agent_protocol::operations::Submission = serde_json::from_str(&payload)?;
        apply_change(
            &tx,
            target,
            &SessionChange::Submission {
                id: input.client_user_message_id.clone(),
                delivery: agent_protocol::session::SubmissionDelivery::Sending,
            },
            recorded_at(),
            true,
        )?;
        tx.commit()?;
        Ok(Some(input))
    }
}

fn bind(tx: &Transaction<'_>, native: &SessionRef, scope: &str) -> Result<SessionRef> {
    native.validate().map_err(anyhow::Error::msg)?;
    let provider = serde_json::to_string(&native.provider)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM conversations WHERE provider=?1 AND scope=?2 AND native_id=?3",
            params![provider, scope, native.id],
            |row| row.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let metadata = Thread {
                id: Some(SessionRef {
                    provider: native.provider,
                    id: id.clone(),
                }),
                ..Default::default()
            };
            tx.execute("INSERT INTO conversations(id, provider, scope, native_id, metadata, model) VALUES(?1, ?2, ?3, ?4, ?5, 'null')",
                params![id, provider, scope, native.id, serde_json::to_string(&metadata)?])?;
            tx.execute("INSERT INTO events(conversation, body) VALUES(?1, ?2)", params![id, serde_json::to_string(&serde_json::json!({"created": {"metadata": metadata, "native": native, "scope": scope}}))?])?;
            id
        }
    };
    Ok(SessionRef {
        provider: native.provider,
        id,
    })
}

#[cfg(test)]
fn queued(
    connection: &Connection,
    id: &str,
) -> Result<Vec<agent_protocol::operations::Submission>> {
    let mut query = connection.prepare("SELECT execution FROM commands WHERE conversation=?1 AND delivery='\"queued\"' ORDER BY queue_position, rowid")?;
    query
        .query_map([id], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect()
}

fn recorded_at() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn manual_title(tx: &Transaction<'_>, id: &str) -> Result<Option<(String, f64)>> {
    Ok(tx.query_row(
        "SELECT json_extract(metadata, '$.name'), json_extract(metadata, '$.updatedAt') FROM conversations WHERE id=?1 AND manual_title=1",
        [id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?)
}

fn apply_change(
    tx: &Transaction<'_>,
    target: &SessionRef,
    change: &SessionChange,
    recorded_at: f64,
    update_activity: bool,
) -> Result<()> {
    let metadata: String = tx.query_row(
        "SELECT metadata FROM conversations WHERE id=?1",
        [&target.id],
        |row| row.get(0),
    )?;
    let mut thread: Thread = serde_json::from_str(&metadata)?;
    ensure!(
        thread.id.as_ref() == Some(target),
        "conversation identity does not match"
    );
    if let SessionChange::Submission { id, delivery } = change
        && thread.submissions.get(id) == Some(delivery)
    {
        return Ok(());
    }
    let turn_id = match change {
        SessionChange::Turn { turn, .. } => Some(&turn.id),
        SessionChange::Item { turn_id, .. }
        | SessionChange::TurnItems { turn_id, .. }
        | SessionChange::RemoveItem { turn_id, .. }
        | SessionChange::Text { turn_id, .. }
        | SessionChange::ReasoningPart { turn_id, .. }
        | SessionChange::Error { turn_id, .. } => Some(turn_id),
        _ => None,
    };
    let position = if let SessionChange::Submission { id, .. } = change {
        tx.query_row("SELECT turn_position FROM items WHERE conversation=?1 AND client_input_id=?2 ORDER BY turn_position DESC LIMIT 1", params![target.id, id.as_str()], |row| row.get(0)).optional()?
    } else {
        turn_id
            .map(|id| latest_turn(tx, &target.id, id))
            .transpose()?
            .flatten()
    };
    if let Some(position) = position {
        thread.turns = Some(vec![Arc::new(read_turn(tx, &target.id, position, true)?)]);
    }
    let mut next = change.apply(&thread)?;
    // Activity timestamps are owned by this Host; imports retain source dates.
    if update_activity && !matches!(change, SessionChange::TurnItems { .. }) {
        next.updated_at = Some(recorded_at);
    }
    next.submissions.retain(|id, delivery| {
        !agent_protocol::session::submission_confirmed(
            id,
            delivery,
            next.turns.as_deref().unwrap_or_default(),
        )
    });
    let changed_turn = next.turns.take().and_then(|mut turns| turns.pop());
    if turn_id.is_some()
        && let Some(turn) = changed_turn
    {
        let position = match position {
            Some(position) => position,
            None => tx.query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM turns WHERE conversation=?1",
                [&target.id],
                |row| row.get(0),
            )?,
        };
        match change {
            SessionChange::Text { item_id, .. } | SessionChange::ReasoningPart { item_id, .. } => {
                let item = turn
                    .items
                    .as_ref()
                    .and_then(|items| items.iter().find(|item| &item.id == item_id))
                    .context("changed item is missing")?;
                ensure!(tx.execute("UPDATE items SET body=?4 WHERE conversation=?1 AND turn_position=?2 AND native_id=?3", params![target.id, position, item_id.as_str(), serde_json::to_string(item)?])? == 1, "changed item identity is ambiguous");
            }
            _ => write_turn(tx, &target.id, position, &turn, true)?,
        }
        tx.execute(
            "UPDATE turns SET observed=1 WHERE conversation=?1 AND position=?2",
            params![target.id, position],
        )?;
    }
    tx.execute(
        "UPDATE conversations SET metadata=?2 WHERE id=?1",
        params![target.id, serde_json::to_string(&next)?],
    )?;
    if let SessionChange::Submission { id, delivery } = change {
        tx.execute(
            "UPDATE commands SET delivery=?3 WHERE conversation=?1 AND input_id=?2",
            params![target.id, id.as_str(), serde_json::to_string(delivery)?],
        )?;
    }
    tx.execute(
        "INSERT INTO events(conversation, body) VALUES(?1, ?2)",
        params![
            target.id,
            serde_json::to_string(
                &serde_json::json!({"change": change, "recordedAt": recorded_at, "updateActivity": update_activity})
            )?
        ],
    )?;
    Ok(())
}

fn previous_command(
    connection: &Connection,
    input: &agent_protocol::operations::Submission,
) -> Result<Option<agent_protocol::session::SubmissionDelivery>> {
    let previous: Option<(String, String)> = connection
        .query_row(
            "SELECT payload, delivery FROM commands WHERE conversation=?1 AND input_id=?2",
            params![input.thread_id.id, input.client_user_message_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    previous
        .map(|(payload, delivery)| {
            ensure!(
                payload == serde_json::to_string(input)?,
                "input ID was already used for a different command"
            );
            Ok(serde_json::from_str(&delivery)?)
        })
        .transpose()
}

fn latest_turn(connection: &Connection, id: &str, native: &str) -> Result<Option<i64>> {
    Ok(connection.query_row("SELECT position FROM turns WHERE conversation=?1 AND native_id=?2 ORDER BY position DESC LIMIT 1", params![id, native], |row| row.get(0)).optional()?)
}

fn write_turn(
    tx: &Transaction<'_>,
    id: &str,
    position: i64,
    turn: &Turn,
    observed: bool,
) -> Result<()> {
    let mut header = turn.clone();
    header.items = None;
    tx.execute("INSERT INTO turns(conversation, position, native_id, body, observed) VALUES(?1, ?2, ?3, ?4, ?5) ON CONFLICT(conversation, position) DO UPDATE SET native_id=excluded.native_id, body=excluded.body, observed=MAX(turns.observed, excluded.observed)", params![id, position, turn.id.as_str(), serde_json::to_string(&header)?, observed])?;
    tx.execute(
        "DELETE FROM items WHERE conversation=?1 AND turn_position=?2",
        params![id, position],
    )?;
    for (index, item) in turn.items.iter().flatten().enumerate() {
        let summary_role = match item.body() {
            agent_protocol::models::ItemBody::UserMessage { .. } => 1,
            agent_protocol::models::ItemBody::AssistantText {
                phase:
                    agent_protocol::models::AssistantPhase::Final
                    | agent_protocol::models::AssistantPhase::Unknown,
                ..
            } => 2,
            _ => 0,
        };
        tx.execute("INSERT INTO items(conversation, turn_position, position, native_id, body, client_input_id, summary_role) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![id, position, i64::try_from(index)?, item.id.as_str(), serde_json::to_string(item)?, item.client_input_id.as_ref().map(|id| id.as_str()), summary_role])?;
    }
    Ok(())
}

fn read_turn(
    connection: &Connection,
    id: &str,
    position: i64,
    include_activity: bool,
) -> Result<Turn> {
    let header: String = connection.query_row(
        "SELECT body FROM turns WHERE conversation=?1 AND position=?2",
        params![id, position],
        |row| row.get(0),
    )?;
    let mut turn: Turn = serde_json::from_str(&header)?;
    turn.items_summary = !include_activity;
    let sql = if include_activity {
        "SELECT body FROM items WHERE conversation=?1 AND turn_position=?2 ORDER BY position"
    } else {
        "SELECT body FROM items WHERE conversation=?1 AND turn_position=?2 AND position IN (
            SELECT MIN(position) FROM items WHERE conversation=?1 AND turn_position=?2 AND summary_role=1
            UNION SELECT MAX(position) FROM items WHERE conversation=?1 AND turn_position=?2 AND summary_role=2
         ) ORDER BY position"
    };
    let mut query = connection.prepare(sql)?;
    turn.items = Some(
        query
            .query_map(params![id, position], |row| row.get::<_, String>(0))?
            .map(|row| Ok(Arc::new(serde_json::from_str::<Item>(&row?)?)))
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(turn)
}

fn read_page(
    connection: &Connection,
    id: &str,
    before: Option<i64>,
    limit: usize,
    include_activity: bool,
) -> Result<(Vec<Arc<Turn>>, Option<String>)> {
    ensure!(
        limit > 0 && limit <= 1000,
        "history limit must be between 1 and 1000"
    );
    let mut query = connection.prepare("SELECT position FROM turns WHERE conversation=?1 AND (?2 IS NULL OR position < ?2) ORDER BY position DESC LIMIT ?3")?;
    let mut positions = query
        .query_map(params![id, before, i64::try_from(limit + 1)?], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = positions.len() > limit;
    positions.truncate(limit);
    positions.reverse();
    let cursor = if more {
        Some(serde_json::to_string(&(id, positions[0], limit))?)
    } else {
        None
    };
    Ok((
        positions
            .into_iter()
            .map(|position| read_turn(connection, id, position, include_activity).map(Arc::new))
            .collect::<Result<_>>()?,
        cursor,
    ))
}
