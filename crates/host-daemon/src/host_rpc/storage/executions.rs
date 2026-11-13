//! Native callback correlation has the same owner and lifetime as its saved
//! execution. Runtime resources retain only active I/O and buffered output.
use super::*;

#[derive(Debug, Clone)]
pub(in crate::host_rpc) struct NativeExecution {
    pub attempt: decision::Attempt,
    pub native_turn: Option<TurnId>,
    pub submitted: bool,
    pub accepting: bool,
}

pub(super) fn read(
    connection: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<NativeExecution>, String> {
    let mut statement=connection.prepare(&format!("SELECT e.attempt,e.turn_id,s.provider,s.native_id,e.native_turn_id,e.submitted,(e.current AND e.callbacks AND e.process=?1) FROM executions e JOIN sources s ON s.id=e.source_id LEFT JOIN turns t ON t.thread=e.thread AND t.id=e.turn_id WHERE {filter}")).map_err(failure)?;
    let mut rows = statement.query(params).map_err(failure)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let id: String = row.get(0).map_err(failure)?;
        let turn: String = row.get(1).map_err(failure)?;
        let kind: String = row.get(2).map_err(failure)?;
        let native_turn: Option<String> = row.get(4).map_err(failure)?;
        let submitted: bool = row.get(5).map_err(failure)?;
        result.push(NativeExecution {
            attempt: decision::Attempt {
                id: Uuid::parse_str(&id).map_err(failure)?,
                turn: turn.into(),
                provider: provider(&kind)?,
                source: row.get(3).map_err(failure)?,
            },
            native_turn: native_turn.map(Into::into),
            submitted,
            accepting: row.get(6).map_err(failure)?,
        });
    }
    Ok(result)
}

/// The first native turn makes a previously pending Stop executable. Repeated
/// native snapshots update correlation without issuing another interrupt.
pub(super) fn bind_turn(
    connection: &Connection,
    key: i64,
    process: Uuid,
    (execution, native): (Uuid, TurnId),
    turns: &std::collections::BTreeMap<TurnId, decision::Turn>,
) -> Result<Option<decision::Action>, String> {
    let previous = read(
        connection,
        "e.thread=?2 AND e.attempt=?3",
        params![process.to_string(), key, execution.to_string()],
    )?
    .pop();
    connection
        .execute(
            "UPDATE executions SET native_turn_id=?1 WHERE thread=?2 AND attempt=?3 AND process=?4",
            params![
                native.as_str(),
                key,
                execution.to_string(),
                process.to_string()
            ],
        )
        .map_err(failure)?;
    let Some(previous) =
        previous.filter(|execution| execution.accepting && execution.native_turn.is_none())
    else {
        return Ok(None);
    };
    let Some(decision::Turn {
        state: decision::TurnState::Running {
            interrupt: Some(action),
        },
        ..
    }) = turns.get(&previous.attempt.turn)
    else {
        return Ok(None);
    };
    Ok(Some(decision::Action::Interrupt {
        attempt: previous.attempt,
        action: *action,
        background: false,
    }))
}

impl Storage {
    pub(in crate::host_rpc) fn execution(
        &self,
        target: &SessionRef,
        execution: Uuid,
    ) -> Result<Option<NativeExecution>, String> {
        self.with(|connection| {
            let key = thread_key(connection, target)?;
            Ok(read(
                connection,
                "e.thread=?2 AND e.attempt=?3",
                params![self.process.to_string(), key, execution.to_string()],
            )?
            .pop())
        })
    }
    pub(in crate::host_rpc) fn source_executions(
        &self,
        target: &SessionRef,
        native: &SessionRef,
    ) -> Result<Vec<NativeExecution>, String> {
        self.with(|connection| {
            let key=thread_key(connection,target)?;
            read(connection,"e.thread=?2 AND s.provider=?3 AND s.native_id=?4 AND e.current=1 AND e.callbacks=1 AND e.process=?1 AND (t.state IN ('running','waiting') OR t.id IS NULL)",params![self.process.to_string(),key,native.native_provider().map_err(failure)?.key(),native.id])
        })
    }
    pub(in crate::host_rpc) fn running_sources(
        &self,
        provider_kind: ProviderKind,
    ) -> Result<Vec<(SessionRef, SessionRef)>, String> {
        self.with(|connection| {
            let mut statement=connection.prepare("SELECT DISTINCT t.provider,t.id,s.provider,s.native_id FROM executions e JOIN threads t ON t.key=e.thread JOIN sources s ON s.id=e.source_id LEFT JOIN turns u ON u.thread=e.thread AND u.id=e.turn_id WHERE s.provider=?1 AND e.process=?2 AND e.current=1 AND e.callbacks=1 AND (u.state IN ('running','waiting') OR u.id IS NULL)").map_err(failure)?;
            let rows=statement.query_map(params![provider_kind.key(),self.process.to_string()],|row|Ok((row.get::<_,Option<String>>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?))).map_err(failure)?;
            rows.map(|row| {
                let (kind,id,native_kind,native_id)=row.map_err(failure)?;
                Ok((SessionRef {provider:kind.as_deref().map(provider).transpose()?,id},SessionRef {provider:Some(provider(&native_kind)?),id:native_id}))
            }).collect()
        })
    }
    pub(in crate::host_rpc) fn fence_provider(
        &self,
        provider_kind: ProviderKind,
    ) -> Result<(), String> {
        self.with(|connection| {
            connection.execute("UPDATE executions SET callbacks=0 WHERE process=?1 AND source_id IN (SELECT id FROM sources WHERE provider=?2)",params![self.process.to_string(),provider_kind.key()]).map_err(failure)?;
            Ok(())
        })
    }
    /// Delivery proof and fencing compete in one transaction. A proof that
    /// commits first keeps its execution; callbacks after this fence are stale.
    pub(in crate::host_rpc) fn fence_delivery(
        &self,
        target: &SessionRef,
        id: &ClientInputId,
        action: Uuid,
        attempt: &decision::Attempt,
    ) -> Result<Option<bool>, String> {
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let key=thread_key(&transaction,target)?;
            let state=rows::load(&transaction,key,self.process)?;
            if !state.permits(&decision::Action::FenceDelivery {id: id.clone(),action,attempt:attempt.clone()}) {return Ok(None);}
            transaction.execute("UPDATE executions SET callbacks=0 WHERE thread=?1 AND attempt=?2 AND process=?3",params![key,attempt.id.to_string(),self.process.to_string()]).map_err(failure)?;
            let unload=!state.source_in_use(attempt.provider,&attempt.source,attempt.id);
            transaction.commit().map_err(failure)?;
            Ok(Some(unload))
        })
    }

    pub(in crate::host_rpc) fn fence_execution(
        &self,
        target: &SessionRef,
        execution: Uuid,
    ) -> Result<(), String> {
        self.with(|connection| {
            let key=thread_key(connection,target)?;
            connection.execute("UPDATE executions SET callbacks=0 WHERE thread=?1 AND attempt=?2 AND process=?3",params![key,execution.to_string(),self.process.to_string()]).map_err(failure)?;
            Ok(())
        })
    }
}
