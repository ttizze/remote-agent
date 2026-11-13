//! Current list snapshots and journal replay share one projection contract.
use super::*;

pub(super) fn project(mut row: Row) -> Row {
    let (Content::Full(value) | Content::Summary(value)) = &row.content;
    if let Record::Thread(header) = value.as_ref() {
        let mut header = header.clone();
        header.details.workspace = None;
        header.details.selection = Default::default();
        row.content = Content::Summary(Arc::new(Record::Thread(header)));
    }
    row
}

fn headers(connection: &Connection) -> Result<Vec<Row>, String> {
    Ok(rows::headers(connection, false)?
        .into_iter()
        .map(|(id, header)| {
            project(Row {
                key: Key::Thread(id),
                version: 0,
                position: Position::Unordered,
                content: Content::Full(Arc::new(Record::Thread(header))),
            })
        })
        .collect())
}

pub(super) fn catalogs(connection: &Connection) -> Result<Vec<Row>, String> {
    let mut statement = connection
        .prepare("SELECT body,last_error FROM jobs WHERE kind='catalog' ORDER BY position")
        .map_err(failure)?;
    let mut entries = statement.query([]).map_err(failure)?;
    let mut result = Vec::new();
    while let Some(entry) = entries.next().map_err(failure)? {
        let body: Vec<u8> = entry.get(0).map_err(failure)?;
        let error: Option<String> = entry.get(1).map_err(failure)?;
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
            version: 0,
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
            let head = cursor(&transaction)?;
            let message = if let Some(events) = journal::replay(&transaction, None, cached, head)? {
                events
            } else {
                let mut rows = headers(&transaction)?;
                rows.extend(projects::read(&transaction)?);
                rows.extend(catalogs(&transaction)?);
                Message::Snapshot {
                    cursor: head,
                    window: None,
                    rows,
                }
            };
            transaction.commit().map_err(failure)?;
            Ok(message)
        })
    }
}
