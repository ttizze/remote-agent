//! The list and open conversations read the same header row and global cursor.
use super::*;

fn headers(connection: &Connection, after: u64) -> Result<Vec<Row>, String> {
    Ok(rows::headers(connection, after, false)?
        .into_iter()
        .map(|(id, mut header, version)| {
            // List consumers never use setup logs or model selection. Keep the
            // same row identity/version without forwarding conversation details.
            header.details.workspace = None;
            header.details.selection = Default::default();
            Row {
                key: Key::Thread(id),
                version,
                position: Position::Unordered,
                content: Content::Summary(Arc::new(Record::Thread(header))),
            }
        })
        .collect())
}

pub(super) fn catalogs(connection: &Connection, after: u64) -> Result<Vec<Row>, String> {
    let mut statement=connection.prepare("SELECT seq,body,last_error FROM jobs WHERE kind='catalog' AND seq>?1 ORDER BY position").map_err(failure)?;
    let mut entries = statement.query([integer(after)?]).map_err(failure)?;
    let mut result = Vec::new();
    while let Some(entry) = entries.next().map_err(failure)? {
        let sequence: i64 = entry.get(0).map_err(failure)?;
        let body: Vec<u8> = entry.get(1).map_err(failure)?;
        let error: Option<String> = entry.get(2).map_err(failure)?;
        let jobs::Work::Catalog {
            provider, progress, ..
        } = codec::enum_record("catalog", &body)?
        else {
            return Err("invalid catalog job".into());
        };
        let catalog = match error {
            Some(error) => decision::records::Catalog::Failed(error),
            None if progress == jobs::CatalogProgress::Ready => decision::records::Catalog::Ready,
            None => decision::records::Catalog::Loading,
        };
        result.push(Row {
            key: Key::Catalog(provider),
            version: sequence.try_into().map_err(failure)?,
            position: Position::Unordered,
            content: Content::Full(Arc::new(Record::Catalog(catalog))),
        });
    }
    Ok(result)
}

impl Storage {
    pub(in crate::host_rpc) fn changes(&self) -> tokio::sync::watch::Receiver<Option<Cursor>> {
        self.changed.subscribe()
    }

    pub(in crate::host_rpc) fn list(&self, cached: Option<Cursor>) -> Result<Message, String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let current = cursor(&transaction)?;
            let pruned: i64 = transaction.query_row("SELECT removals_pruned_through FROM store",[],|row|row.get(0)).map_err(failure)?;
            let resume = agent_protocol::sync::can_resume(cached,current,pruned.try_into().map_err(failure)?,0);
            let after = if resume { cached.expect("resumable cursor").sequence } else { 0 };
            let mut rows = headers(&transaction,after)?;
            rows.extend(projects::read(&transaction,after)?);
            rows.extend(catalogs(&transaction,after)?);
            let mut removed = Vec::new();
            if resume {
                let present: std::collections::BTreeSet<_> = rows.iter().map(|row| &row.key).collect();
                let mut statement = transaction.prepare("SELECT kind,provider,id,max(seq) FROM removals WHERE thread IS NULL AND seq>?1 GROUP BY kind,provider,id").map_err(failure)?;
                let mut entries = statement.query([integer(after)?]).map_err(failure)?;
                while let Some(entry) = entries.next().map_err(failure)? {
                    let kind: String = entry.get(0).map_err(failure)?;
                    let provider_key: Option<String> = entry.get(1).map_err(failure)?;
                    let id: String = entry.get(2).map_err(failure)?;
                    let version: i64 = entry.get(3).map_err(failure)?;
                    let key = match kind.as_str() {
                        "project" => Key::Project(id),
                        "thread" => Key::Thread(SessionRef::new(provider_key.as_deref().map(provider).transpose()?,id).map_err(failure)?),
                        _ => return Err("invalid list removal".into()),
                    };
                    if !present.contains(&key) {
                        removed.push(Removal { key, version: version.try_into().map_err(failure)? });
                    }
                }
            }
            if resume && rows.len()+removed.len() <= 1000 {
                transaction.commit().map_err(failure)?;
                Ok(Message::Changes { from: cached.expect("resumable cursor"), through: current, rows, removed })
            } else {
                if resume {
                    rows = headers(&transaction,0)?;
                    rows.extend(projects::read(&transaction,0)?);
                    rows.extend(catalogs(&transaction,0)?);
                }
                transaction.commit().map_err(failure)?;
                Ok(Message::Snapshot { cursor: current, window: None, rows })
            }
        })
    }
}
