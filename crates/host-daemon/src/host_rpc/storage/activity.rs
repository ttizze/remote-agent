//! Completion receipts share the conversation transaction and header.
use super::*;
use agent_protocol::ids::TurnId;

impl Storage {
    pub(in crate::host_rpc) fn activity(
        &self,
    ) -> Result<agent_protocol::live_activity::TaskActivityState, String> {
        let Message::Snapshot { rows, .. } = self.list(None)? else {
            unreachable!("initial list is a snapshot")
        };
        Ok(decision::records::list_activity(rows.iter().filter_map(
            |row| {
                let record = match &row.content {
                    Content::Full(record) | Content::Summary(record) => record,
                };
                match (&row.key, record.as_ref()) {
                    (Key::Thread(id), Record::Thread(header)) => Some((id, header)),
                    _ => None,
                }
            },
        )))
    }
    pub(in crate::host_rpc) fn mark_read(
        &self,
        id: &SessionRef,
        turn: &TurnId,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let state = rows::load(&transaction, key, self.process)?;
            let (mut header, counters) = rows::header(&transaction, key)?;
            let mut current = cursor(&transaction)?;
            let mut changed = Vec::new();
            if header.details.activity.completion.as_ref() == Some(turn)
                && header.details.read_through.as_ref() != Some(turn)
            {
                header.details.read_through = Some(turn.clone());
                current = Self::advance(&transaction)?;
                transaction
                    .execute(
                        "UPDATE threads SET seq=?1,body=?2 WHERE key=?3",
                        params![
                            integer(current.sequence)?,
                            codec::header(&header, &counters)?,
                            key
                        ],
                    )
                    .map_err(failure)?;
                changed.push(Row {
                    key: Key::Thread(id.clone()),
                    version: current.sequence,
                    position: Position::Unordered,
                    content: Content::Full(Arc::new(Record::Thread(header))),
                });
            }
            transaction.commit().map_err(failure)?;
            Ok(Committed {
                state: Some(state),
                step: Step {
                    outcome: Outcome::Accepted,
                    ..Default::default()
                },
                cursor: current,
                rows: changed,
                removed: Vec::new(),
                reset: false,
            })
        })
    }
}

pub(super) fn native_status(
    connection: &Transaction<'_>,
    target: &SessionRef,
    (provider, source, status): (ProviderKind, &str, agent_protocol::models::SessionStatus),
    active: Option<(bool, bool)>,
    activity: &mut decision::records::Activity,
    now: i64,
) -> Result<bool, String> {
    use agent_protocol::models::SessionStatus;
    if status == SessionStatus::Idle && active == Some((true, false)) {
        imports::queue(
            connection,
            target,
            &SessionRef::new(provider, source.to_owned()).map_err(failure)?,
            now,
        )?;
    }
    if active.is_none()
        && activity.status != SessionStatus::Preparing
        && activity.latest != Some(agent_protocol::execution::TurnStatus::Running)
        && matches!(status, SessionStatus::Idle | SessionStatus::Running)
    {
        let changed = activity.status != status;
        activity.status = status;
        return Ok(changed);
    }
    Ok(false)
}
