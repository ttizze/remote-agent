use super::*;
use agent_protocol::models::Project;

pub(super) fn write(
    transaction: &Transaction<'_>,
    project: &Project,
    sequence: u64,
) -> Result<(), String> {
    // Icons belong to filesystem decoration reads; One row version is shared by every list subscriber.
    let (_, body) = codec::pack(project, &["id", "icon"], false)?;
    transaction
        .execute(
            "INSERT INTO projects(id,seq,body,position) VALUES (?1,?2,?3,(SELECT coalesce(max(position),0)+1 FROM projects))",
            params![project.id, integer(sequence)?, body],
        )
        .map_err(failure)?;
    Ok(())
}

pub(super) fn read(connection: &Connection, after: u64) -> Result<Vec<Row>, String> {
    let mut statement = connection
        .prepare("SELECT id,seq,body,position FROM projects WHERE seq>?1 ORDER BY position")
        .map_err(failure)?;
    let mut entries = statement.query([integer(after)?]).map_err(failure)?;
    let mut rows = Vec::new();
    while let Some(entry) = entries.next().map_err(failure)? {
        let id: String = entry.get(0).map_err(failure)?;
        let sequence: i64 = entry.get(1).map_err(failure)?;
        let body: Vec<u8> = entry.get(2).map_err(failure)?;
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
            version: sequence.try_into().map_err(failure)?,
            position: Position::Project {
                position: entry
                    .get::<_, i64>(3)
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
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let sequence = Self::advance(&transaction)?;
            let now = super::super::runtime::now();
            for row in read(&transaction, 0)? {
                let Key::Project(id) = row.key else { unreachable!("project row") };
                if !projects.iter().any(|project| project.id == id) {
                    transaction.execute("INSERT INTO removals(seq,kind,id,thread,at) VALUES (?1,'project',?2,NULL,?3)", params![integer(sequence.sequence)?,id,now]).map_err(failure)?;
                }
            }
            transaction.execute("DELETE FROM projects", []).map_err(failure)?;
            for project in projects {
                write(&transaction, project, sequence.sequence)?;
            }
            jobs::enqueue(&transaction, Uuid::new_v4(), "projects", now, &jobs::Work::ProjectsChanged)?;
            transaction.commit().map_err(failure)
        })
    }

    pub(crate) fn projects(&self) -> Result<Vec<Project>, String> {
        self.with(|connection| {
            read(connection, 0)?
                .into_iter()
                .map(|row| match row.content {
                    Content::Full(record) | Content::Summary(record) => match record.as_ref() {
                        Record::Project(project) => Ok(project.clone()),
                        _ => unreachable!(),
                    },
                })
                .collect()
        })
    }

    pub(crate) fn register_project(&self, project: &Project) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let existing = read(&transaction,0)?;
            if existing.iter().any(|row| match &row.content {
                Content::Full(record) | Content::Summary(record) => matches!(record.as_ref(), Record::Project(existing) if existing.roots.iter().any(|root| project.roots.iter().any(|new| root.path == new.path)))
            }) { return Ok(()); }
            let sequence = Self::advance(&transaction)?;
            write(&transaction,project,sequence.sequence)?;
            jobs::enqueue(&transaction,Uuid::new_v4(),"projects",super::super::runtime::now(),&jobs::Work::ProjectsChanged)?;
            transaction.commit().map_err(failure)
        })
    }
}
