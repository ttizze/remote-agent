use crate::{AgentError, Attachment, JsonValue, ListQuery, WorktreeSettings, error};
use agent_core::state::Intent as Core;
use base64::Engine;
use std::collections::HashMap;

#[derive(uniffi::Enum)]
pub enum Answer {
    Decision { index: u32 },
    Permissions { allow: bool },
    Questions { answers: HashMap<String, String> },
    Raw { value: JsonValue },
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
        request_id: JsonValue,
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
            Intent::OpenThread { id } => Core::OpenThread(id),
            Intent::ListThreads { query } => Core::ListThreads(query.into()),
            Intent::ReadThread { id } => Core::ReadThread(id),
            Intent::ReadOlder {
                thread_id,
                turn_id,
                cursor,
            } => Core::ReadOlder {
                thread_id,
                turn_id,
                cursor,
            },
            Intent::ReadItem {
                thread_id,
                turn_id,
                item_id,
            } => Core::ReadItem {
                thread_id,
                turn_id,
                item_id,
            },
            Intent::ForkThread {
                thread_id,
                last_turn_id,
            } => Core::ForkThread {
                thread_id,
                last_turn_id,
            },
            Intent::SetDraftText { key, text } => Core::SetDraftText {
                thread_id: key,
                text,
            },
            Intent::LoadModels => Core::LoadModels,
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
            Intent::Interrupt { thread_id, turn_id } => Core::Interrupt { thread_id, turn_id },
            Intent::Respond { request_id, answer } => Core::Respond {
                request_id: request_id.try_into()?,
                answer: match answer {
                    Answer::Decision { index } => {
                        agent_core::client::Answer::Decision(index as usize)
                    }
                    Answer::Permissions { allow } => agent_core::client::Answer::Permissions(allow),
                    Answer::Questions { answers } => {
                        agent_core::client::Answer::Questions(answers.into_iter().collect())
                    }
                    Answer::Raw { value } => agent_core::client::Answer::Raw(
                        serde_json::value::to_raw_value(&serde_json::Value::try_from(value)?)
                            .map_err(error)?,
                    ),
                },
            },
            Intent::AddAttachment { key, attachment } => Core::AddAttachment {
                draft_key: key,
                attachment: attachment.into(),
            },
            Intent::RemoveAttachment { key, index } => Core::RemoveAttachment {
                draft_key: key,
                index: index as usize,
            },
            Intent::UploadAttachment {
                key,
                attachment,
                directory,
            } => Core::UploadAttachment {
                draft_key: key,
                attachment: attachment.into(),
                directory,
            },
            Intent::DownloadFile {
                source,
                destination,
            } => Core::DownloadFile {
                source: source.into(),
                destination: destination.into(),
            },
            Intent::LoadSessionImages { thread_id } => Core::LoadSessionImages(thread_id),
            Intent::Transcribe {
                key,
                audio,
                send,
                client_user_message_id,
            } => Core::Transcribe {
                draft_key: key,
                audio: base64::engine::general_purpose::STANDARD.encode(audio),
                send,
                client_user_message_id,
            },
            Intent::ListFiles { path } => Core::ListFiles(path),
            Intent::ReadFile {
                path,
                discard_draft,
            } => Core::ReadFile {
                path,
                discard_draft,
            },
            Intent::SetFileDraft { path, text } => Core::SetFileDraft { path, text },
            Intent::SaveFile { path } => Core::SaveFile(path),
            Intent::ReviewWorkspace { cwd } => Core::ReviewWorkspace(cwd),
            Intent::ReadWorktreeSettings => Core::ReadWorktreeSettings,
            Intent::UpdateWorktreeSettings { settings } => {
                Core::UpdateWorktreeSettings(settings.into())
            }
            Intent::ListAccounts => Core::ListAccounts,
            Intent::SelectAccount { id } => Core::SelectAccount(id),
            Intent::StartAccountLogin => Core::StartAccountLogin,
            Intent::ReadAccountLogin { id } => Core::ReadAccountLogin(id),
            Intent::CancelAccountLogin { id } => Core::CancelAccountLogin(id),
        })
    }
}
