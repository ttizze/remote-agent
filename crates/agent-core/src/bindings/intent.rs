use super::{AgentError, error};
use crate::state::Intent as Core;
use crate::state::operations as op;
use crate::{
    models::{ListQuery, WorktreeSettings},
    state::Attachment,
};
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;

#[derive(uniffi::Enum)]
pub enum Answer {
    Decision { index: u32 },
    Permissions { allow: bool },
    Questions { answers: HashMap<String, String> },
    Raw { value: Value },
}
#[derive(uniffi::Enum)]
pub enum Intent {
    ShowThreadList,
    NewChat {
        cwd: String,
    },
    OpenThread {
        id: String,
    },
    ListThreads {
        query: ListQuery,
    },
    ReadThread {
        id: String,
    },
    ReadOlder {
        thread_id: String,
        turn_id: Option<String>,
        cursor: Option<String>,
    },
    ReadItem {
        thread_id: String,
        turn_id: String,
        item_id: String,
    },
    ForkThread {
        thread_id: String,
        last_turn_id: String,
    },
    SetDraftText {
        key: String,
        text: String,
    },
    LoadModels,
    SelectModel {
        key: String,
        model: String,
    },
    SelectEffort {
        key: String,
        effort: String,
    },
    SelectServiceTier {
        key: String,
        service_tier: String,
    },
    Submit {
        thread_id: Option<String>,
        client_user_message_id: String,
    },
    Interrupt {
        thread_id: String,
        turn_id: String,
    },
    Respond {
        request_id: Value,
        answer: Answer,
    },
    AddAttachment {
        key: String,
        attachment: Attachment,
    },
    RemoveAttachment {
        key: String,
        index: u32,
    },
    UploadAttachment {
        key: String,
        attachment: Attachment,
        directory: String,
    },
    DownloadFile {
        source: String,
        destination: String,
    },
    LoadSessionImages {
        thread_id: String,
    },
    Transcribe {
        key: String,
        audio: Vec<u8>,
        send: bool,
        client_user_message_id: String,
    },
    ListFiles {
        path: String,
    },
    ReadFile {
        path: String,
        discard_draft: bool,
    },
    SetFileDraft {
        path: String,
        text: String,
    },
    SaveFile {
        path: String,
    },
    ReviewWorkspace {
        cwd: String,
    },
    ReadWorktreeSettings,
    UpdateWorktreeSettings {
        settings: WorktreeSettings,
    },
    ListAccounts,
    SelectAccount {
        id: String,
    },
    StartAccountLogin,
    ReadAccountLogin {
        id: String,
    },
    CancelAccountLogin {
        id: String,
    },
}
impl TryFrom<Intent> for Core {
    type Error = AgentError;
    fn try_from(i: Intent) -> Result<Self, Self::Error> {
        Ok(match i {
            Intent::ShowThreadList => Core::ShowThreadList,
            Intent::NewChat { cwd } => Core::NewChat(cwd),
            Intent::OpenThread { id } => Core::ReadThread(op::ReadThread::open(id)),
            Intent::ListThreads { query } => Core::ListThreads(op::ListThreads::new(query)),
            Intent::ReadThread { id } => Core::ReadThread(op::ReadThread::new(id)),
            Intent::ReadOlder {
                thread_id,
                turn_id,
                cursor,
            } => Core::ReadOlder(op::ReadOlder {
                thread_id,
                turn_id,
                cursor,
                defer_item_details: true,
            }),
            Intent::ReadItem {
                thread_id,
                turn_id,
                item_id,
            } => Core::ReadItem(op::ReadItem {
                thread_id,
                turn_id,
                item_id,
            }),
            Intent::ForkThread {
                thread_id,
                last_turn_id,
            } => Core::ForkThread(op::ForkThread {
                thread_id,
                last_turn_id,
                exclude_turns: false,
            }),
            Intent::SetDraftText { key, text } => Core::SetDraftText {
                thread_id: key,
                text,
            },
            Intent::LoadModels => Core::LoadModels(op::LoadModels),
            Intent::SelectModel { key, model } => Core::SelectModel {
                thread_id: key,
                model,
            },
            Intent::SelectEffort { key, effort } => Core::SelectEffort {
                thread_id: key,
                effort,
            },
            Intent::SelectServiceTier { key, service_tier } => Core::SelectServiceTier {
                thread_id: key,
                service_tier,
            },
            Intent::Submit {
                thread_id,
                client_user_message_id,
            } => Core::Submit {
                thread_id,
                client_user_message_id,
            },
            Intent::Interrupt { thread_id, turn_id } => {
                Core::Interrupt(op::Interrupt { thread_id, turn_id })
            }
            Intent::Respond { request_id, answer } => Core::Respond(op::Respond {
                request_id,
                answer: match answer {
                    Answer::Decision { index } => crate::client::Answer::Decision(index as usize),
                    Answer::Permissions { allow } => crate::client::Answer::Permissions(allow),
                    Answer::Questions { answers } => {
                        crate::client::Answer::Questions(answers.into_iter().collect())
                    }
                    Answer::Raw { value } => crate::client::Answer::Raw(
                        serde_json::value::to_raw_value(&value).map_err(error)?,
                    ),
                },
            }),
            Intent::AddAttachment { key, attachment } => Core::AddAttachment {
                draft_key: key,
                attachment,
            },
            Intent::RemoveAttachment { key, index } => Core::RemoveAttachment {
                draft_key: key,
                index: index as usize,
            },
            Intent::UploadAttachment {
                key,
                attachment,
                directory,
            } => Core::UploadAttachment(op::UploadAttachment {
                draft_key: key,
                attachment,
                directory,
            }),
            Intent::DownloadFile {
                source,
                destination,
            } => Core::DownloadFile(op::DownloadFile {
                source: source.into(),
                destination: destination.into(),
            }),
            Intent::LoadSessionImages { thread_id } => {
                Core::LoadSessionImages(op::LoadSessionImages { thread_id })
            }
            Intent::Transcribe {
                key,
                audio,
                send,
                client_user_message_id,
            } => Core::Transcribe(op::Dictate {
                draft_key: key,
                request: crate::client::Transcribe {
                    audio: base64::engine::general_purpose::STANDARD.encode(audio),
                },
                send,
                client_user_message_id,
            }),
            Intent::ListFiles { path } => Core::ListFiles(op::ListFiles { path }),
            Intent::ReadFile {
                path,
                discard_draft,
            } => Core::ReadFile(op::ReadFile {
                path,
                discard_draft,
            }),
            Intent::SetFileDraft { path, text } => Core::SetFileDraft { path, text },
            Intent::SaveFile { path } => Core::SaveFile(op::SaveFile { path }),
            Intent::ReviewWorkspace { cwd } => Core::ReviewWorkspace(op::ReviewWorkspace { cwd }),
            Intent::ReadWorktreeSettings => Core::ReadWorktreeSettings(op::ReadWorktreeSettings {}),
            Intent::UpdateWorktreeSettings { settings } => {
                Core::UpdateWorktreeSettings(op::UpdateWorktreeSettings { settings })
            }
            Intent::ListAccounts => Core::ListAccounts(op::ListAccounts {}),
            Intent::SelectAccount { id } => Core::SelectAccount(op::SelectAccount { id }),
            Intent::StartAccountLogin => Core::StartAccountLogin(op::StartAccountLogin {}),
            Intent::ReadAccountLogin { id } => Core::ReadAccountLogin(op::ReadAccountLogin { id }),
            Intent::CancelAccountLogin { id } => {
                Core::CancelAccountLogin(op::CancelAccountLogin { id })
            }
        })
    }
}
