//! This owner persists dispatch IDs before sending. An interrupted dispatch is
//! reconciled from conversation receipts; it is never blindly sent again.
use agent_protocol::{automations::*, conversation::Outcome, session::SessionRef};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Mutex};
use uuid::Uuid;

pub(crate) struct Automations(Mutex<Result<Connection, String>>);
fn failure(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn decode(bytes: Vec<u8>) -> Result<Automation, String> {
    serde_json::from_slice(&bytes).map_err(failure)
}
fn save(connection: &Connection, value: &Automation) -> Result<(), String> {
    let uncertain = value
        .last_run
        .as_ref()
        .is_some_and(|run| matches!(run.state, RunState::Sending | RunState::Unknown));
    connection.execute("INSERT INTO automations(id,session,next_at,uncertain,body) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET next_at=excluded.next_at,uncertain=excluded.uncertain,body=excluded.body", params![value.definition.id.to_string(), value.definition.session.to_string(), value.next_at_ms, uncertain, serde_json::to_vec(value).map_err(failure)?]).map_err(failure)?;
    Ok(())
}
fn find(
    connection: &Connection,
    session: &SessionRef,
    id: Uuid,
) -> Result<Option<Automation>, String> {
    connection
        .query_row(
            "SELECT body FROM automations WHERE id=?1 AND session=?2",
            params![id.to_string(), session.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?
        .map(decode)
        .transpose()
}
fn all(connection: &Connection, session: Option<&SessionRef>) -> Result<Vec<Automation>, String> {
    let mut statement = connection
        .prepare("SELECT body FROM automations WHERE ?1 IS NULL OR session=?1 ORDER BY id")
        .map_err(failure)?;
    statement
        .query_map([session.map(ToString::to_string)], |row| row.get(0))
        .map_err(failure)?
        .map(|row| decode(row.map_err(failure)?))
        .collect()
}
fn claim(
    definition: &Definition,
    previous: Option<&Run>,
    now: i64,
    id: Uuid,
) -> Result<Automation, String> {
    if previous.is_some_and(|run| matches!(run.state, RunState::Sending | RunState::Unknown)) {
        return Err("resolve the previous dispatch before running again".into());
    }
    Ok(Automation {
        definition: definition.clone(),
        next_at_ms: definition.next_after(now),
        last_run: Some(Run {
            id,
            at_ms: now,
            state: RunState::Sending,
        }),
    })
}

impl Automations {
    pub fn open(path: &Path) -> Self {
        let result = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(failure)?;
            }
            let connection = Connection::open(path).map_err(failure)?;
            connection
                .busy_timeout(std::time::Duration::from_secs(10))
                .map_err(failure)?;
            connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS automations(id TEXT PRIMARY KEY,session TEXT NOT NULL,next_at INTEGER,uncertain INTEGER NOT NULL CHECK(uncertain IN (0,1)),body BLOB NOT NULL) STRICT; CREATE INDEX IF NOT EXISTS automation_due ON automations(next_at) WHERE uncertain=0; CREATE INDEX IF NOT EXISTS automation_uncertain ON automations(session) WHERE uncertain=1;").map_err(failure)?;
            // An absent receipt after restart is unknown, not evidence of a failed send.
            for mut value in all(&connection, None)? {
                value.definition.validate().map_err(str::to_owned)?;
                if let Some(run) = &mut value.last_run
                    && run.state == RunState::Sending
                {
                    run.state = RunState::Unknown;
                    save(&connection, &value)?;
                }
            }
            Ok(connection)
        })();
        Self(Mutex::new(result))
    }
    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.0.lock().map_err(failure)?;
        f(guard.as_mut().map_err(|error| error.clone())?)
    }
    pub fn list(&self, session: Option<&SessionRef>) -> Result<Reply, String> {
        self.with(|c| all(c, session).map(|automations| Reply { automations }))
    }
    pub fn uncertain(&self, session: Option<&SessionRef>) -> Result<Vec<Automation>, String> {
        self.with(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT body FROM automations WHERE uncertain=1 AND (?1 IS NULL OR session=?1)",
                )
                .map_err(failure)?;
            statement
                .query_map([session.map(ToString::to_string)], |row| row.get(0))
                .map_err(failure)?
                .map(|row| decode(row.map_err(failure)?))
                .collect()
        })
    }
    pub fn change(&self, request: &Request, now: i64) -> Result<Option<Automation>, String> {
        request.validate().map_err(str::to_owned)?;
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let mut dispatch = None;
            match &request.action {
                Action::List => {}
                Action::Save { definition } => {
                    let previous = tx
                        .query_row(
                            "SELECT body FROM automations WHERE id=?1",
                            [definition.id.to_string()],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(failure)?
                        .map(decode)
                        .transpose()?;
                    if previous
                        .as_ref()
                        .is_some_and(|old| old.definition.session != request.session)
                    {
                        return Err("automation belongs to a different conversation".into());
                    }
                    if previous.is_none() {
                        let count: i64 = tx.query_row("SELECT count(*) FROM automations", [], |row| row.get(0)).map_err(failure)?;
                        if count >= AUTOMATION_LIMIT { return Err("too many automations".into()); }
                    }
                    if previous
                        .as_ref()
                        .and_then(|old| old.last_run.as_ref())
                        .is_some_and(|run| {
                            matches!(run.state, RunState::Sending | RunState::Unknown)
                        })
                    {
                        return Err("resolve the previous dispatch before editing".into());
                    }
                    let next_at_ms = definition.next_after(now);
                    if definition.enabled && next_at_ms.is_none() { return Err("schedule has no future execution time".into()); }
                    save(
                        &tx,
                        &Automation {
                            definition: definition.as_ref().clone(),
                            next_at_ms,
                            last_run: previous.and_then(|old| old.last_run),
                        },
                    )?;
                }
                Action::Remove { id } => {
                    tx.execute(
                        "DELETE FROM automations WHERE id=?1 AND session=?2",
                        params![id.to_string(), request.session.to_string()],
                    )
                    .map_err(failure)?;
                }
                action => {
                    let id = match action {
                        Action::Pause { id } | Action::Resume { id } | Action::RunNow { id } | Action::Retry { id } => *id,
                        _ => unreachable!(),
                    };
                    let mut value = find(&tx, &request.session, id)?.ok_or("automation does not belong to this conversation")?;
                    match action {
                        Action::Retry { .. } => {
                            let run = value.last_run.as_mut().ok_or("no dispatch to retry")?;
                            if run.state != RunState::Unknown { return Err("only uncertain dispatches can be retried with the same identity".into()); }
                            run.state = RunState::Sending;
                            dispatch = Some(value.clone());
                        }
                        Action::Pause { .. } => {
                            value.definition.enabled = false;
                            value.next_at_ms = None;
                        }
                        Action::Resume { .. } => {
                            value.definition.enabled = true;
                            value.next_at_ms = value.definition.next_after(now);
                            if value.next_at_ms.is_none() { return Err("schedule has no future execution time".into()); }
                        }
                        Action::RunNow { .. } => {
                            value = claim(&value.definition, value.last_run.as_ref(), now, Uuid::new_v4())?;
                            dispatch = Some(value.clone());
                        }
                        _ => unreachable!(),
                    }
                    save(&tx, &value)?;
                }
            }
            tx.commit().map_err(failure)?;
            Ok(dispatch)
        })
    }
    pub fn due(&self, now: i64) -> Result<Vec<Automation>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let candidates = {
                let mut statement = tx.prepare("SELECT body FROM automations WHERE uncertain=0 AND next_at<=?1 ORDER BY next_at,id").map_err(failure)?;
                statement.query_map([now], |row| row.get(0)).map_err(failure)?.map(|row| decode(row.map_err(failure)?)).collect::<Result<Vec<_>, _>>()?
            };
            let mut due = Vec::new();
            for value in candidates {
                if value.definition.enabled
                    && value.next_at_ms.is_some_and(|at| at <= now)
                    && let Ok(claimed) = claim(&value.definition, value.last_run.as_ref(), now, Uuid::new_v4())
                {
                    save(&tx, &claimed)?;
                    due.push(claimed);
                }
            }
            tx.commit().map_err(failure)?;
            Ok(due)
        })
    }
    pub fn finish(
        &self,
        value: &Automation,
        result: Result<Outcome, String>,
    ) -> Result<(), String> {
        self.with(|connection| {
            let Some(run) = &value.last_run else {
                return Ok(());
            };
            let Some(mut current) =
                find(connection, &value.definition.session, value.definition.id)?
            else {
                return Ok(());
            };
            let Some(saved) = current.last_run.as_mut().filter(|saved| saved.id == run.id) else {
                return Ok(());
            };
            saved.state = match result {
                Ok(Outcome::Accepted) => RunState::Accepted,
                Ok(Outcome::Rejected(message)) => RunState::Failed {
                    message: message.chars().take(4096).collect(),
                },
                Ok(Outcome::DeliveryUnknown) | Err(_) => RunState::Unknown,
                Ok(Outcome::Ignored) => RunState::Failed {
                    message: "input was ignored".into(),
                },
            };
            save(connection, &current)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition(session: SessionRef) -> Definition {
        Definition {
            id: Uuid::new_v4(),
            session,
            title: "Review".into(),
            prompt: "Check changes".into(),
            model: None,
            effort: None,
            service_tier: None,
            schedule: Schedule::Interval { every_ms: 60_000 },
            enabled: true,
        }
    }
    fn request(definition: &Definition, action: Action) -> Request {
        Request {
            session: definition.session.clone(),
            action,
        }
    }
    #[test]
    fn paused_schedules_persist_and_each_due_time_is_claimed_only_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("automations.sqlite");
        let owner = Automations::open(&path);
        let definition = definition(SessionRef::new(None, Uuid::new_v4().to_string()).unwrap());
        owner
            .change(
                &request(
                    &definition,
                    Action::Save {
                        definition: definition.clone().into(),
                    },
                ),
                0,
            )
            .unwrap();
        owner
            .change(
                &request(&definition, Action::Pause { id: definition.id }),
                20_000,
            )
            .unwrap();
        assert!(owner.due(120_000).unwrap().is_empty());
        drop(owner);
        let owner = Automations::open(&path);
        assert!(!owner.list(None).unwrap().automations[0].definition.enabled);
        owner
            .change(
                &request(&definition, Action::Resume { id: definition.id }),
                120_000,
            )
            .unwrap();
        assert!(owner.due(179_999).unwrap().is_empty());
        let first = owner.due(180_000).unwrap().pop().unwrap();
        assert!(owner.due(180_001).unwrap().is_empty());
        owner.finish(&first, Ok(Outcome::Accepted)).unwrap();
        let second = owner.due(240_000).unwrap().pop().unwrap();
        assert_ne!(first.last_run.unwrap().id, second.last_run.unwrap().id);
    }
    #[test]
    fn restart_retains_unknown_delivery_until_receipt_reconciliation_and_blocks_foreign_edits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("automations.sqlite");
        let owner = Automations::open(&path);
        let definition = definition(SessionRef::new(None, Uuid::new_v4().to_string()).unwrap());
        owner
            .change(
                &request(
                    &definition,
                    Action::Save {
                        definition: definition.clone().into(),
                    },
                ),
                0,
            )
            .unwrap();
        let sent = owner.due(60_000).unwrap().pop().unwrap();
        drop(owner);
        let owner = Automations::open(&path);
        assert_eq!(
            owner.list(None).unwrap().automations[0]
                .last_run
                .as_ref()
                .unwrap()
                .state,
            RunState::Unknown
        );
        assert!(owner.due(240_000).unwrap().is_empty());
        assert!(
            owner
                .change(
                    &request(&definition, Action::RunNow { id: definition.id }),
                    250_000
                )
                .is_err()
        );
        let retry = owner
            .change(
                &request(&definition, Action::Retry { id: definition.id }),
                250_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            retry.last_run.as_ref().unwrap().id,
            sent.last_run.as_ref().unwrap().id
        );
        owner.finish(&retry, Ok(Outcome::Accepted)).unwrap();
        let mut foreign = definition.clone();
        foreign.session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        assert!(
            owner
                .change(
                    &request(
                        &foreign,
                        Action::Save {
                            definition: foreign.clone().into()
                        }
                    ),
                    250_000
                )
                .is_err()
        );
        assert_eq!(
            owner.list(None).unwrap().automations[0].definition.session,
            definition.session
        );
        assert_eq!(owner.due(250_000).unwrap().len(), 1);
    }
}
