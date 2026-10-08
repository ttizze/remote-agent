use agent_domain::{PullRequestKey, PullRequestLink, PullRequestWatch, ThreadId};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PullRequestStoreError {
    #[error("pull request store database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("pull request store encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("pull request store lock poisoned")]
    Poisoned,
}

#[derive(Clone)]
pub struct PullRequestStore {
    connection: Arc<Mutex<Connection>>,
}

impl PullRequestStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PullRequestStoreError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS pull_request_links (
                thread_id TEXT NOT NULL,
                host TEXT NOT NULL,
                repository TEXT NOT NULL,
                number INTEGER NOT NULL,
                payload TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(thread_id, host, repository, number)
            );
            CREATE TABLE IF NOT EXISTS pull_request_watches (
                thread_id TEXT NOT NULL,
                host TEXT NOT NULL,
                repository TEXT NOT NULL,
                number INTEGER NOT NULL,
                payload TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(thread_id, host, repository, number)
            );
            CREATE TABLE IF NOT EXISTS pull_request_viewed_files (
                host TEXT NOT NULL,
                repository TEXT NOT NULL,
                number INTEGER NOT NULL,
                path TEXT NOT NULL,
                viewed INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(host, repository, number, path)
            );",
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn sync_thread(
        &self,
        thread: &ThreadId,
        links: &[PullRequestLink],
        updated_at: &str,
    ) -> Result<(), PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM pull_request_links WHERE thread_id = ?1",
            params![thread.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM pull_request_watches WHERE thread_id = ?1",
            params![thread.as_str()],
        )?;
        for link in links {
            let key = link.key();
            transaction.execute(
                "INSERT INTO pull_request_links
                    (thread_id, host, repository, number, payload, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    thread.as_str(),
                    &key.host,
                    &key.repository,
                    key.number,
                    serde_json::to_string(link)?,
                    updated_at,
                ],
            )?;
            if let Some(watch) = link.watch.as_ref() {
                transaction.execute(
                    "INSERT INTO pull_request_watches
                        (thread_id, host, repository, number, payload, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        thread.as_str(),
                        &key.host,
                        &key.repository,
                        key.number,
                        serde_json::to_string(watch)?,
                        updated_at,
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn links(&self, thread: &ThreadId) -> Result<Vec<PullRequestLink>, PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT links.payload, watches.payload FROM pull_request_links links
             LEFT JOIN pull_request_watches watches
               ON watches.thread_id = links.thread_id
              AND watches.host = links.host
              AND watches.repository = links.repository
              AND watches.number = links.number
             WHERE links.thread_id = ?1 ORDER BY links.updated_at DESC, links.number ASC",
        )?;
        let rows = statement.query_map(params![thread.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        rows.map(|row| {
            let (payload, watch_payload) = row?;
            let mut link: PullRequestLink = serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    payload.len(),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            if link.watch.is_none() {
                link.watch = watch_payload
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            watch_payload.as_ref().map_or(0, String::len),
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
            }
            Ok(link)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(PullRequestStoreError::Database)
    }

    pub fn set_watch(
        &self,
        thread: &ThreadId,
        key: &PullRequestKey,
        watch: Option<&PullRequestWatch>,
        updated_at: &str,
    ) -> Result<(), PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        if let Some(watch) = watch {
            connection.execute(
                "INSERT INTO pull_request_watches
                    (thread_id, host, repository, number, payload, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(thread_id, host, repository, number)
                 DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
                params![
                    thread.as_str(),
                    &key.host,
                    &key.repository,
                    key.number,
                    serde_json::to_string(watch)?,
                    updated_at,
                ],
            )?;
        } else {
            connection.execute(
                "DELETE FROM pull_request_watches
                 WHERE thread_id = ?1 AND host = ?2 AND repository = ?3 AND number = ?4",
                params![thread.as_str(), &key.host, &key.repository, key.number],
            )?;
        }
        Ok(())
    }

    pub fn watch(
        &self,
        thread: &ThreadId,
        key: &PullRequestKey,
    ) -> Result<Option<PullRequestWatch>, PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let payload = connection
            .query_row(
                "SELECT payload FROM pull_request_watches
                 WHERE thread_id = ?1 AND host = ?2 AND repository = ?3 AND number = ?4",
                params![thread.as_str(), &key.host, &key.repository, key.number],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()
            .map_err(PullRequestStoreError::Encoding)
    }

    /// Returns links whose watches survive a process restart. The caller owns
    /// provider I/O and decides when a due link is refreshed.
    pub fn watched_links(&self) -> Result<Vec<(ThreadId, PullRequestLink)>, PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let mut statement = connection
            .prepare("SELECT DISTINCT thread_id FROM pull_request_links ORDER BY thread_id")?;
        let threads = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        threads
            .into_iter()
            .filter_map(|thread| {
                let thread_id = ThreadId::new(thread).ok()?;
                Some(thread_id)
            })
            .try_fold(Vec::new(), |mut found, thread| {
                let links = self.links(&thread)?;
                found.extend(
                    links
                        .into_iter()
                        .filter(|link| link.watch.is_some())
                        .map(|link| (thread.clone(), link)),
                );
                Ok(found)
            })
    }

    pub fn viewed_files(
        &self,
        key: &PullRequestKey,
        limit: usize,
    ) -> Result<(Vec<(String, bool)>, bool), PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT path, viewed FROM pull_request_viewed_files
             WHERE host = ?1 AND repository = ?2 AND number = ?3
             ORDER BY path LIMIT ?4",
        )?;
        let rows = statement
            .query_map(
                params![
                    &key.host,
                    &key.repository,
                    key.number,
                    limit.saturating_add(1)
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.len() > limit;
        Ok((rows.into_iter().take(limit).collect(), truncated))
    }

    pub fn set_viewed_files(
        &self,
        key: &PullRequestKey,
        files: &[(&str, bool)],
        updated_at: &str,
    ) -> Result<(), PullRequestStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| PullRequestStoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        for (path, viewed) in files {
            transaction.execute(
                "INSERT INTO pull_request_viewed_files
                    (host, repository, number, path, viewed, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(host, repository, number, path)
                 DO UPDATE SET viewed = excluded.viewed, updated_at = excluded.updated_at",
                params![
                    &key.host,
                    &key.repository,
                    key.number,
                    path,
                    viewed,
                    updated_at
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{PullRequestLinkSource, Timestamp};

    #[test]
    fn links_and_watches_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite");
        let store = PullRequestStore::open(&path).unwrap();
        let thread = ThreadId::new("thread").unwrap();
        let link = PullRequestLink {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 4,
            url: "https://github.com/owner/repo/pull/4".into(),
            source: PullRequestLinkSource::Manual,
            linked_at: Timestamp::parse("2026-10-01T00:00:00Z").unwrap(),
            snapshot: None,
            stack: None,
            watch: None,
        };
        store
            .sync_thread(&thread, std::slice::from_ref(&link), "2026-10-01T00:00:00Z")
            .unwrap();
        assert_eq!(store.links(&thread).unwrap(), vec![link.clone()]);
        let key = link.key();
        let watch = PullRequestWatch {
            started_at: Timestamp::parse("2026-10-01T00:00:00Z").unwrap(),
            head_sha: None,
            failed_checks: vec![],
            passed: false,
            remarks_through: None,
            remark_ids: vec![],
            conflicting: false,
            wakes: 0,
        };
        store
            .set_watch(&thread, &key, Some(&watch), "2026-10-01T00:00:00Z")
            .unwrap();
        drop(store);
        let reopened = PullRequestStore::open(&path).unwrap();
        assert_eq!(reopened.watch(&thread, &key).unwrap(), Some(watch.clone()));
        assert_eq!(reopened.links(&thread).unwrap()[0].watch, Some(watch));
    }

    #[test]
    fn viewed_files_are_bounded_and_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite");
        let store = PullRequestStore::open(&path).unwrap();
        let key = PullRequestKey::new("github.com", "owner/repo", 4);
        store
            .set_viewed_files(
                &key,
                &[("src/lib.rs", true), ("src/main.rs", false)],
                "2026-10-01T00:00:00Z",
            )
            .unwrap();
        assert_eq!(
            store.viewed_files(&key, 1).unwrap(),
            (vec![("src/lib.rs".into(), true)], true)
        );
        drop(store);
        let reopened = PullRequestStore::open(&path).unwrap();
        assert_eq!(reopened.viewed_files(&key, 10).unwrap().0.len(), 2);
    }
}
