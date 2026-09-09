//! Native intent boundary. These are client operations, not Host RPC methods.
use crate::operations::{AgentClient, AgentError, Attachment, TurnOptions, message_input};
use host_protocol::api;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInput<'a> {
    #[serde(borrow)]
    text: Cow<'a, str>,
    #[serde(borrow)]
    attachments: Vec<Attachment<'a>>,
    #[serde(borrow)]
    client_user_message_id: Cow<'a, str>,
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command<'a> {
    Models,
    SessionImages {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
    },
    WatchThread {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        watch_key: u64,
        watch_id: u64,
        #[serde(borrow)]
        path: Cow<'a, str>,
    },
    UnwatchThread {
        watch_key: u64,
        watch_id: u64,
    },
    ListThreads {
        query: serde_json::Value,
    },
    ReadThread {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        defer_item_details: bool,
    },
    ReadOlder {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        cursor: Option<Cow<'a, str>>,
        turn_id: Option<Cow<'a, str>>,
        defer_item_details: bool,
    },
    ReadItem {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        turn_id: Cow<'a, str>,
        #[serde(borrow)]
        item_id: Cow<'a, str>,
    },
    StartThread {
        #[serde(borrow)]
        cwd: Cow<'a, str>,
        model: Option<Cow<'a, str>>,
    },
    StartTurn {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        cwd: Cow<'a, str>,
        #[serde(borrow)]
        input: TurnInput<'a>,
        resume: bool,
        model: Option<Cow<'a, str>>,
        effort: Option<Cow<'a, str>>,
    },
    SteerTurn {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        turn_id: Cow<'a, str>,
        #[serde(borrow)]
        input: TurnInput<'a>,
    },
    QueueTurn {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        input: TurnInput<'a>,
    },
    InterruptTurn {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        turn_id: Cow<'a, str>,
    },
    ListFiles {
        #[serde(borrow)]
        path: Cow<'a, str>,
    },
    ReadFile {
        #[serde(borrow)]
        path: Cow<'a, str>,
    },
    WriteFile {
        #[serde(borrow)]
        path: Cow<'a, str>,
        #[serde(borrow)]
        revision: Cow<'a, str>,
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    ReviewWorkspace {
        #[serde(borrow)]
        cwd: Cow<'a, str>,
    },
    WorktreeSettings,
    UpdateWorktreeSettings {
        settings: api::WorktreeSettings,
    },
    Accounts,
    SelectAccount {
        #[serde(borrow)]
        account_id: Cow<'a, str>,
    },
    StartAccountLogin,
    AccountLoginStatus {
        #[serde(borrow)]
        login_id: Cow<'a, str>,
    },
    CancelAccountLogin {
        #[serde(borrow)]
        login_id: Cow<'a, str>,
    },
    ForkThread {
        #[serde(borrow)]
        thread_id: Cow<'a, str>,
        #[serde(borrow)]
        last_turn_id: Cow<'a, str>,
    },
}

impl AgentClient {
    /// Marshal an intent once at the native boundary; all operation and wire
    /// semantics execute in the same methods used directly by the Mac client.
    pub async fn command_json(&self, command: &str) -> Result<String, AgentError> {
        let command: Command<'_> = serde_json::from_str(command)
            .map_err(|error| AgentError::InvalidResponse(error.to_string()))?;
        match command {
            Command::SessionImages { thread_id } => encode(self.session_images(&thread_id).await),
            Command::WatchThread {
                thread_id,
                watch_key,
                watch_id,
                path,
            } => encode(
                self.watch_thread(&thread_id, watch_key, watch_id, &path)
                    .await,
            ),
            Command::UnwatchThread {
                watch_key,
                watch_id,
            } => encode(self.unwatch_thread(watch_key, watch_id).await),
            Command::Models => encode(self.models().await),
            Command::ListThreads { query } => encode(self.list_threads(&query).await),
            Command::ReadThread {
                thread_id,
                defer_item_details,
            } => encode(self.read_thread(&thread_id, defer_item_details).await),
            Command::ReadOlder {
                thread_id,
                cursor,
                turn_id,
                defer_item_details,
            } => encode(
                self.read_older(
                    &thread_id,
                    cursor.as_deref(),
                    turn_id.as_deref(),
                    defer_item_details,
                )
                .await,
            ),
            Command::ReadItem {
                thread_id,
                turn_id,
                item_id,
            } => encode(self.read_item(&thread_id, &turn_id, &item_id).await),
            Command::StartThread { cwd, model } => {
                encode(self.start_thread(&cwd, model.as_deref()).await)
            }
            Command::StartTurn {
                thread_id,
                cwd,
                input,
                resume,
                model,
                effort,
            } => {
                if resume {
                    self.resume_thread(&thread_id, Some(&cwd)).await?;
                }
                encode(
                    self.start_turn(
                        &thread_id,
                        &message_input(&input.text, input.attachments),
                        &input.client_user_message_id,
                        TurnOptions {
                            model: model.as_deref(),
                            effort: effort.as_deref(),
                            service_tier_for_turn: None,
                        },
                    )
                    .await,
                )
            }
            Command::SteerTurn {
                thread_id,
                turn_id,
                input,
            } => encode(
                self.steer_turn(
                    &thread_id,
                    &turn_id,
                    &message_input(&input.text, input.attachments),
                    &input.client_user_message_id,
                )
                .await,
            ),
            Command::QueueTurn { thread_id, input } => encode(
                self.queue_turn(
                    &thread_id,
                    &message_input(&input.text, input.attachments),
                    &input.client_user_message_id,
                )
                .await,
            ),
            Command::InterruptTurn { thread_id, turn_id } => {
                encode(self.interrupt_turn(&thread_id, &turn_id).await)
            }
            Command::ListFiles { path } => encode(self.list_files(Path::new(path.as_ref())).await),
            Command::ReadFile { path } => encode(self.read_file(Path::new(path.as_ref())).await),
            Command::WriteFile {
                path,
                revision,
                text,
            } => encode(
                self.write_file(Path::new(path.as_ref()), &revision, &text)
                    .await,
            ),
            Command::ReviewWorkspace { cwd } => encode(self.review_workspace(&cwd).await),
            Command::WorktreeSettings => encode(self.worktree_settings().await),
            Command::UpdateWorktreeSettings { settings } => {
                encode(self.update_worktree_settings(&settings).await)
            }
            Command::Accounts => encode(self.accounts().await),
            Command::SelectAccount { account_id } => encode(self.select_account(&account_id).await),
            Command::StartAccountLogin => encode(self.start_account_login().await),
            Command::AccountLoginStatus { login_id } => {
                encode(self.account_login_status(&login_id).await)
            }
            Command::CancelAccountLogin { login_id } => {
                encode(self.cancel_account_login(&login_id).await)
            }
            Command::ForkThread {
                thread_id,
                last_turn_id,
            } => encode(self.fork_thread(&thread_id, &last_turn_id).await),
        }
    }
}

fn encode<T: Serialize>(result: Result<T, AgentError>) -> Result<String, AgentError> {
    serde_json::to_string(&result?).map_err(|error| AgentError::InvalidResponse(error.to_string()))
}
