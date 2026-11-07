use super::*;
use crate::host_rpc::service::model_selections::Selection;
use agent_protocol::{
    operations::{Submission, SubmissionReceipt},
    session::SubmissionDelivery,
};
use rusqlite::OptionalExtension;

fn storage_error(error: impl std::fmt::Display) -> Failure {
    Failure::new("submission_storage_failed", error)
}

impl Database {
    pub(in crate::host_rpc) fn model_selections(&self) -> Result<Vec<Selection>, String> {
        self.read(|connection| {
            let mut query = connection
                .prepare("SELECT native_session_id, options_json FROM model_selections")
                .map_err(failure)?;
            query
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(failure)?
                .map(|row| {
                    let (session, options) = row.map_err(failure)?;
                    Ok(Selection {
                        session: decode(&session)?,
                        options: decode(&options)?,
                    })
                })
                .collect()
        })
    }

    pub(in crate::host_rpc) fn save_model_selections(
        &self,
        selections: &[Selection],
    ) -> Result<(), String> {
        self.write(|connection| {
            for selection in selections {
                connection.execute("INSERT INTO model_selections (native_session_id, options_json) VALUES (?1, ?2)
                    ON CONFLICT(native_session_id) DO UPDATE SET options_json = excluded.options_json WHERE options_json <> excluded.options_json", params![key(&selection.session), encode(&selection.options)?]).map_err(failure)?;
            }
            Ok(())
        })
    }
    pub(in crate::host_rpc) fn admit_command(
        &self,
        input: &Submission,
    ) -> Result<Option<SubmissionReceipt>, Failure> {
        let id = &input.client_user_message_id;
        if id.is_empty() || id.len() > 256 {
            return Err(Failure::new(
                "invalid_params",
                "clientUserMessageId is required",
            ));
        }
        self.write(|connection| {
            let saved: Option<String> = connection.query_row("SELECT delivery_json FROM command_receipts WHERE thread_id = ?1 AND command_id = ?2", params![key(&input.thread_id), id.as_str()], |row| row.get(0)).optional().map_err(storage_error)?;
            if let Some(saved) = saved {
                match decode(&saved).map_err(storage_error)? {
                    SubmissionDelivery::Accepted { turn_id } => return Ok(Some(SubmissionReceipt { turn_id })),
                    SubmissionDelivery::Sending | SubmissionDelivery::Unknown => return Err(Failure::unknown("submission_outcome_unknown", "The previous input may already have run. It will not be sent again.")),
                    SubmissionDelivery::Rejected => {}
                }
            }
            ensure_thread(connection, &input.thread_id).map_err(storage_error)?;
            connection.execute("INSERT INTO command_receipts (thread_id, command_id, delivery_json) VALUES (?1, ?2, ?3) ON CONFLICT(thread_id, command_id) DO UPDATE SET delivery_json = excluded.delivery_json", params![key(&input.thread_id), id.as_str(), encode(SubmissionDelivery::Sending).map_err(storage_error)?]).map_err(storage_error)?;
            changed(connection, &input.thread_id, "command", id).map_err(storage_error)?;
            Ok(None)
        })
    }

    pub(in crate::host_rpc) fn finish_command(
        &self,
        session: &SessionRef,
        id: &str,
        delivery: &SubmissionDelivery,
    ) -> Result<(), Failure> {
        self.write(|connection| {
            let updated = connection.execute("UPDATE command_receipts SET delivery_json = ?3 WHERE thread_id = ?1 AND command_id = ?2 AND delivery_json <> ?3", params![key(session), id, encode(delivery).map_err(storage_error)?]).map_err(storage_error)?;
            if updated > 0 { changed(connection, session, "command", id).map_err(storage_error)?; }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{operations::Input, session::ProviderKind};

    fn input() -> Submission {
        Submission {
            thread_id: SessionRef::new(ProviderKind::Codex, "task".into()).unwrap(),
            client_user_message_id: "command".into(),
            input: vec![Input::Text {
                text: "original request".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        }
    }

    #[test]
    fn only_a_changed_receipt_advances_the_transcript_revision() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(&root.path().join("state.sqlite"));
        let input = input();
        database
            .import(&input.thread_id, ThreadResponse::default())
            .unwrap();
        let version = || {
            database
                .window(&input.thread_id, None, 10)
                .unwrap()
                .unwrap()
                .response
                .thread
                .transcript_version
                .unwrap()
        };
        let initial = version();
        database.admit_command(&input).unwrap();
        let sending = version();
        assert!(sending > initial);
        let receipt = SubmissionDelivery::Accepted {
            turn_id: Some("turn".into()),
        };
        database
            .finish_command(&input.thread_id, &input.client_user_message_id, &receipt)
            .unwrap();
        let accepted = version();
        assert!(accepted > sending);
        database
            .finish_command(&input.thread_id, &input.client_user_message_id, &receipt)
            .unwrap();
        assert_eq!(version(), accepted);
        database
            .finish_command(&input.thread_id, "missing", &receipt)
            .unwrap();
        assert_eq!(version(), accepted);
    }

    #[test]
    fn command_ids_enforce_the_byte_boundary_before_admission() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(&root.path().join("state.sqlite"));
        let mut input = input();
        for id in [String::new(), "x".repeat(257), "あ".repeat(86)] {
            input.client_user_message_id = id.into();
            assert_eq!(
                database.admit_command(&input).unwrap_err().code,
                "invalid_params"
            );
        }
        input.client_user_message_id = "x".repeat(256).into();
        assert!(database.admit_command(&input).unwrap().is_none());
    }

    #[test]
    fn accepted_receipt_survives_restart_and_replay_never_readmits_the_command() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.sqlite");
        let database = Database::open(&path);
        let mut input = input();
        assert!(database.admit_command(&input).unwrap().is_none());
        database
            .finish_command(
                &input.thread_id,
                &input.client_user_message_id,
                &SubmissionDelivery::Accepted {
                    turn_id: Some("run".into()),
                },
            )
            .unwrap();
        drop(database);
        let database = Database::open(&path);
        input.input = vec![Input::Text {
            text: "different request with the same id".into(),
        }];
        assert_eq!(
            database
                .admit_command(&input)
                .unwrap()
                .unwrap()
                .turn_id
                .unwrap()
                .as_str(),
            "run"
        );
        let count: i64 = database
            .read(|connection| {
                connection
                    .query_row("SELECT count(*) FROM command_receipts", [], |row| {
                        row.get(0)
                    })
                    .map_err(failure)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn unacknowledged_delivery_stays_unknown_after_restart_and_rejected_input_can_retry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.sqlite");
        let input = input();
        let database = Database::open(&path);
        database.admit_command(&input).unwrap();
        drop(database);
        let database = Database::open(&path);
        assert_eq!(
            database.admit_command(&input).unwrap_err().delivery,
            agent_protocol::error::Delivery::Unknown
        );
        database
            .finish_command(
                &input.thread_id,
                &input.client_user_message_id,
                &SubmissionDelivery::Rejected,
            )
            .unwrap();
        assert!(database.admit_command(&input).unwrap().is_none());
    }

    #[test]
    fn receipt_write_failure_never_commits_partial_admission_and_can_recover() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(&root.path().join("state.sqlite"));
        let input = input();
        database.with(|connection| connection.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON command_receipts BEGIN SELECT RAISE(ABORT, 'test failure'); END;").map_err(failure)).unwrap();
        assert_eq!(
            database.admit_command(&input).unwrap_err().delivery,
            agent_protocol::error::Delivery::NotSent
        );
        let rows: i64 = database
            .read(|connection| {
                connection
                    .query_row("SELECT count(*) FROM threads", [], |row| row.get(0))
                    .map_err(failure)
            })
            .unwrap();
        assert_eq!(rows, 0);
        database
            .with(|connection| {
                connection
                    .execute_batch("DROP TRIGGER reject_receipt")
                    .map_err(failure)
            })
            .unwrap();
        assert!(database.admit_command(&input).unwrap().is_none());
    }
}
