//! Native discovery writes small headers; opening owns history import.
use super::*;

impl Storage {
    pub(in crate::host_rpc) fn catalog_failed(
        &self,
        claim: &jobs::Claim,
        provider: ProviderKind,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let current:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='running' AND attempts=?2)",params![claim.id.to_string(),integer(claim.attempt)?],|row|row.get(0)).map_err(failure)?;
            if !current {return Ok(())}
            let keys={
                let mut statement=transaction.prepare("SELECT key FROM threads WHERE provider=?1 AND loaded=0").map_err(failure)?;
                statement.query_map([provider.key()],|row|row.get::<_,i64>(0)).map_err(failure)?.collect::<Result<Vec<_>,_>>().map_err(failure)?
            };
            let mut sequence=None;
            for key in keys {
                let (mut header,counters)=rows::header(&transaction,key)?;
                if header.details.activity.stale {continue}
                let sequence=match sequence {Some(sequence)=>sequence,None=>{let next=Self::advance(&transaction)?.sequence;sequence=Some(next);next}};
                header.details.activity.stale=true;
                transaction.execute("UPDATE threads SET seq=?1,body=?2 WHERE key=?3",params![integer(sequence)?,codec::header(&header,&counters)?,key]).map_err(failure)?;
            }
            transaction.commit().map_err(failure)
        })
    }
    pub(in crate::host_rpc) fn catalog_complete(&self) -> Result<bool, String> {
        self.with(|connection| {
            Ok(list::catalogs(connection, 0)?.iter().all(|row| {
                let (Content::Full(record) | Content::Summary(record)) = &row.content;
                matches!(
                    record.as_ref(),
                    Record::Catalog(decision::records::Catalog::Ready)
                )
            }))
        })
    }
    pub(in crate::host_rpc) fn loaded(&self, id: &SessionRef) -> Result<bool, String> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT loaded=1 FROM threads WHERE provider IS ?1 AND id=?2",
                    params![id.provider.map(ProviderKind::key), id.id],
                    |row| row.get(0),
                )
                .optional()
                .map(|loaded| loaded.unwrap_or(false))
                .map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn queue_catalog(
        &self,
        provider: ProviderKind,
        at: i64,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let lane = format!("catalog:{}", provider.key());
            let pending: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE lane=?1)",
                    [&lane],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if !pending {
                jobs::enqueue(
                    &transaction,
                    Uuid::new_v4(),
                    &lane,
                    at,
                    &jobs::Work::Catalog {
                        provider,
                        cursor: None,
                        seen: Vec::new(),
                        listed: Default::default(),
                        progress: jobs::CatalogProgress::Scanning,
                    },
                )?;
            } else {
                // A refresh advances a sleeping scan without replacing an
                // in-flight claim or restarting an unfinished page walk.
                transaction.execute("UPDATE jobs SET run_after=MIN(run_after,?1) WHERE lane=?2 AND state='pending'",params![at,lane]).map_err(failure)?;
                let (state, body): (String, Vec<u8>) = transaction.query_row("SELECT state,body FROM jobs WHERE lane=?1", [&lane], |row|Ok((row.get(0)?,row.get(1)?))).map_err(failure)?;
                let mut work: jobs::Work = codec::enum_record("catalog", &body)?;
                if let jobs::Work::Catalog {cursor, progress, ..} = &mut work {
                    *progress=if state=="running" || cursor.is_some() {jobs::CatalogProgress::Refresh} else {jobs::CatalogProgress::Scanning};
                    let (_,body)=codec::enum_parts(&work)?;
                    transaction.execute("UPDATE jobs SET body=?1 WHERE lane=?2",params![body,lane]).map_err(failure)?;
                }
            }
            let sequence=Self::advance(&transaction)?;
            transaction.execute("UPDATE jobs SET seq=?1,last_error=NULL WHERE lane=?2",params![integer(sequence.sequence)?,lane]).map_err(failure)?;
            transaction.commit().map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn catalog_page(
        &self,
        claim: &jobs::Claim,
        headers: &[(SessionRef, Header)],
        next: Option<String>,
        now: i64,
    ) -> Result<(), String> {
        let jobs::Work::Catalog {
            provider,
            seen,
            listed,
            ..
        } = &claim.work
        else {
            return Err("expected catalog job".into());
        };
        let mut seen = seen.clone();
        let mut listed = listed.clone();
        listed.extend(headers.iter().map(|(id, _)| id.id.clone()));
        if let Some(next) = &next {
            if seen.contains(next) {
                return Err("provider repeated its catalog cursor".into());
            }
            seen.push(next.clone());
        }
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let current:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='running' AND attempts=?2)",params![claim.id.to_string(),integer(claim.attempt)?],|row|row.get(0)).map_err(failure)?;
            if !current { return Err("catalog claim is obsolete".into()); }
            let mut sequence=None;
            for (id,header) in headers {
                if id.provider!=Some(*provider) { return Err("catalog identity changed".into()); }
                // Every native source of a Host conversation, including old
                // providers, is private and cannot reappear as another task.
                let owned:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE provider=?1 AND native_id=?2)",params![provider.key(),id.id],|row|row.get(0)).map_err(failure)?;
                if owned { continue; }
                let parent=header.details.parent.clone();
                let mut header=header.clone();
                if let Some(parent) = &parent {
                    header.details.parent = titles::native_owner(&transaction, parent)?.or_else(|| Some(parent.clone()));
                }
                let key=existing_key(&transaction,id)?;
                if let Some(key)=key {
                    let loaded:i64=transaction.query_row("SELECT loaded FROM threads WHERE key=?1",[key],|row|row.get(0)).map_err(failure)?;
                    if loaded == 1 { continue; }
                    let previous = rows::header(&transaction,key)?.0;
                    header.details.history_read_state = previous.details.history_read_state.clone();
                    if loaded == 0 && previous==header { continue; }
                }
                let sequence = match sequence { Some(sequence)=>sequence, None=>{let current=Self::advance(&transaction)?.sequence; sequence=Some(current); current} };
                header.details.parent=None;
                let key=if let Some(key)=key {
                    transaction.execute("UPDATE threads SET loaded=0,seq=?1,cwd=?2,updated_at=?3,body=?4 WHERE key=?5",params![integer(sequence)?,header.state.workspace.cwd(),header.details.updated_at,codec::header(&header,&Counters::default())?,key]).map_err(failure)?;
                    key
                } else {
                    let key=history::insert_header(&transaction,id,&header,&Counters::default(),sequence)?;
                    transaction.execute("UPDATE threads SET loaded=0 WHERE key=?1",[key]).map_err(failure)?;
                    key
                };
                history::link_parent(&transaction,key,parent.as_ref(),sequence)?;
            }
            let sequence=match sequence {Some(sequence)=>sequence,None=>Self::advance(&transaction)?.sequence};
            transaction.execute("UPDATE jobs SET seq=?1 WHERE id=?2",params![integer(sequence)?,claim.id.to_string()]).map_err(failure)?;
            let complete=next.is_none();
            if complete { prune_catalog(&transaction, *provider, &listed, sequence, now)?; }
            let body: Vec<u8> = transaction.query_row("SELECT body FROM jobs WHERE id=?1", [claim.id.to_string()], |row|row.get(0)).map_err(failure)?;
            let jobs::Work::Catalog {progress,..}=codec::enum_record("catalog",&body)? else {return Err("expected catalog job".into())};
            let work=jobs::Work::Catalog {provider:*provider,cursor:next,seen:if complete {Vec::new()} else {seen},listed:if complete {Default::default()} else {listed},progress:if !complete {progress} else if progress==jobs::CatalogProgress::Refresh {jobs::CatalogProgress::Scanning} else {jobs::CatalogProgress::Ready}};
            let (_,body)=codec::enum_parts(&work)?;
            transaction.execute("UPDATE jobs SET state='pending',run_after=?1,last_error=NULL,body=?2 WHERE id=?3 AND attempts=?4",params![if complete && progress!=jobs::CatalogProgress::Refresh {now.saturating_add(60_000)} else {now},body,claim.id.to_string(),integer(claim.attempt)?]).map_err(failure)?;
            transaction.commit().map_err(failure)
        })
    }
}

fn prune_catalog(
    transaction: &Transaction<'_>,
    provider: ProviderKind,
    listed: &std::collections::BTreeSet<String>,
    sequence: u64,
    now: i64,
) -> Result<(), String> {
    let candidates = {
        let mut statement = transaction.prepare("SELECT key,id FROM threads t WHERE provider=?1 AND loaded=0 AND NOT EXISTS(SELECT 1 FROM sources s WHERE s.provider=t.provider AND s.native_id=t.id)").map_err(failure)?;
        statement
            .query_map([provider.key()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?
    };
    for (key, id) in candidates {
        if listed.contains(&id) {
            continue;
        }
        let session = SessionRef::new(provider, id.clone()).map_err(failure)?;
        let retained: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM threads WHERE parent=?1) OR EXISTS(SELECT 1 FROM jobs WHERE lane=?2)", params![key, format!("import:{session}")], |row| row.get(0)).map_err(failure)?;
        if retained {
            // Keep identities needed by child links or an import already in flight.
            transaction
                .execute(
                    "UPDATE threads SET loaded=-1,seq=?1 WHERE key=?2",
                    params![integer(sequence)?, key],
                )
                .map_err(failure)?;
        } else {
            transaction
                .execute("DELETE FROM threads WHERE key=?1", [key])
                .map_err(failure)?;
        }
        transaction.execute("INSERT INTO removals(seq,kind,provider,id,thread,at) VALUES (?1,'thread',?2,?3,NULL,?4)", params![integer(sequence)?,provider.key(),id,now]).map_err(failure)?;
    }
    Ok(())
}
