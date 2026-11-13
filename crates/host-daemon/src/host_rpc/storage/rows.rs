use super::*;
use decision::{Attempt, Binding, InputState, Item, PendingInput, Request, Turn};
use rusqlite::types::Value as SqlValue;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeSet;

const INPUT: &[(&str, &str)] = &[("id", "id"), ("position", "position")];
pub(super) const TURN: &[(&str, &str)] = &[("id", "id"), ("ordinal", "ordinal")];
const ITEM: &[(&str, &str)] = &[
    ("id", "id"),
    ("turn_id", "turn"),
    ("position", "position"),
    ("status", "status"),
];
const REQUEST: &[(&str, &str)] = &[("id", "id")];

fn scalar(value: &Value) -> Result<SqlValue, String> {
    match value {
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .ok_or("indexed value exceeds SQLite range".into()),
        Value::Null => Ok(SqlValue::Null),
        _ => Err("indexed field must be a scalar".into()),
    }
}

fn json(value: SqlValue) -> Result<Value, String> {
    match value {
        SqlValue::Text(value) => Ok(Value::String(value)),
        SqlValue::Integer(value) => Ok(value.into()),
        SqlValue::Null => Ok(Value::Null),
        _ => Err("invalid indexed value".into()),
    }
}

fn save(
    transaction: &Transaction<'_>,
    key: i64,
    table: &str,
    record: &impl Serialize,
    fields: &[(&str, &str)],
    state: bool,
    sequence: u64,
) -> Result<(), String> {
    let value = serde_json::to_value(record).map_err(failure)?;
    let (tag, body) = codec::pack(
        record,
        &fields.iter().map(|(_, wire)| *wire).collect::<Vec<_>>(),
        state,
    )?;
    let mut columns = vec!["thread", "seq"];
    columns.extend(fields.iter().map(|(sql, _)| *sql));
    if state {
        columns.push("state");
    }
    columns.push("body");
    let mut values = vec![
        SqlValue::Integer(key),
        SqlValue::Integer(integer(sequence)?),
    ];
    for (_, name) in fields {
        values.push(scalar(value.get(*name).ok_or("missing indexed field")?)?);
    }
    if state {
        values.push(SqlValue::Text(tag));
    }
    values.push(SqlValue::Blob(body));
    let assignments = columns
        .iter()
        .map(|column| format!("{column}=excluded.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let conflict = if matches!(table, "inputs" | "requests") {
        "id"
    } else {
        "thread,id"
    };
    let sql = format!(
        "INSERT INTO {table}({}) VALUES ({}) ON CONFLICT({conflict}) DO UPDATE SET {assignments} WHERE {table}.thread=excluded.thread",
        columns.join(","),
        vec!["?"; columns.len()].join(",")
    );
    let count = transaction
        .execute(&sql, rusqlite::params_from_iter(values))
        .map_err(failure)?;
    if count != 1 {
        return Err("record belongs to another conversation".into());
    }
    Ok(())
}

pub(super) fn read<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    filter: &str,
    values: Vec<SqlValue>,
    fields: &[(&str, &'static str)],
    state: bool,
) -> Result<Vec<(u64, T)>, String> {
    let mut columns = fields.iter().map(|(sql, _)| *sql).collect::<Vec<_>>();
    if state {
        columns.push("state");
    }
    columns.push("seq");
    columns.push("body");
    let sql = format!("SELECT {} FROM {table} WHERE {filter}", columns.join(","));
    let mut statement = connection.prepare(&sql).map_err(failure)?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(values))
        .map_err(failure)?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let indexed = fields
            .iter()
            .enumerate()
            .map(|(index, (_, name))| Ok((*name, json(row.get(index).map_err(failure)?)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let tag: Option<String> = state
            .then(|| row.get(fields.len()).map_err(failure))
            .transpose()?;
        let body: Vec<u8> = row.get(columns.len() - 1).map_err(failure)?;
        let sequence: i64 = row.get(columns.len() - 2).map_err(failure)?;
        records.push((
            u64::try_from(sequence).map_err(failure)?,
            codec::unpack(&body, indexed, tag.as_deref())?,
        ));
    }
    Ok(records)
}

pub(super) fn input(
    connection: &Connection,
    key: i64,
    id: ClientInputId,
) -> Result<Option<PendingInput>, String> {
    Ok(read(
        connection,
        "inputs",
        "thread=?1 AND id=?2",
        vec![key.into(), id.to_string().into()],
        INPUT,
        true,
    )?
    .into_iter()
    .next()
    .map(|(_, input)| input))
}

pub(super) fn turn(connection: &Connection, key: i64, id: &TurnId) -> Result<Option<Turn>, String> {
    Ok(read(
        connection,
        "turns",
        "thread=?1 AND id=?2 AND state!='rolled_back'",
        vec![key.into(), id.to_string().into()],
        TURN,
        true,
    )?
    .into_iter()
    .next()
    .map(|(_, turn)| turn))
}

pub(super) fn item(
    connection: &Connection,
    key: i64,
    id: &agent_protocol::ids::ItemId,
) -> Result<Option<decision::Item>, String> {
    Ok(read(
        connection,
        "items",
        "thread=?1 AND id=?2",
        vec![key.into(), id.to_string().into()],
        ITEM,
        false,
    )?
    .into_iter()
    .next()
    .map(|(_, item)| item))
}

pub(super) fn item_row(
    connection: &Connection,
    key: i64,
    id: &SessionRef,
    item: &agent_protocol::ids::ItemId,
) -> Result<Option<Row>, String> {
    let Some((version, item)) = read::<Item>(
        connection,
        "items",
        "thread=?1 AND id=?2",
        vec![key.into(), item.to_string().into()],
        ITEM,
        false,
    )?
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let ordinal: i64 = connection
        .query_row(
            "SELECT ordinal FROM turns WHERE thread=?1 AND id=?2",
            params![key, item.turn.as_str()],
            |row| row.get(0),
        )
        .map_err(failure)?;
    Ok(Some(Row {
        key: Key::Item {
            thread: id.clone(),
            id: item.id.clone(),
        },
        version,
        position: Position::Item {
            turn: item.turn.clone(),
            ordinal: ordinal.try_into().map_err(failure)?,
            position: item.position,
        },
        content: Content::Full(Arc::new(Record::Item(item.into()))),
    }))
}

pub(super) fn older_window(
    connection: &Connection,
    key: i64,
    current: Window,
) -> Result<Window, String> {
    page_floor(connection, key, Some(current))
}

// T3 threadHistoryPaging: ten user turns, 500 items when a user turn is
// present, otherwise 75 items inside a long turn, and at most 1 MiB per page.
fn page_floor(connection: &Connection, key: i64, before: Option<Window>) -> Result<Window, String> {
    let end = before.unwrap_or(Window {
        turn: i64::MAX as u64,
        item: i64::MAX as u64,
    });
    let raw_floor: Option<i64> = connection.query_row("SELECT min(ordinal) FROM (SELECT ordinal FROM turns WHERE thread=?1 AND state!='rolled_back' AND ordinal<=?2 ORDER BY ordinal DESC LIMIT 150)", params![key, integer(end.turn)?], |row| row.get(0)).map_err(failure)?;
    let Some(raw_floor) = raw_floor else {
        return Ok(Window { turn: 0, item: 0 });
    };
    let mut statement = connection.prepare("SELECT i.id,i.turn_id,i.position,i.status,i.body,t.ordinal FROM items i JOIN turns t ON t.thread=i.thread AND t.id=i.turn_id WHERE i.thread=?1 AND t.state!='rolled_back' AND t.ordinal>=?2 AND (t.ordinal<?3 OR (t.ordinal=?3 AND i.position<?4)) ORDER BY t.ordinal DESC,i.position DESC LIMIT 500").map_err(failure)?;
    let mut saved = statement
        .query(params![
            key,
            raw_floor,
            integer(end.turn)?,
            integer(end.item)?
        ])
        .map_err(failure)?;
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut user_turns = BTreeSet::new();
    while let Some(row) = saved.next().map_err(failure)? {
        if user_turns.len() >= 10 {
            break;
        }
        let id: String = row.get(0).map_err(failure)?;
        let turn: String = row.get(1).map_err(failure)?;
        let position: i64 = row.get(2).map_err(failure)?;
        let status: String = row.get(3).map_err(failure)?;
        let body: Vec<u8> = row.get(4).map_err(failure)?;
        let ordinal: i64 = row.get(5).map_err(failure)?;
        let item: Item = codec::unpack(
            &body,
            vec![
                ("id", Value::String(id)),
                ("turn", Value::String(turn.clone())),
                ("position", position.into()),
                ("status", Value::String(status)),
            ],
            None,
        )?;
        let mut item = decision::records::Item::from(item);
        let mut cost = protocol::encode(item.content.as_ref())
            .map_err(failure)?
            .len();
        // Large bodies travel separately; they must not consume the window's
        // entire inline budget and hide the preceding messages.
        if cost > agent_protocol::models::MAX_INLINE_ITEM_BYTES {
            Arc::make_mut(&mut item.content).defer();
            cost = protocol::encode(item.content.as_ref())
                .map_err(failure)?
                .len();
        }
        if !selected.is_empty() && bytes.saturating_add(cost) > 1_048_576 {
            break;
        }
        bytes = bytes.saturating_add(cost);
        if matches!(
            item.content.body(),
            agent_protocol::items::ItemBody::UserMessage { .. }
        ) {
            user_turns.insert(turn);
        }
        selected.push(Window {
            turn: u64::try_from(ordinal).map_err(failure)?,
            item: u64::try_from(position).map_err(failure)?,
        });
    }
    if user_turns.is_empty() {
        selected.truncate(75);
    }
    let floor = selected.last().copied().unwrap_or(Window {
        turn: u64::try_from(raw_floor).map_err(failure)?,
        item: 0,
    });
    let older: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE thread=?1 AND state!='rolled_back' AND ordinal<?2) OR EXISTS(SELECT 1 FROM items i JOIN turns t ON t.thread=i.thread AND t.id=i.turn_id WHERE i.thread=?1 AND t.state!='rolled_back' AND t.ordinal=?2 AND i.position<?3)", params![key, integer(floor.turn)?, integer(floor.item)?], |row| row.get(0)).map_err(failure)?;
    Ok(if older {
        floor
    } else {
        Window { turn: 0, item: 0 }
    })
}

pub(super) fn headers(
    connection: &Connection,
    after: u64,
    loaded_only: bool,
) -> Result<Vec<(SessionRef, Header, u64)>, String> {
    let mut statement=connection.prepare(
        "SELECT t.provider,t.id,t.seq,t.cwd,p.provider,p.id,t.updated_at,t.body FROM threads t LEFT JOIN threads p ON p.key=t.parent WHERE t.seq>?1 AND t.loaded>=0 AND (?2=0 OR t.loaded=1) ORDER BY t.updated_at DESC,t.key"
    ).map_err(failure)?;
    let entries = statement
        .query_map(params![integer(after)?, loaded_only], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Vec<u8>>(7)?,
            ))
        })
        .map_err(failure)?;
    entries
        .map(|entry| {
            let (kind, id, version, cwd, parent_kind, parent_id, updated, body) =
                entry.map_err(failure)?;
            let parent = parent_id
                .map(|id| {
                    SessionRef::new(parent_kind.as_deref().map(provider).transpose()?, id)
                        .map_err(failure)
                })
                .transpose()?;
            let header = codec::read_header(&body, cwd.as_deref(), parent, updated)?.0;
            Ok((
                SessionRef::new(kind.as_deref().map(provider).transpose()?, id).map_err(failure)?,
                header,
                version.try_into().map_err(failure)?,
            ))
        })
        .collect()
}

pub(super) fn header(connection: &Connection, key: i64) -> Result<(Header, Counters), String> {
    let (cwd, parent_provider, parent_id, updated, bytes) = connection.query_row(
        "SELECT t.cwd,p.provider,p.id,t.updated_at,t.body FROM threads t LEFT JOIN threads p ON p.key=t.parent WHERE t.key=?1",
        [key], |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,i64>(3)?,row.get::<_,Vec<u8>>(4)?)),
    ).map_err(failure)?;
    let parent = parent_id
        .map(|id| {
            SessionRef::new(parent_provider.as_deref().map(provider).transpose()?, id)
                .map_err(failure)
        })
        .transpose()?;
    codec::read_header(&bytes, cwd.as_deref(), parent, updated)
}

pub(super) fn load(connection: &Connection, key: i64, process: Uuid) -> Result<State, String> {
    let (header, counters) = header(connection, key)?;
    let mut state = State::new(header.state);
    state.counters = counters;
    for (_, input) in read::<PendingInput>(
        connection,
        "inputs",
        "thread=?1",
        vec![key.into()],
        INPUT,
        true,
    )? {
        state.inputs.insert(input.id.clone(), input);
    }
    for (_, request) in read::<Request>(
        connection,
        "requests",
        "thread=?1",
        vec![key.into()],
        REQUEST,
        true,
    )? {
        state.requests.insert(request.id.clone(), request);
    }
    let mut referenced = state
        .requests
        .values()
        .map(|request| request.turn.clone())
        .collect::<BTreeSet<_>>();
    for input in state.inputs.values() {
        match &input.state {
            InputState::Delivering(delivery) | InputState::Unknown { delivery, .. } => {
                referenced.insert(delivery.attempt.turn.clone());
            }
            _ => {}
        }
    }
    let mut filter = "thread=?1 AND (state IN ('running','waiting') OR ordinal=(SELECT max(ordinal) FROM turns WHERE thread=?1 AND state!='rolled_back')".to_owned();
    let mut values = vec![SqlValue::Integer(key)];
    for id in referenced {
        filter.push_str(&format!(" OR id=?{}", values.len() + 1));
        values.push(SqlValue::Text(id.to_string()));
    }
    filter.push(')');
    for (_, turn) in read::<Turn>(connection, "turns", &filter, values, TURN, true)? {
        state.turns.insert(turn.id.clone(), turn);
    }
    let filter = "thread=?1 AND status='running' AND turn_id IN (SELECT id FROM turns WHERE thread=?1 AND state IN ('running','waiting'))";
    for (_, item) in read::<Item>(connection, "items", filter, vec![key.into()], ITEM, false)? {
        state.items.insert(item.id.clone(), item);
    }
    load_bindings(connection, key, &mut state)?;
    let mut statement = connection.prepare("SELECT e.attempt,e.turn_id,s.provider,s.native_id FROM executions e JOIN sources s ON s.id=e.source_id WHERE e.thread=?1 AND e.current=1 AND e.process=?2").map_err(failure)?;
    let mut rows = statement
        .query(params![key, process.to_string()])
        .map_err(failure)?;
    while let Some(row) = rows.next().map_err(failure)? {
        let id: String = row.get(0).map_err(failure)?;
        let turn: String = row.get(1).map_err(failure)?;
        let kind: String = row.get(2).map_err(failure)?;
        let attempt = Attempt {
            id: Uuid::parse_str(&id).map_err(failure)?,
            turn: turn.into(),
            provider: provider(&kind)?,
            source: row.get(3).map_err(failure)?,
        };
        if state.turns.contains_key(&attempt.turn) {
            state.attempts.insert(attempt.turn.clone(), attempt);
        }
    }
    Ok(state)
}

fn load_bindings(connection: &Connection, key: i64, state: &mut State) -> Result<(), String> {
    let mut statement = connection.prepare("SELECT b.provider,b.state,b.body,s.native_id,b.covered FROM bindings b JOIN sources s ON s.id=b.source_id WHERE b.thread=?1").map_err(failure)?;
    let mut rows = statement.query([key]).map_err(failure)?;
    while let Some(row) = rows.next().map_err(failure)? {
        let kind: String = row.get(0).map_err(failure)?;
        let tag: String = row.get(1).map_err(failure)?;
        let body: Vec<u8> = row.get(2).map_err(failure)?;
        let source = row.get(3).map_err(failure)?;
        let binding = match tag.as_str() {
            "ready" => Binding::Ready {
                source,
                covered: u64::try_from(row.get::<_, i64>(4).map_err(failure)?).map_err(failure)?,
            },
            "aligning" => Binding::Aligning {
                source,
                action: protocol::decode(&body).map_err(failure)?,
            },
            "fork" => Binding::Fork {
                source,
                through: protocol::decode(&body).map_err(failure)?,
            },
            _ => return Err("invalid provider binding state".into()),
        };
        state.bindings.insert(provider(&kind)?, binding);
    }
    Ok(())
}

pub(super) fn source(
    transaction: &Transaction<'_>,
    key: i64,
    kind: ProviderKind,
    native: &str,
) -> Result<i64, String> {
    transaction.execute("INSERT INTO sources(thread,provider,native_id) VALUES (?1,?2,?3) ON CONFLICT(provider,native_id) DO NOTHING", params![key,kind.key(),native]).map_err(failure)?;
    let (id, owner): (i64, i64) = transaction
        .query_row(
            "SELECT id,thread FROM sources WHERE provider=?1 AND native_id=?2",
            params![kind.key(), native],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(failure)?;
    if owner != key {
        return Err("native source belongs to another conversation".into());
    }
    Ok(id)
}

fn write_input(
    transaction: &Transaction<'_>,
    key: i64,
    value: &PendingInput,
    sequence: u64,
    process: Uuid,
) -> Result<(), String> {
    save(transaction, key, "inputs", value, INPUT, true, sequence)?;
    if let InputState::Delivering(delivery) | InputState::Unknown { delivery, .. } = &value.state {
        // Delivery owns a durable private execution before any public
        // turn is confirmed. The same row is promoted on acknowledgement.
        let source = source(
            transaction,
            key,
            delivery.attempt.provider,
            &delivery.attempt.source,
        )?;
        transaction
            .execute(
                "UPDATE executions SET current=0 WHERE thread=?1 AND turn_id=?2 AND attempt!=?3",
                params![
                    key,
                    delivery.attempt.turn.as_str(),
                    delivery.attempt.id.to_string()
                ],
            )
            .map_err(failure)?;
        transaction.execute("INSERT INTO executions(thread,turn_id,attempt,source_id,current,process,started_seq,submitted) VALUES (?1,?2,?3,?4,1,?5,?6,1) ON CONFLICT(thread,turn_id,attempt) DO UPDATE SET submitted=1 WHERE executions.process=excluded.process AND executions.source_id=excluded.source_id",params![key,delivery.attempt.turn.as_str(),delivery.attempt.id.to_string(),source,process.to_string(),integer(sequence)?]).map_err(failure)?;
        if matches!(
            value.state,
            InputState::Unknown {
                status: decision::UnknownState::Fenced,
                ..
            }
        ) {
            transaction
                .execute(
                    "UPDATE executions SET callbacks=0 WHERE attempt=?1",
                    [delivery.attempt.id.to_string()],
                )
                .map_err(failure)?;
        }
    }

    Ok(())
}

pub(super) struct WriteScope<'a> {
    pub key: i64,
    pub id: &'a SessionRef,
    pub sequence: u64,
    pub at: i64,
    pub process: Uuid,
}

pub(super) fn write(
    transaction: &Transaction<'_>,
    scope: &WriteScope<'_>,
    change: &Change,
    turns: &std::collections::BTreeMap<TurnId, Turn>,
    updated: &mut Vec<Row>,
    removed: &mut Vec<Removal<SessionRef>>,
) -> Result<(), String> {
    let &WriteScope {
        key,
        id,
        sequence,
        process,
        ..
    } = scope;
    // A failed or discarded delivery must retain its identity as fenced,
    // unless it belongs to an already materialized (possibly steered) turn.
    let retired = match change {
        Change::Input(value)
            if matches!(
                value.state,
                InputState::Failed(_) | InputState::Queued { .. }
            ) =>
        {
            Some(&value.id)
        }
        Change::RemoveInput(id) => Some(id),
        _ => None,
    };
    if let Some(input) = retired {
        let previous = read::<PendingInput>(
            transaction,
            "inputs",
            "thread=?1 AND id=?2",
            vec![key.into(), input.to_string().into()],
            INPUT,
            true,
        )?
        .pop();
        if let Some((
            _,
            PendingInput {
                state: InputState::Delivering(delivery) | InputState::Unknown { delivery, .. },
                ..
            },
        )) = previous
            && !turns.contains_key(&delivery.attempt.turn)
        {
            transaction
                .execute(
                    "UPDATE executions SET current=0,callbacks=0 WHERE thread=?1 AND attempt=?2",
                    params![key, delivery.attempt.id.to_string()],
                )
                .map_err(failure)?;
        }
    }
    let (entity, position, record) = match change {
        Change::Input(value) => {
            write_input(transaction, key, value, sequence, process)?;

            (
                Key::Input {
                    thread: id.clone(),
                    id: value.id.clone(),
                },
                Position::Unordered,
                Record::Input(value.clone().into()),
            )
        }
        Change::Turn(value) => {
            save(transaction, key, "turns", value, TURN, true, sequence)?;
            if value.state == decision::TurnState::RolledBack {
                transaction
                    .execute(
                        "INSERT INTO removals(seq,kind,id,thread,at) VALUES (?1,'turn',?2,?3,?4)",
                        params![integer(sequence)?, value.id.as_str(), key, scope.at],
                    )
                    .map_err(failure)?;
                removed.push(Removal {
                    key: Key::Turn {
                        thread: id.clone(),
                        id: value.id.clone(),
                    },
                    version: sequence,
                });
                return Ok(());
            }
            (
                Key::Turn {
                    thread: id.clone(),
                    id: value.id.clone(),
                },
                Position::Turn {
                    ordinal: value.ordinal,
                },
                Record::Turn(value.clone().into()),
            )
        }
        Change::Item(value) => {
            save(transaction, key, "items", value, ITEM, false, sequence)?;
            let turn = turns.get(&value.turn).ok_or("item turn is missing")?;
            (
                Key::Item {
                    thread: id.clone(),
                    id: value.id.clone(),
                },
                Position::Item {
                    turn: value.turn.clone(),
                    ordinal: turn.ordinal,
                    position: value.position,
                },
                Record::Item(value.clone().into()),
            )
        }
        Change::Request(value) => {
            save(transaction, key, "requests", value, REQUEST, true, sequence)?;
            (
                Key::Request {
                    thread: id.clone(),
                    id: value.id.clone(),
                },
                Position::Unordered,
                Record::Request(value.clone().into()),
            )
        }
        Change::Thread(_) | Change::Counters(_) => return Ok(()),
        Change::RemoveInput(value) => {
            return remove(
                transaction,
                scope,
                "inputs",
                "input",
                value.as_str(),
                removed,
            );
        }
        Change::RemoveItem(value) => {
            return remove(transaction, scope, "items", "item", value.as_str(), removed);
        }
        Change::RemoveRequest(value) => {
            return remove(
                transaction,
                scope,
                "requests",
                "request",
                value.as_str(),
                removed,
            );
        }
        Change::WorkspaceStrategy(strategy) => {
            let (mut header, counters) = header(transaction, key)?;
            let workspace = header
                .details
                .workspace
                .as_mut()
                .ok_or("workspace setup is unavailable")?;
            workspace.strategy = Some(strategy.clone());
            transaction
                .execute(
                    "UPDATE threads SET body=?1 WHERE key=?2",
                    params![codec::header(&header, &counters)?, key],
                )
                .map_err(failure)?;
            return Ok(());
        }
        Change::CancelSetup(generation) => {
            let (mut header, counters) = header(transaction, key)?;
            if let Some(workspace) = header
                .details
                .workspace
                .as_mut()
                .filter(|workspace| workspace.generation == *generation)
            {
                if let Some(setup) = &mut workspace.setup {
                    setup.finish(
                        agent_protocol::workspace::SetupPhase::Cancelled,
                        None,
                        scope.at.max(0) as u64,
                    );
                }
                transaction
                    .execute(
                        "UPDATE threads SET body=?1 WHERE key=?2",
                        params![codec::header(&header, &counters)?, key],
                    )
                    .map_err(failure)?;
            }
            return Ok(());
        }

        Change::Binding { provider, value } => {
            if let Some(value) = value {
                let (native, tag, covered, body) = match value {
                    Binding::Ready { source, covered } => (
                        source,
                        "ready",
                        Some(integer(*covered)?),
                        protocol::encode(()).map_err(failure)?,
                    ),
                    Binding::Aligning { source, action } => (
                        source,
                        "aligning",
                        None,
                        protocol::encode(action).map_err(failure)?,
                    ),
                    Binding::Fork { source, through } => (
                        source,
                        "fork",
                        None,
                        protocol::encode(through).map_err(failure)?,
                    ),
                };
                let source = if matches!(value, Binding::Fork { .. }) {
                    transaction
                        .query_row(
                            "SELECT id FROM sources WHERE provider=?1 AND native_id=?2",
                            params![provider.key(), native],
                            |row| row.get(0),
                        )
                        .map_err(failure)?
                } else {
                    source(transaction, key, *provider, native)?
                };
                transaction.execute("INSERT INTO bindings(thread,provider,source_id,state,covered,body) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(thread,provider) DO UPDATE SET source_id=excluded.source_id,state=excluded.state,covered=excluded.covered,body=excluded.body", params![key,provider.key(),source,tag,covered,body]).map_err(failure)?;
            } else {
                transaction
                    .execute(
                        "DELETE FROM bindings WHERE thread=?1 AND provider=?2",
                        params![key, provider.key()],
                    )
                    .map_err(failure)?;
            }
            return Ok(());
        }
        Change::Attempt { turn, value } => {
            transaction
                .execute(
                    "UPDATE executions SET current=0 WHERE thread=?1 AND turn_id=?2",
                    params![key, turn.as_str()],
                )
                .map_err(failure)?;
            if let Some(value) = value {
                let source = source(transaction, key, value.provider, &value.source)?;
                let count=transaction.execute("INSERT INTO executions(thread,turn_id,attempt,source_id,current,process,started_seq) VALUES (?1,?2,?3,?4,1,?5,?6) ON CONFLICT(thread,turn_id,attempt) DO UPDATE SET current=1 WHERE executions.source_id=excluded.source_id AND executions.process=excluded.process",params![key,turn.as_str(),value.id.to_string(),source,process.to_string(),integer(sequence)?]).map_err(failure)?;
                if count != 1 {
                    return Err("attempt generation is already owned".into());
                }
            }
            return Ok(());
        }
    };
    updated.push(Row {
        key: entity,
        version: sequence,
        position,
        content: Content::Full(Arc::new(record)),
    });
    Ok(())
}

pub(super) fn write_header(
    transaction: &Transaction<'_>,
    scope: &WriteScope<'_>,
    state: &State,
    admitted: Option<(decision::records::Selection, Option<String>)>,
    previous: Option<&Header>,
    steers: Option<bool>,
) -> Result<Option<Row>, String> {
    let &WriteScope {
        key,
        id,
        sequence,
        at,
        ..
    } = scope;
    let (mut header, counters) = header(transaction, key)?;
    let external_status = (state.active_turn().is_none()
        && (header.details.activity.unobserved_running()
            || matches!(
                header.details.activity.status,
                agent_protocol::execution::SessionStatus::Unknown
                    | agent_protocol::execution::SessionStatus::Unavailable
            )))
    .then_some(header.details.activity.status);
    header.state = state.thread.clone();
    if let Some(steers) = steers {
        header.details.capabilities.steer = steers;
    }
    let target = state.steer_target(header.details.capabilities.steer);
    header.details.steerable = matches!(target, Some(decision::SteerTarget::Running(_)));
    header.details.skips_compaction = matches!(
        target,
        Some(decision::SteerTarget::CompactionTurn(_) | decision::SteerTarget::CompactionInput(_))
    );
    if let Some((selection, preview)) = admitted {
        header.details.selection = selection;
        if header.details.preview.is_none() {
            header.details.preview = preview.as_deref().and_then(decision::records::preview);
        }
    }
    let latest = state
        .turns
        .values()
        .filter(|turn| turn.state != decision::TurnState::RolledBack)
        .max_by_key(|turn| turn.ordinal);
    let completion = latest
        .filter(|turn| turn.state == decision::TurnState::Completed)
        .map(|turn| turn.id.clone())
        .or(header.details.activity.completion.clone());
    let waiting = state
        .requests
        .values()
        .any(|request| request.state == decision::RequestState::Awaiting);
    let approval = state.requests.values().any(|request| {
        request.state == decision::RequestState::Awaiting
            && matches!(
                request.body,
                agent_protocol::requests::RequestBody::Approval { .. }
                    | agent_protocol::requests::RequestBody::Permission { .. }
            )
    });
    let preparing = state.inputs.values().any(|input| {
        matches!(
            input.state,
            decision::InputState::Preparing { .. }
                | decision::InputState::Delivering(_)
                | decision::InputState::Unknown {
                    status: decision::UnknownState::Live | decision::UnknownState::Resolving { .. },
                    ..
                }
        )
    });
    let running = state.turns.values().any(|turn| {
        matches!(
            turn.state,
            decision::TurnState::Running { .. }
                | decision::TurnState::Waiting(decision::Wait::Retry { .. })
        )
    });
    header.details.activity = decision::records::Activity {
        stale: false,
        completion,
        waiting,
        approval,
        latest: latest.map(|turn| match turn.state {
            decision::TurnState::Running { .. }
            | decision::TurnState::Waiting(decision::Wait::Retry { .. }) => {
                agent_protocol::execution::TurnStatus::Running
            }
            decision::TurnState::Completed
            | decision::TurnState::Waiting(decision::Wait::Background { .. }) => {
                agent_protocol::execution::TurnStatus::Completed
            }
            decision::TurnState::Interrupted | decision::TurnState::RolledBack => {
                agent_protocol::execution::TurnStatus::Interrupted
            }
            decision::TurnState::Failed(_) => agent_protocol::execution::TurnStatus::Failed,
        }),
        status: if running {
            agent_protocol::execution::SessionStatus::Running
        } else if preparing {
            agent_protocol::execution::SessionStatus::Preparing
        } else {
            external_status.unwrap_or(agent_protocol::execution::SessionStatus::Idle)
        },
        background: state
            .turns
            .values()
            .flat_map(|turn| match &turn.state {
                decision::TurnState::Waiting(decision::Wait::Background { tasks }) => tasks.clone(),
                _ => Vec::new(),
            })
            .collect(),
    };
    if matches!(
        header.details.activity.status,
        agent_protocol::execution::SessionStatus::Running
            | agent_protocol::execution::SessionStatus::Preparing
    ) {
        header.details.read_through = header.details.activity.completion.clone();
    }
    if header
        .details
        .activity
        .completion
        .as_ref()
        .is_some_and(|id| {
            state
                .turns
                .get(id)
                .is_some_and(|turn| turn.state == decision::TurnState::RolledBack)
        })
    {
        header.details.activity.completion = None;
    }
    // T3 ShellStream suppresses timestamp-only thread updates for five seconds.
    // Keep one row/version for list and conversation consumers in that window.
    if let Some(previous) = previous
        && previous == &header
        && at.saturating_sub(previous.details.updated_at) < 5_000
    {
        header.details.updated_at = previous.details.updated_at;
    } else {
        header.details.updated_at = at;
    }
    if previous == Some(&header) {
        // Counters belong to the writer, but aren't part of the public header.
        if counters != state.counters {
            transaction
                .execute(
                    "UPDATE threads SET body=?1 WHERE key=?2",
                    params![codec::header(&header, &state.counters)?, key],
                )
                .map_err(failure)?;
        }
        return Ok(None);
    }
    transaction
        .execute(
            "UPDATE threads SET seq=?1,cwd=?2,updated_at=?3,body=?4 WHERE key=?5",
            params![
                integer(sequence)?,
                header.state.workspace.cwd(),
                at,
                codec::header(&header, &state.counters)?,
                key
            ],
        )
        .map_err(failure)?;
    Ok(Some(Row {
        key: Key::Thread(id.clone()),
        version: sequence,
        position: Position::Unordered,
        content: Content::Full(Arc::new(Record::Thread(header))),
    }))
}

pub(super) fn hydrate(
    connection: &Connection,
    key: i64,
    process: Uuid,
    mut state: State,
    event: &Event,
) -> Result<State, String> {
    if let Event::Command {
        command: decision::Command::Rewind(id),
        ..
    } = event
    {
        for (_, turn) in read::<Turn>(
            connection,
            "turns",
            "thread=?1 AND ordinal >= (SELECT ordinal FROM turns WHERE thread=?1 AND id=?2)",
            vec![key.into(), id.to_string().into()],
            TURN,
            true,
        )? {
            state.turns.insert(turn.id.clone(), turn);
        }
    }
    let item = match event {
        Event::Provider {
            native_items: _,
            native_turn: _,
            attempt,
            observation,
        } => {
            let execution = connection.query_row("SELECT e.turn_id,s.provider,s.native_id FROM executions e JOIN sources s ON s.id=e.source_id WHERE e.thread=?1 AND e.attempt=?2 AND e.current=1 AND e.process=?3",params![key,attempt.to_string(),process.to_string()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))).optional().map_err(failure)?;
            if let Some((turn, kind, source)) = execution {
                let turns = read::<Turn>(
                    connection,
                    "turns",
                    "thread=?1 AND id=?2",
                    vec![key.into(), turn.clone().into()],
                    TURN,
                    true,
                )?;
                for (_, record) in turns {
                    state.turns.insert(record.id.clone(), record);
                }
                let turn: TurnId = turn.into();
                if state.turns.contains_key(&turn) {
                    state.attempts.insert(
                        turn.clone(),
                        Attempt {
                            id: *attempt,
                            turn,
                            provider: provider(&kind)?,
                            source,
                        },
                    );
                }
            }
            match observation {
                decision::Observation::Item { id, .. } | decision::Observation::RemoveItem(id) => {
                    Some(id.to_string())
                }
                _ => None,
            }
        }
        Event::Command {
            command: decision::Command::Submit { id, .. },
            ..
        } => Some(id.to_string()),
        _ => None,
    };
    if let Some(id) = item {
        for (_, record) in read::<Item>(
            connection,
            "items",
            "thread=?1 AND id=?2 COLLATE NOCASE",
            vec![key.into(), id.into()],
            ITEM,
            false,
        )? {
            if !state.turns.contains_key(&record.turn) {
                for (_, turn) in read::<Turn>(
                    connection,
                    "turns",
                    "thread=?1 AND id=?2",
                    vec![key.into(), record.turn.to_string().into()],
                    TURN,
                    true,
                )? {
                    state.turns.insert(turn.id.clone(), turn);
                }
            }
            state.items.insert(record.id.clone(), record);
        }
    }
    Ok(state)
}

fn remove(
    transaction: &Transaction<'_>,
    scope: &WriteScope<'_>,
    table: &str,
    kind: &str,
    record: &str,
    removed: &mut Vec<Removal<SessionRef>>,
) -> Result<(), String> {
    let &WriteScope {
        key,
        id,
        sequence,
        at,
        ..
    } = scope;
    let deleted = transaction
        .execute(
            &format!("DELETE FROM {table} WHERE thread=?1 AND id=?2"),
            params![key, record],
        )
        .map_err(failure)?;
    if deleted == 0 {
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO removals(seq,kind,id,thread,at) VALUES (?1,?2,?3,?4,?5)",
            params![integer(sequence)?, kind, record, key, at],
        )
        .map_err(failure)?;
    let entity = match kind {
        "input" => Key::Input {
            thread: id.clone(),
            id: record.to_owned().into(),
        },
        "request" => Key::Request {
            thread: id.clone(),
            id: record.to_owned().into(),
        },
        "item" => Key::Item {
            thread: id.clone(),
            id: record.to_owned().into(),
        },
        _ => unreachable!("owned removable record"),
    };
    removed.push(Removal {
        key: entity,
        version: sequence,
    });
    Ok(())
}

pub(super) fn window(
    connection: &Connection,
    key: i64,
    requested: Option<Window>,
) -> Result<Window, String> {
    let latest: Option<i64> = connection
        .query_row(
            "SELECT max(ordinal) FROM turns WHERE thread=?1 AND state!='rolled_back'",
            [key],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let Some(latest) = latest else {
        return Ok(Window { turn: 0, item: 0 });
    };
    let latest = u64::try_from(latest).map_err(failure)?;
    if let Some(requested) = requested.filter(|floor| floor.turn <= latest) {
        let last:Option<i64>=connection.query_row("SELECT max(i.position) FROM items i JOIN turns t ON t.thread=i.thread AND t.id=i.turn_id WHERE i.thread=?1 AND t.ordinal=?2",params![key,integer(requested.turn)?],|row|row.get(0)).map_err(failure)?;
        if last.is_none_or(|last| u64::try_from(last).is_ok_and(|last| requested.item <= last)) {
            return Ok(requested);
        }
    }
    page_floor(connection, key, None)
}

pub(super) fn snapshot(
    connection: &Connection,
    key: i64,
    id: &SessionRef,
    window: Window,
) -> Result<Vec<Row>, String> {
    let (header, _) = header(connection, key)?;
    let version: i64 = connection
        .query_row("SELECT seq FROM threads WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .map_err(failure)?;
    let mut rows = vec![Row {
        key: Key::Thread(id.clone()),
        version: u64::try_from(version).map_err(failure)?,
        position: Position::Unordered,
        content: Content::Full(Arc::new(Record::Thread(header))),
    }];
    let mut ordinals = std::collections::BTreeMap::new();
    for (version, turn) in read::<Turn>(
        connection,
        "turns",
        "thread=?1 AND ordinal>=?2 AND state!='rolled_back'",
        vec![key.into(), integer(window.turn)?.into()],
        TURN,
        true,
    )? {
        ordinals.insert(turn.id.clone(), turn.ordinal);
        rows.push(Row {
            key: Key::Turn {
                thread: id.clone(),
                id: turn.id.clone(),
            },
            version,
            position: Position::Turn {
                ordinal: turn.ordinal,
            },
            content: Content::Full(Arc::new(Record::Turn(turn.into()))),
        });
    }
    for (version, item) in read::<Item>(
        connection,
        "items",
        "thread=?1 AND turn_id IN (SELECT id FROM turns WHERE thread=?1 AND ordinal>=?2 AND state!='rolled_back') AND (position>=?3 OR turn_id IN (SELECT id FROM turns WHERE thread=?1 AND ordinal>?2))",
        vec![
            key.into(),
            integer(window.turn)?.into(),
            integer(window.item)?.into(),
        ],
        ITEM,
        false,
    )? {
        let ordinal = *ordinals.get(&item.turn).ok_or("item turn is missing")?;
        rows.push(Row {
            key: Key::Item {
                thread: id.clone(),
                id: item.id.clone(),
            },
            version,
            position: Position::Item {
                turn: item.turn.clone(),
                ordinal,
                position: item.position,
            },
            content: Content::Full(Arc::new(Record::Item(item.into()))),
        });
    }
    for (version, input) in read::<PendingInput>(
        connection,
        "inputs",
        "thread=?1",
        vec![key.into()],
        INPUT,
        true,
    )? {
        rows.push(Row {
            key: Key::Input {
                thread: id.clone(),
                id: input.id.clone(),
            },
            version,
            position: Position::Unordered,
            content: Content::Full(Arc::new(Record::Input(input.into()))),
        });
    }
    for (version, request) in read::<Request>(
        connection,
        "requests",
        "thread=?1",
        vec![key.into()],
        REQUEST,
        true,
    )? {
        rows.push(Row {
            key: Key::Request {
                thread: id.clone(),
                id: request.id.clone(),
            },
            version,
            position: Position::Unordered,
            content: Content::Full(Arc::new(Record::Request(request.into()))),
        });
    }
    Ok(rows)
}

pub(super) fn removals(
    connection: &Connection,
    key: i64,
    id: &SessionRef,
    after: u64,
) -> Result<Vec<Removal<SessionRef>>, String> {
    let mut statement = connection
        .prepare("SELECT kind,id,seq FROM removals WHERE thread=?1 AND seq>?2")
        .map_err(failure)?;
    let mut saved = statement
        .query(params![key, integer(after)?])
        .map_err(failure)?;
    let mut removed = Vec::new();
    while let Some(row) = saved.next().map_err(failure)? {
        let kind: String = row.get(0).map_err(failure)?;
        let record: String = row.get(1).map_err(failure)?;
        let version: i64 = row.get(2).map_err(failure)?;
        let key = match kind.as_str() {
            "input" => Key::Input {
                thread: id.clone(),
                id: record.into(),
            },
            "turn" => Key::Turn {
                thread: id.clone(),
                id: record.into(),
            },
            "item" => Key::Item {
                thread: id.clone(),
                id: record.into(),
            },
            "request" => Key::Request {
                thread: id.clone(),
                id: record.into(),
            },
            _ => return Err("invalid conversation removal".into()),
        };
        removed.push(Removal {
            key,
            version: u64::try_from(version).map_err(failure)?,
        });
    }
    Ok(removed)
}

pub(super) fn bind_items(
    transaction: &Transaction<'_>,
    key: i64,
    attempt: Uuid,
    process: Uuid,
    items: &[(String, agent_protocol::ids::ItemId)],
) -> Result<(), String> {
    for (native, item) in items {
        transaction.execute("INSERT INTO item_natives(thread,turn_id,attempt,native_id,item_id) SELECT e.thread,e.turn_id,e.attempt,?1,?2 FROM executions e JOIN items i ON i.thread=e.thread AND i.turn_id=e.turn_id AND i.id=?2 WHERE e.thread=?3 AND e.attempt=?4 AND e.process=?5 AND e.current=1 ON CONFLICT(thread,turn_id,attempt,native_id) DO NOTHING", params![native,item.as_str(),key,attempt.to_string(),process.to_string()]).map_err(failure)?;
    }
    Ok(())
}
