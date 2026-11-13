use super::*;
use agent_protocol::models::Project;

fn write(transaction: &Transaction<'_>, project: &Project) -> Result<Row, String> {
    // Icons belong to filesystem decoration reads, not the saved project.
    let (_, body) = codec::pack(project, &["id", "icon"], false)?;
    transaction.execute(
        "INSERT INTO projects(id,body,position) VALUES (?1,?2,(SELECT coalesce(max(position),0)+1 FROM projects))",
        params![project.id,body],
    ).map_err(failure)?;
    let position: i64 = transaction
        .query_row(
            "SELECT position FROM projects WHERE id=?1",
            [&project.id],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let mut project = project.clone();
    project.icon = None;
    Ok(Row {
        key: Key::Project(project.id.clone()),
        version: 0,
        position: Position::Project {
            position: position.try_into().map_err(failure)?,
        },
        content: Content::Full(Arc::new(Record::Project(project))),
    })
}

pub(super) fn read(connection: &Connection) -> Result<Vec<Row>, String> {
    let mut statement = connection
        .prepare("SELECT id,body,position FROM projects ORDER BY position")
        .map_err(failure)?;
    let mut entries = statement.query([]).map_err(failure)?;
    let mut rows = Vec::new();
    while let Some(entry) = entries.next().map_err(failure)? {
        let id: String = entry.get(0).map_err(failure)?;
        let body: Vec<u8> = entry.get(1).map_err(failure)?;
        let project = codec::unpack::<Project>(
            &body,
            [
                ("id", serde_json::json!(id)),
                ("icon", serde_json::Value::Null),
            ],
            None,
        )?;
        rows.push(Row {
            key: Key::Project(id),
            version: 0,
            position: Position::Project {
                position: entry
                    .get::<_, i64>(2)
                    .map_err(failure)?
                    .try_into()
                    .map_err(failure)?,
            },
            content: Content::Full(Arc::new(Record::Project(project))),
        });
    }
    Ok(rows)
}

impl Storage {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn replace_projects(&self, projects: &[Project]) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let removed = read(&transaction)?
                .into_iter()
                .filter(|row| match &row.key {
                    Key::Project(id) => !projects.iter().any(|project| &project.id == id),
                    _ => false,
                })
                .map(|row| row.key)
                .collect();
            transaction
                .execute("DELETE FROM projects", [])
                .map_err(failure)?;
            let rows = projects
                .iter()
                .map(|project| write(&transaction, project))
                .collect::<Result<Vec<_>, _>>()?;
            jobs::enqueue(
                &transaction,
                Uuid::new_v4(),
                "projects",
                super::super::runtime::now(),
                &jobs::Work::ProjectsChanged,
            )?;
            Self::commit(transaction, None, Step::default(), rows, removed, false).map(|_| ())
        })
    }
    pub(crate) fn projects(&self) -> Result<Vec<Project>, String> {
        self.with(|connection| {
            read(connection)?
                .into_iter()
                .map(|row| match row.content {
                    Content::Full(record) | Content::Summary(record) => match record.as_ref() {
                        Record::Project(project) => Ok(project.clone()),
                        _ => unreachable!("project row"),
                    },
                })
                .collect()
        })
    }
    pub(crate) fn register_project(&self, project: &Project) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let existing = read(&transaction)?;
            if existing.iter().any(|row| match &row.content {
                Content::Full(record) | Content::Summary(record) => matches!(record.as_ref(),Record::Project(saved)
                    if saved.roots.iter().any(|root| project.roots.iter().any(|new|root.path==new.path))),
            }) { return Ok(()); }
            let row = write(&transaction,project)?;
            jobs::enqueue(&transaction,Uuid::new_v4(),"projects",super::super::runtime::now(),&jobs::Work::ProjectsChanged)?;
            Self::commit(transaction,None,Step::default(),vec![row],Vec::new(),false).map(|_|())
        })
    }
}
