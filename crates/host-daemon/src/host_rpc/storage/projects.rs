use super::*;
use agent_protocol::models::Project;

pub(super) fn timestamp() -> String {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::from_timestamp(
        time.as_secs().try_into().unwrap_or(i64::MAX),
        time.subsec_nanos(),
    )
    .expect("system time is in chrono's range")
    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

pub(super) fn write(transaction: &Transaction<'_>, project: &Project) -> Result<Row, String> {
    let [root] = project.roots.as_slice() else {
        return Err("a project must own exactly one workspace root".into());
    };
    if project.name.trim().is_empty() || root.path.trim().is_empty() {
        return Err("project title and workspace root are required".into());
    }
    let now = timestamp();
    transaction.execute("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES (?1,?2,?3,'[]',?4,?4)
        ON CONFLICT(project_id) DO UPDATE SET title=excluded.title,workspace_root=excluded.workspace_root,updated_at=excluded.updated_at,deleted_at=NULL",
        params![project.id,project.name,root.path,now]).map_err(failure)?;
    let position: i64 = transaction
        .query_row(
            "SELECT rowid FROM projection_projects WHERE project_id=?1",
            [&project.id],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let mut visible = project.clone();
    visible.icon = None;
    Ok(Row {
        key: Key::Project(project.id.clone()),
        version: 0,
        position: Position::Project {
            position: position.try_into().map_err(failure)?,
        },
        content: Content::Full(Arc::new(Record::Project(visible))),
    })
}

pub(super) fn read(connection: &Connection) -> Result<Vec<Row>, String> {
    let mut statement=connection.prepare("SELECT project_id,title,workspace_root,scripts_json,rowid FROM projection_projects WHERE deleted_at IS NULL ORDER BY rowid").map_err(failure)?;
    let mut entries = statement.query([]).map_err(failure)?;
    let mut rows = Vec::new();
    while let Some(entry) = entries.next().map_err(failure)? {
        let id: String = entry.get(0).map_err(failure)?;
        let name: String = entry.get(1).map_err(failure)?;
        let path: String = entry.get(2).map_err(failure)?;
        if name.trim().is_empty() || path.trim().is_empty() {
            return Err("project title and workspace root are required".into());
        }
        // Scripts are part of this same canonical project, including when the
        // caller only asks for a list of project shells.
        let _: Vec<agent_protocol::orchestration::ProjectScript> =
            serde_json::from_str(&entry.get::<_, String>(3).map_err(failure)?).map_err(failure)?;
        rows.push(Row {
            key: Key::Project(id.clone()),
            version: 0,
            position: Position::Project {
                position: entry
                    .get::<_, i64>(4)
                    .map_err(failure)?
                    .try_into()
                    .map_err(failure)?,
            },
            content: Content::Full(Arc::new(Record::Project(Project {
                id,
                name,
                roots: vec![agent_protocol::models::ProjectRoot { path }],
                icon: None,
            }))),
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
                .execute(
                    "UPDATE projection_projects SET deleted_at=?1 WHERE deleted_at IS NULL",
                    [timestamp()],
                )
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
