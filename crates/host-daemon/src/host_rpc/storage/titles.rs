//! Lists read committed headers, never a live transcript overlay.
use super::*;
use agent_protocol::models::Thread;

pub(super) fn native_owner(
    connection: &Connection,
    native: &SessionRef,
) -> Result<Option<SessionRef>, String> {
    connection.query_row("SELECT threads.provider,threads.id FROM sources JOIN threads ON threads.key=sources.thread WHERE sources.provider=?1 AND native_id=?2", params![native.provider.map(ProviderKind::key),native.id], |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,String>(1)?))).optional().map_err(failure)?.map(|(kind,id)| Ok(SessionRef { provider: kind.as_deref().map(provider).transpose()?, id })).transpose()
}

impl Storage {
    pub(in crate::host_rpc) fn headers(
        &self,
        loaded_only: bool,
    ) -> Result<Vec<(SessionRef, Header, u64)>, String> {
        self.with(|connection| rows::headers(connection, 0, loaded_only))
    }

    pub(in crate::host_rpc) fn summaries(
        &self,
        search: &str,
        loaded_only: bool,
    ) -> Result<Vec<Thread>, String> {
        let search = search.trim().to_lowercase();
        Ok(self
            .headers(loaded_only)?
            .into_iter()
            .filter_map(|(id, header, _)| {
                if !format!(
                    "{} {}",
                    header.state.name,
                    header.details.preview.as_deref().unwrap_or_default()
                )
                .to_lowercase()
                .contains(&search)
                {
                    return None;
                }
                Some(decision::records::summary(&id, &header))
            })
            .collect())
    }

    pub(in crate::host_rpc) fn active_sources(&self) -> Result<Vec<SessionRef>, String> {
        self.with(|connection| {
            let mut statement = connection.prepare("SELECT sources.provider,sources.native_id FROM bindings JOIN sources ON sources.id=bindings.source_id WHERE bindings.state='ready'").map_err(failure)?;
            statement.query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(failure)?.map(|row| { let (kind,id) = row.map_err(failure)?; Ok(SessionRef { provider: Some(provider(&kind)?), id }) }).collect()
        })
    }

    /// A superseded source does not become a second public conversation.
    pub(in crate::host_rpc) fn native_summary(
        &self,
        native: Thread,
    ) -> Result<Option<Thread>, String> {
        let Some(id) = native.id.clone() else {
            return Ok(Some(decision::records::compact(native)));
        };
        self.with(|connection| {
            let source = connection.query_row("SELECT sources.thread,EXISTS(SELECT 1 FROM bindings WHERE source_id=sources.id AND state='ready'),threads.provider,threads.id FROM sources JOIN threads ON threads.key=sources.thread WHERE sources.provider=?1 AND native_id=?2", params![id.provider.map(ProviderKind::key),id.id], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,bool>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?))).optional().map_err(failure)?;
            let Some((key, active, kind, id)) = source else { return Ok(Some(decision::records::compact(native))); };
            if !active { return Ok(None); }
            let id = SessionRef { provider: kind.as_deref().map(provider).transpose()?, id };
            let (header, _) = rows::header(connection, key)?;
            Ok(Some(decision::records::summary(&id, &header)))
        })
    }

    pub(in crate::host_rpc) fn native_owner(
        &self,
        native: &SessionRef,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| native_owner(connection, native))
    }

    pub(in crate::host_rpc) fn native_owners<'a>(
        &self,
        links: impl IntoIterator<Item = &'a SessionRef>,
    ) -> Result<std::collections::HashMap<SessionRef, SessionRef>, String> {
        let links = links.into_iter().collect::<std::collections::BTreeSet<_>>();
        self.with(|connection| {
            let mut owners = std::collections::HashMap::new();
            for link in links {
                if let Some(owner) = native_owner(connection, link)? {
                    owners.insert(link.clone(), owner);
                }
            }
            Ok(owners)
        })
    }

    pub(in crate::host_rpc) fn active_source(
        &self,
        id: &SessionRef,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| {
            let Some(key) = existing_key(connection, id)? else {
                return Ok(None);
            };
            let header = rows::header(connection, key)?.0;
            connection.query_row("SELECT sources.provider,sources.native_id FROM bindings JOIN sources ON sources.id=bindings.source_id WHERE bindings.thread=?1 AND bindings.provider=?2 AND bindings.state='ready'", params![key,header.state.provider.key()], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).optional().map_err(failure)?.map(|(kind,id)| Ok(SessionRef { provider: Some(provider(&kind)?), id })).transpose()
        })
    }
}

impl Storage {
    pub(in crate::host_rpc) fn workspace_membership(
        &self,
        id: &SessionRef,
        cwd: &str,
        membership: agent_protocol::models::ProjectMembership,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let (mut header, counters) = rows::header(&transaction, key)?;
            let mut committed = Committed {
                step: Step::default(),
                cursor: cursor(&transaction)?,
                rows: vec![],
                removed: vec![],
                reset: false,
            };
            if header.state.workspace.cwd() != Some(cwd) || header.details.project == membership {
                return Ok(committed);
            }
            header.details.project = membership;
            committed.cursor = Self::advance(&transaction)?;
            transaction
                .execute(
                    "UPDATE threads SET seq=?1,body=?2 WHERE key=?3",
                    params![
                        integer(committed.cursor.sequence)?,
                        codec::header(&header, &counters)?,
                        key
                    ],
                )
                .map_err(failure)?;
            committed.rows.push(Row {
                key: Key::Thread(id.clone()),
                version: committed.cursor.sequence,
                position: Position::Unordered,
                content: Content::Full(Arc::new(Record::Thread(header))),
            });
            transaction.commit().map_err(failure)?;
            Ok(committed)
        })
    }
}
