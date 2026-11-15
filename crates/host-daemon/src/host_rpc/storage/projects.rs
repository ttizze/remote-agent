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

pub(super) fn write(transaction: &Transaction<'_>, project: &Project) -> Result<(), String> {
    let [root] = project.roots.as_slice() else {
        return Err("a project must own exactly one workspace root".into());
    };
    if project.name.trim().is_empty() || root.path.trim().is_empty() {
        return Err("project title and workspace root are required".into());
    }
    let now = timestamp();
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projection_projects WHERE project_id=?1)",
            [&project.id],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let changed = transaction.execute("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES (?1,?2,?3,'[]',?4,?4)
        ON CONFLICT(project_id) DO UPDATE SET title=excluded.title,workspace_root=excluded.workspace_root,updated_at=excluded.updated_at,deleted_at=NULL WHERE projection_projects.title!=excluded.title OR projection_projects.workspace_root!=excluded.workspace_root OR projection_projects.deleted_at IS NOT NULL",
        params![project.id,project.name,root.path,now]).map_err(failure)?;
    if changed > 0 {
        publish(
            transaction,
            &project.id,
            if exists {
                "project.meta-updated"
            } else {
                "project.created"
            },
        )?;
    }
    Ok(())
}

pub(super) fn publish(transaction: &Transaction<'_>, id: &str, kind: &str) -> Result<(), String> {
    let project = shell::projects(transaction)?
        .into_iter()
        .find(|project| project.id == id)
        .ok_or("project does not exist")?;
    let mut payload = serde_json::to_value(&project).map_err(failure)?;
    let fields = payload.as_object_mut().ok_or("project is not an object")?;
    fields.remove("id");
    fields.insert("projectId".into(), id.into());
    event_store::append_metadata(
        transaction,
        "project",
        id,
        kind,
        &project.updated_at,
        &payload,
    )
}

pub(super) fn read(connection: &Connection) -> Result<Vec<Project>, String> {
    shell::projects(connection).map(|projects| {
        projects
            .into_iter()
            .map(|project| Project {
                id: project.id,
                name: project.title,
                roots: vec![agent_protocol::models::ProjectRoot {
                    path: project.workspace_root,
                }],
                icon: None,
            })
            .collect()
    })
}
impl Storage {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn replace_projects(&self, projects: &[Project]) -> Result<(), String> {
        self.with(|connection| {
            let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let now=timestamp();
            for old in read(&tx)?.into_iter().filter(|old|!projects.iter().any(|project|project.id==old.id)) {
                tx.execute("UPDATE projection_projects SET deleted_at=?1,updated_at=?1 WHERE project_id=?2",params![now,old.id]).map_err(failure)?;
                event_store::append_metadata(&tx,"project",&old.id,"project.deleted",&now,&serde_json::json!({"projectId":old.id,"deletedAt":now}))?;
            }
            for project in projects {write(&tx,project)?;}
            tx.commit().map_err(failure)
        })
    }
    pub(crate) fn projects(&self) -> Result<Vec<Project>, String> {
        self.with(|connection| read(connection))
    }
    pub(crate) fn register_project(&self, project: &Project) -> Result<(), String> {
        self.with(|connection| {
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            if read(&tx)?.iter().any(|saved| {
                saved
                    .roots
                    .iter()
                    .any(|root| project.roots.iter().any(|new| root.path == new.path))
            }) {
                return Ok(());
            }
            write(&tx, project)?;
            tx.commit().map_err(failure)
        })
    }
    // Bex explicitly supports projectless conversations. Assign them when a
    // project is registered, using the same pure membership rule as creation.
    // Existing assignments remain owned by their conversation commands.
    pub(crate) fn assign_projectless_threads(
        &self,
        projects: &crate::projects::state::Snapshot,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let threads={
                let mut query=tx.prepare("SELECT payload_json FROM orchestration_v2_projection_threads WHERE deleted_at IS NULL AND json_extract(payload_json,'$.projectId') IS NULL").map_err(failure)?;
                query.query_map([],|row|row.get::<_,String>(0)).map_err(failure)?.map(|row|serde_json::from_str::<agent_protocol::orchestration::Thread>(&row.map_err(failure)?).map_err(failure)).collect::<Result<Vec<_>,_>>()?
            };
            for thread in threads {
                let project_id=projects.project_membership(thread.worktree_path.as_deref()).as_ref().cloned();
                if project_id.is_none() {continue;}
                let state=agent_protocol::orchestration::State::new(thread.clone());
                let now=timestamp();
                commands::append_decision(&tx,&state,agent_protocol::orchestration::Decision {
                    changes:vec![agent_protocol::orchestration::Change::Thread(agent_protocol::orchestration::Thread {project_id,updated_at:now.clone(),..thread})],effects:vec![],
                },None,&now)?;
            }
            tx.commit().map_err(failure)
        })
    }
}
