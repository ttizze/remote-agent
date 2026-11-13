//! At-least-once work, ordered by the first unfinished job in each lane.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::host_rpc) enum Work {
    ProjectsChanged,
    Catalog {
        provider: ProviderKind,
        cursor: Option<String>,
        seen: Vec<String>,
        listed: std::collections::BTreeSet<String>,
        missing: std::collections::BTreeSet<String>,
        progress: CatalogProgress,
    },
    Continue {
        target: SessionRef,
        source: TurnId,
    },
    Import {
        target: SessionRef,
        source: SessionRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::host_rpc) enum CatalogProgress {
    Scanning,
    Ready,
    Refresh,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::host_rpc) struct Claim {
    pub id: Uuid,
    pub attempt: u64,
    pub work: Work,
}

pub(super) fn enqueue(
    transaction: &Transaction<'_>,
    id: Uuid,
    lane: &str,
    at: i64,
    work: &Work,
) -> Result<(), String> {
    let (kind, body) = codec::enum_parts(work)?;
    transaction.execute("INSERT INTO jobs(id,kind,lane,state,run_after,attempts,body) VALUES (?1,?2,?3,'pending',?4,0,?5)",
        params![id.to_string(),kind,lane,at,body]).map_err(failure)?;
    Ok(())
}

impl Storage {
    pub(in crate::host_rpc) fn continuations(
        &self,
        target: &SessionRef,
    ) -> Result<std::collections::HashSet<TurnId>, String> {
        self.with(|connection| {
            let mut statement = connection
                .prepare("SELECT kind,body FROM jobs WHERE lane=?1")
                .map_err(failure)?;
            let mut rows = statement
                .query([format!("continue:{target}")])
                .map_err(failure)?;
            let mut pending = std::collections::HashSet::new();
            while let Some(row) = rows.next().map_err(failure)? {
                let kind: String = row.get(0).map_err(failure)?;
                let body: Vec<u8> = row.get(1).map_err(failure)?;
                if let Work::Continue { source, .. } = codec::enum_record(&kind, &body)? {
                    pending.insert(source);
                }
            }
            Ok(pending)
        })
    }

    pub(in crate::host_rpc) fn finish_job(&self, id: Uuid, attempt: u64) -> Result<bool, String> {
        self.with(|connection| {
            connection
                .execute(
                    "DELETE FROM jobs WHERE id=?1 AND state='running' AND attempts=?2",
                    params![id.to_string(), integer(attempt)?],
                )
                .map(|count| count == 1)
                .map_err(failure)
        })
    }
    pub(in crate::host_rpc) fn claim(&self, now: i64, limit: u32) -> Result<Vec<Claim>, String> {
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            // A client older than a retained event must replace its scope
            // with a snapshot. Receipts remain durable after this one-day tail.
            let cutoff = now.saturating_sub(24 * 60 * 60 * 1000);
            let expired: Option<i64> = transaction.query_row(
                "SELECT max(seq) FROM journal WHERE at<=?1", [cutoff], |row|row.get(0)
            ).map_err(failure)?;
            if let Some(through) = expired {
                transaction.execute("UPDATE store SET pruned_through=max(pruned_through,?1)",[through]).map_err(failure)?;
                transaction.execute("DELETE FROM journal WHERE at<=?1",[cutoff]).map_err(failure)?;
            }
            let claims={
                let mut statement=transaction.prepare("UPDATE jobs SET state='running',attempts=attempts+1 WHERE position IN (
                    SELECT j.position FROM jobs j WHERE j.state='pending' AND j.run_after<=?1 AND NOT EXISTS (
                        SELECT 1 FROM jobs older WHERE older.lane=j.lane AND older.position<j.position
                    ) ORDER BY j.position LIMIT ?2
                ) RETURNING id,attempts,kind,body").map_err(failure)?;
                let mut rows=statement.query(params![now,limit]).map_err(failure)?;
                let mut claims=Vec::new();
                while let Some(row)=rows.next().map_err(failure)? {
                    let id:String=row.get(0).map_err(failure)?;
                    let attempt:i64=row.get(1).map_err(failure)?;
                    let kind:String=row.get(2).map_err(failure)?;
                    let body:Vec<u8>=row.get(3).map_err(failure)?;
                    claims.push(Claim {id:Uuid::parse_str(&id).map_err(failure)?,attempt:u64::try_from(attempt).map_err(failure)?,work:codec::enum_record(&kind,&body)?});
                }
                claims
            };
            transaction.commit().map_err(failure)?;
            Ok(claims)
        })
    }

    pub(in crate::host_rpc) fn retry(
        &self,
        id: Uuid,
        attempt: u64,
        at: i64,
        error: &str,
    ) -> Result<bool, String> {
        self.with(|connection| {
            let transaction=connection.transaction().map_err(failure)?;
            let before = list::catalogs(&transaction)?;
            let changed=transaction.execute("UPDATE jobs SET state='pending',run_after=?1,last_error=?2 WHERE id=?3 AND state='running' AND attempts=?4",params![at,error,id.to_string(),integer(attempt)?]).map_err(failure)?==1;
            let updated=list::catalogs(&transaction)?.into_iter().filter(|row|!before.contains(row)).collect();
            Self::commit(transaction,None,Step::default(),updated,Vec::new(),false)?;
            Ok(changed)
        })
    }

    pub(in crate::host_rpc) fn delivered(&self, id: Uuid, attempt: u64) -> Result<bool, String> {
        self.with(|connection| {
            let transaction=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            // A relay is settled only after its destination's transaction
            // recorded acceptance or rejection. Re-delivery uses this same ID.
            let stored=transaction.query_row("SELECT kind,body FROM jobs WHERE id=?1 AND state='running' AND attempts=?2",
                params![id.to_string(),integer(attempt)?],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Vec<u8>>(1)?))).optional().map_err(failure)?;
            let Some((kind,body))=stored else { return Ok(false); };
            let target = match codec::enum_record(&kind,&body)? {
                Work::Continue {target,..} | Work::Import {target,..} => target,
                Work::Catalog {..} | Work::ProjectsChanged => return Err("catalog jobs commit their own progress".into()),
            };
            let acknowledged:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM receipts WHERE operation_id=?1 AND provider IS ?2 AND thread_id=?3)",
                params![decision::OperationId::Job(id).receipt_key(),target.provider.map(ProviderKind::key),target.id],|row|row.get(0)).map_err(failure)?;
            if !acknowledged { return Err("destination has not committed the job receipt".into()); }
            let deleted=transaction.execute("DELETE FROM jobs WHERE id=?1 AND state='running' AND attempts=?2",
                params![id.to_string(),integer(attempt)?]).map_err(failure)?;
            transaction.commit().map_err(failure)?;
            Ok(deleted==1)
        })
    }
}
