//! Immutable client state and pure conversation transitions.
use crate::{
    client::{Answer, ServerRequest},
    models::{
        FileContent, FileList, HostStatus, Invitation, Item, ListQuery, Model, RemoteHost, Thread,
        ThreadList, ThreadStatus, Turn, WorkspaceReview, WorktreeSettings,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub path: String,
    pub name: String,
    pub is_image: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDraft {
    pub revision: String,
    pub text: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub directory_request: u64,
    pub directory: Option<Arc<FileList>>,
    pub file_request: u64,
    pub file: Option<Arc<FileContent>>,
    pub review_request: u64,
    pub review_cwd: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub settings_request: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Navigation {
    pub thread_id: Option<String>,
    pub cwd: String,
    pub draft_key: String,
    pub generation: u64,
    pub watch_id: Option<u64>,
    pub watch_thread_id: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub active: BTreeMap<String, bool>,
    pub unread: BTreeSet<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostManagement {
    pub generation: u64,
    pub status: Option<Arc<HostStatus>>,
    pub remotes: Vec<RemoteHost>,
    #[serde(skip)]
    pub invitation: Option<Arc<Invitation>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmission {
    pub draft_key: String,
    pub draft: Arc<Draft>,
    pub turn_id: Option<String>,
    pub after_item_id: Option<String>,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_draft: Option<Arc<Draft>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TerminalPhase {
    Starting,
    Running,
    Exited(i32),
    Failed(String),
    Closed,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalOutput {
    pub sequence: u64,
    pub data: String,
    pub cap_reached: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub cwd: String,
    pub phase: TerminalPhase,
    pub output: VecDeque<Arc<TerminalOutput>>,
    pub sequence: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(skip)]
    pub terminals: Arc<BTreeMap<String, Arc<Terminal>>>,
    pub conversations: Arc<BTreeMap<String, Arc<Thread>>>,
    pub threads: Option<Arc<ThreadList>>,
    pub models: Arc<Vec<Model>>,
    pub requests: Arc<BTreeMap<String, Arc<ServerRequest>>>,
    pub drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    #[serde(default)]
    pub pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    #[serde(default)]
    pub file_drafts: Arc<BTreeMap<String, FileDraft>>,
    #[serde(default)]
    pub workspace: Arc<Workspace>,
    #[serde(default)]
    pub navigation: Arc<Navigation>,
    #[serde(default)]
    pub activity: Arc<Activity>,
    #[serde(default)]
    pub management: Arc<HostManagement>,
    #[serde(default)]
    pub list_query: Arc<ListQuery>,
    #[serde(default)]
    pub list_request: u64,
    pub connected: bool,
    pub error: Option<String>,
}
#[derive(Debug)]
pub enum Intent {
    StartTerminal {
        handle: String,
        cwd: String,
        size: crate::client::TerminalSize,
    },
    WriteTerminal {
        handle: String,
        data: Vec<u8>,
    },
    ResizeTerminal {
        handle: String,
        size: crate::client::TerminalSize,
    },
    CloseTerminal(String),
    AcknowledgeTerminal {
        handle: String,
        sequence: u64,
    },
    Transcribe {
        draft_key: String,
        audio: String,
        send: bool,
        client_user_message_id: String,
    },
    AddAttachment {
        draft_key: String,
        attachment: Attachment,
    },
    RemoveAttachment {
        draft_key: String,
        index: usize,
    },
    UploadAttachment {
        draft_key: String,
        attachment: Attachment,
        directory: String,
    },
    DownloadFile {
        source: std::path::PathBuf,
        destination: std::path::PathBuf,
    },
    LoadSessionImages(String),
    LoadHostManagement,
    CreateInvitation,
    PairRemoteHost {
        invitation: Invitation,
        name: String,
    },
    RemoveRemoteHost(String),
    RevokeDevice(String),
    NewChat(String),
    OpenThread(String),
    ListFiles(String),
    ReadFile {
        path: String,
        discard_draft: bool,
    },
    SetFileDraft {
        path: String,
        text: String,
    },
    SaveFile(String),
    ReviewWorkspace(String),
    ReadWorktreeSettings,
    UpdateWorktreeSettings(WorktreeSettings),
    ListThreads(ListQuery),
    StartThread {
        cwd: Option<String>,
        model: Option<String>,
    },
    ReadThread(String),
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
    LoadModels,
    SetDraft {
        thread_id: String,
        draft: Draft,
    },
    SetDraftText {
        thread_id: String,
        text: String,
    },
    SelectModel {
        thread_id: String,
        model: String,
    },
    SelectEffort {
        thread_id: String,
        effort: String,
    },
    SelectServiceTier {
        thread_id: String,
        service_tier: String,
    },
    Submit {
        /// None submits the current navigation target, creating its thread if needed.
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
    Watch {
        thread_id: String,
        watch_key: u64,
        watch_id: u64,
        path: Option<String>,
    },
    Unwatch {
        watch_key: u64,
        watch_id: u64,
    },
}
#[derive(Debug)]
pub enum Event {
    TerminalStarted(String),
    TerminalClosed(String),
    TerminalFailed {
        handle: String,
        reason: String,
    },
    Intent(Intent),
    Transcribed {
        draft_key: String,
        generation: u64,
        draft: Arc<Draft>,
        text: String,
        send: bool,
        client_user_message_id: String,
    },
    AttachmentUploaded {
        draft_key: String,
        attachment: Attachment,
    },
    HostManagementLoaded {
        generation: u64,
        status: HostStatus,
        remotes: Vec<RemoteHost>,
    },
    InvitationCreated(Invitation),
    RemoteHostPaired(RemoteHost),
    RemoteHostRemoved(String),
    DeviceRevoked(String),
    ThreadOpened {
        generation: u64,
        thread: Thread,
        model: Option<String>,
    },
    FilesLoaded {
        request: u64,
        files: FileList,
    },
    FileLoaded {
        request: u64,
        file: FileContent,
    },
    FileSaved {
        submitted: FileDraft,
        file: FileContent,
    },
    ReviewLoaded {
        request: u64,
        review: WorkspaceReview,
    },
    WorktreeSettingsLoaded {
        request: u64,
        settings: WorktreeSettings,
    },
    ItemLoaded {
        thread_id: String,
        turn_id: String,
        item: Item,
    },
    Submitted {
        thread_id: String,
        client_user_message_id: String,
        draft: Arc<Draft>,
        turn_id: Option<String>,
    },
    SubmissionFailed(String),
    DraftThreadCreated {
        thread: Thread,
        draft_key: String,
        generation: u64,
        client_user_message_id: String,
        draft: Arc<Draft>,
    },
    ThreadRefreshed(Thread),
    OlderLoaded {
        thread_id: String,
        thread: Thread,
        turn_id: Option<String>,
        cursor: Option<String>,
    },
    ThreadsLoaded {
        request: u64,
        threads: ThreadList,
    },
    ModelsLoaded(Vec<Model>),
    ServerRequest(ServerRequest),
    RequestResolved(Value),
    Notification {
        method: String,
        params: Value,
    },
    Connected,
    Disconnected(String),
    Failed(String),
}
#[derive(Debug)]
pub enum Effect {
    Execute(Intent),
    StartSubmission {
        draft_key: String,
        cwd: Option<String>,
        generation: u64,
        client_user_message_id: String,
        draft: Arc<Draft>,
    },
    Submit {
        thread_id: String,
        client_user_message_id: String,
        draft: Arc<Draft>,
    },
}

// Invalidate both displayed content and responses still in flight. File drafts
// remain keyed by absolute path so navigation never discards unsaved edits.
fn clear_workspace_location(workspace: &mut Workspace) {
    workspace.directory = None;
    workspace.directory_request += 1;
    workspace.file = None;
    workspace.file_request += 1;
    workspace.review = None;
    workspace.review_cwd = None;
    workspace.review_request += 1;
}

pub fn reduce(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    match event {
        Event::Intent(intent @ Intent::StartTerminal { .. }) => {
            let Intent::StartTerminal { handle, cwd, .. } = &intent else {
                unreachable!()
            };
            if previous.terminals.contains_key(handle) {
                return reduce(
                    previous,
                    Event::Failed("terminal handle is already in use".into()),
                );
            }
            Arc::make_mut(&mut next.terminals).insert(
                handle.clone(),
                Arc::new(Terminal {
                    cwd: cwd.clone(),
                    phase: TerminalPhase::Starting,
                    output: VecDeque::new(),
                    sequence: 0,
                }),
            );
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::Intent(Intent::AcknowledgeTerminal { handle, sequence }) => {
            if let Some(terminal) = next.terminals.get(&handle)
                && terminal
                    .output
                    .front()
                    .is_some_and(|chunk| chunk.sequence <= sequence)
            {
                let terminal =
                    Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap());
                while terminal
                    .output
                    .front()
                    .is_some_and(|chunk| chunk.sequence <= sequence)
                {
                    terminal.output.pop_front();
                }
            }
        }
        Event::TerminalStarted(handle) => {
            if let Some(terminal) = next.terminals.get(&handle)
                && terminal.phase == TerminalPhase::Starting
            {
                Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap()).phase =
                    TerminalPhase::Running;
            }
        }
        Event::TerminalClosed(handle) => {
            if next.terminals.contains_key(&handle) {
                Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap()).phase =
                    TerminalPhase::Closed;
            }
        }
        Event::TerminalFailed { handle, reason } => {
            if next.terminals.contains_key(&handle) {
                Arc::make_mut(Arc::make_mut(&mut next.terminals).get_mut(&handle).unwrap()).phase =
                    TerminalPhase::Failed(reason);
            }
        }
        Event::Intent(Intent::Submit {
            thread_id,
            client_user_message_id,
        }) => {
            let thread_id = thread_id.or_else(|| previous.navigation.thread_id.clone());
            let draft_key = thread_id
                .clone()
                .unwrap_or_else(|| previous.navigation.draft_key.clone());
            let draft = previous.drafts.get(&draft_key).cloned().unwrap_or_default();
            return submission(
                previous,
                thread_id,
                draft_key,
                draft,
                client_user_message_id,
                None,
            );
        }
        Event::Transcribed {
            draft_key,
            generation,
            mut draft,
            text,
            send,
            client_user_message_id,
        } => {
            if send
                && previous.navigation.generation == generation
                && previous.navigation.draft_key == draft_key
            {
                let clear_draft = draft.clone();
                append_transcript(&mut Arc::make_mut(&mut draft).text, &text);
                let (mut next, effects) = submission(
                    previous,
                    previous.navigation.thread_id.clone(),
                    draft_key,
                    draft,
                    client_user_message_id.clone(),
                    Some(text),
                );
                Arc::make_mut(
                    Arc::make_mut(&mut next.pending_submissions)
                        .get_mut(&client_user_message_id)
                        .unwrap(),
                )
                .clear_draft = Some(clear_draft);
                return (next, effects);
            }
            let draft = Arc::make_mut(
                Arc::make_mut(&mut next.drafts)
                    .entry(draft_key)
                    .or_default(),
            );
            append_transcript(&mut draft.text, &text);
        }
        Event::DraftThreadCreated {
            thread,
            draft_key,
            generation,
            client_user_message_id,
            draft,
        } => {
            let Some(id) = thread.id.clone() else {
                return reduce(previous, Event::Failed("thread ID is missing".into()));
            };
            let same_view = previous.navigation.generation == generation
                && previous.navigation.draft_key == draft_key;
            let (mut next, mut effects) = reduce(
                previous,
                Event::ThreadOpened {
                    generation,
                    thread,
                    model: None,
                },
            );
            let current = previous.drafts.get(&draft_key);
            let original = previous
                .pending_submissions
                .get(&client_user_message_id)
                .and_then(|pending| pending.clear_draft.as_ref())
                .unwrap_or(&draft);
            let target = if same_view {
                current.cloned().unwrap_or_else(|| original.clone())
            } else {
                original.clone()
            };
            let drafts = Arc::make_mut(&mut next.drafts);
            if same_view || current == Some(original) {
                drafts.remove(&draft_key);
            }
            drafts.insert(id.clone(), target);
            if let Some(pending) =
                Arc::make_mut(&mut next.pending_submissions).get_mut(&client_user_message_id)
            {
                Arc::make_mut(pending).draft_key = id.clone();
            }
            effects.push(Effect::Submit {
                thread_id: id,
                client_user_message_id,
                draft,
            });
            if previous.threads.is_some() {
                let (updated, refresh) = reduce(
                    &next,
                    Event::Intent(Intent::ListThreads((*previous.list_query).clone())),
                );
                next = updated;
                effects.extend(refresh);
            }
            return (next, effects);
        }
        Event::SubmissionFailed(id) => {
            if let Some(pending) = Arc::make_mut(&mut next.pending_submissions).remove(&id)
                && let Some(text) = &pending.recovery_text
            {
                let draft = Arc::make_mut(
                    Arc::make_mut(&mut next.drafts)
                        .entry(pending.draft_key.clone())
                        .or_default(),
                );
                append_transcript(&mut draft.text, text);
            }
        }
        Event::Intent(Intent::AddAttachment {
            draft_key,
            attachment,
        })
        | Event::AttachmentUploaded {
            draft_key,
            attachment,
        } => {
            Arc::make_mut(
                Arc::make_mut(&mut next.drafts)
                    .entry(draft_key)
                    .or_default(),
            )
            .attachments
            .push(attachment);
        }
        Event::Intent(Intent::RemoveAttachment { draft_key, index }) => {
            if previous
                .drafts
                .get(&draft_key)
                .is_some_and(|draft| index < draft.attachments.len())
            {
                Arc::make_mut(Arc::make_mut(&mut next.drafts).get_mut(&draft_key).unwrap())
                    .attachments
                    .remove(index);
            }
        }
        Event::Intent(
            intent @ (Intent::LoadHostManagement
            | Intent::PairRemoteHost { .. }
            | Intent::RemoveRemoteHost(_)
            | Intent::RevokeDevice(_)),
        ) => {
            Arc::make_mut(&mut next.management).generation += 1;
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::HostManagementLoaded {
            generation,
            status,
            remotes,
        } => {
            if generation == previous.management.generation {
                let management = Arc::make_mut(&mut next.management);
                management.status = Some(Arc::new(status));
                management.remotes = remotes;
            }
        }
        Event::InvitationCreated(invitation) => {
            Arc::make_mut(&mut next.management).invitation = Some(Arc::new(invitation));
        }
        Event::RemoteHostPaired(host) => {
            let management = Arc::make_mut(&mut next.management);
            if let Some(current) = management
                .remotes
                .iter_mut()
                .find(|current| current.id == host.id)
            {
                *current = host;
            } else {
                management.remotes.push(host);
            }
        }
        Event::RemoteHostRemoved(id) => {
            Arc::make_mut(&mut next.management)
                .remotes
                .retain(|host| host.id != id);
        }
        Event::DeviceRevoked(id) => {
            if let Some(status) = Arc::make_mut(&mut next.management).status.as_mut() {
                Arc::make_mut(status).devices.retain(|device| device != &id);
            }
        }
        Event::Intent(Intent::ListThreads(query)) => {
            next.list_query = Arc::new(query.clone());
            next.list_request += 1;
            return (next, vec![Effect::Execute(Intent::ListThreads(query))]);
        }
        Event::Intent(Intent::NewChat(cwd)) => {
            let key = format!("new:{cwd}");
            if !previous.drafts.contains_key(&key) {
                let draft = Draft::default();
                let (model, effort, tier) = supported_settings(&draft, &previous.models);
                let draft = Draft {
                    model: model.map(str::to_owned),
                    effort: effort.map(str::to_owned),
                    service_tier: tier.map(str::to_owned),
                    ..draft
                };
                Arc::make_mut(&mut next.drafts).insert(key.clone(), Arc::new(draft));
            }
            if previous.navigation.cwd != cwd {
                clear_workspace_location(Arc::make_mut(&mut next.workspace));
            }
            let navigation = Arc::make_mut(&mut next.navigation);
            let watch = navigation.watch_id.take();
            navigation.watch_thread_id = None;
            navigation.generation += 1;
            navigation.thread_id = None;
            navigation.draft_key = key;
            navigation.cwd = cwd;
            return (
                next,
                watch
                    .into_iter()
                    .map(|watch_id| {
                        Effect::Execute(Intent::Unwatch {
                            watch_key: 1,
                            watch_id,
                        })
                    })
                    .collect(),
            );
        }
        Event::Intent(intent @ Intent::OpenThread(_)) => {
            Arc::make_mut(&mut next.navigation).generation += 1;
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::ThreadOpened {
            generation,
            thread,
            model,
        } => {
            let id = thread.id.clone();
            let cwd = thread.cwd.clone().unwrap_or_default();
            let path = thread.path.clone();
            let (mut next, mut effects) = reduce(previous, Event::ThreadRefreshed(thread));
            if generation == previous.navigation.generation
                && let Some(id) = id
            {
                if previous.navigation.cwd != cwd {
                    clear_workspace_location(Arc::make_mut(&mut next.workspace));
                }
                let navigation = Arc::make_mut(&mut next.navigation);
                if let Some(watch_id) = navigation.watch_id.take() {
                    effects.push(Effect::Execute(Intent::Unwatch {
                        watch_key: 1,
                        watch_id,
                    }));
                }
                navigation.watch_thread_id = None;
                navigation.thread_id = Some(id.clone());
                if previous.activity.unread.contains(&id) {
                    Arc::make_mut(&mut next.activity).unread.remove(&id);
                }
                navigation.draft_key = id.clone();
                navigation.cwd = cwd;
                if path.is_some() {
                    navigation.watch_id = Some(generation);
                    navigation.watch_thread_id = Some(id.clone());
                    effects.push(Effect::Execute(Intent::Watch {
                        thread_id: id.clone(),
                        watch_key: 1,
                        watch_id: generation,
                        path,
                    }));
                }
                if let Some(model) = model {
                    let (updated, _) = reduce(
                        &next,
                        Event::Intent(Intent::SelectModel {
                            thread_id: id,
                            model,
                        }),
                    );
                    next = updated;
                }
            }
            return (next, effects);
        }
        Event::Intent(intent @ Intent::Watch { .. }) => {
            if let Intent::Watch {
                thread_id,
                watch_id,
                ..
            } = &intent
            {
                let navigation = Arc::make_mut(&mut next.navigation);
                navigation.watch_id = Some(*watch_id);
                navigation.watch_thread_id = Some(thread_id.clone());
            }
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::Intent(intent @ Intent::Unwatch { .. }) => {
            if let Intent::Unwatch { watch_id, .. } = &intent
                && previous.navigation.watch_id == Some(*watch_id)
            {
                let navigation = Arc::make_mut(&mut next.navigation);
                navigation.watch_id = None;
                navigation.watch_thread_id = None;
            }
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::Intent(
            intent @ (Intent::ReadWorktreeSettings | Intent::UpdateWorktreeSettings(_)),
        ) => {
            Arc::make_mut(&mut next.workspace).settings_request += 1;
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::Intent(intent @ Intent::ListFiles(_)) => {
            Arc::make_mut(&mut next.workspace).directory_request += 1;
            return (next, vec![Effect::Execute(intent)]);
        }
        Event::Intent(Intent::ReadFile {
            path,
            discard_draft,
        }) => {
            if discard_draft {
                Arc::make_mut(&mut next.file_drafts).remove(&path);
            }
            Arc::make_mut(&mut next.workspace).file_request += 1;
            return (
                next,
                vec![Effect::Execute(Intent::ReadFile {
                    path,
                    discard_draft,
                })],
            );
        }
        Event::Intent(Intent::SetFileDraft { path, text }) => {
            let revision = previous
                .file_drafts
                .get(&path)
                .map(|draft| draft.revision.as_str())
                .or_else(|| {
                    previous
                        .workspace
                        .file
                        .as_ref()
                        .filter(|file| file.path == path)
                        .map(|file| file.revision.as_str())
                });
            if let Some(revision) = revision {
                Arc::make_mut(&mut next.file_drafts).insert(
                    path,
                    FileDraft {
                        revision: revision.into(),
                        text,
                    },
                );
            } else {
                next.error = Some("file has not been loaded".into());
            }
        }
        Event::Intent(Intent::ReviewWorkspace(cwd)) => {
            let workspace = Arc::make_mut(&mut next.workspace);
            workspace.review_request += 1;
            if workspace.review_cwd.as_deref() != Some(&cwd) {
                workspace.review = None;
            }
            workspace.review_cwd = Some(cwd.clone());
            return (next, vec![Effect::Execute(Intent::ReviewWorkspace(cwd))]);
        }
        Event::FilesLoaded { request, files } => {
            if request == previous.workspace.directory_request {
                Arc::make_mut(&mut next.workspace).directory = Some(Arc::new(files));
            }
        }
        Event::FileLoaded { request, file } => {
            if request == previous.workspace.file_request {
                Arc::make_mut(&mut next.workspace).file = Some(Arc::new(file));
            }
        }
        Event::FileSaved { submitted, file } => {
            if let Some(current) = previous.file_drafts.get(&file.path) {
                if current == &submitted {
                    Arc::make_mut(&mut next.file_drafts).remove(&file.path);
                } else if current.revision == submitted.revision {
                    Arc::make_mut(&mut next.file_drafts)
                        .get_mut(&file.path)
                        .unwrap()
                        .revision = file.revision.clone();
                }
            }
            if previous
                .workspace
                .file
                .as_ref()
                .is_some_and(|current| current.path == file.path)
            {
                Arc::make_mut(&mut next.workspace).file = Some(Arc::new(file));
            }
        }
        Event::ReviewLoaded { request, review } => {
            if request == previous.workspace.review_request {
                Arc::make_mut(&mut next.workspace).review = Some(Arc::new(review));
            }
        }
        Event::WorktreeSettingsLoaded { request, settings } => {
            if request == previous.workspace.settings_request {
                Arc::make_mut(&mut next.workspace).settings = Some(Arc::new(settings));
            }
        }
        Event::Intent(Intent::SetDraft { thread_id, draft }) => {
            Arc::make_mut(&mut next.drafts).insert(thread_id, Arc::new(draft));
        }
        Event::Intent(Intent::SetDraftText { thread_id, text }) => {
            if previous
                .drafts
                .get(&thread_id)
                .is_none_or(|draft| draft.text != text)
            {
                let draft = Arc::make_mut(&mut next.drafts)
                    .entry(thread_id)
                    .or_default();
                Arc::make_mut(draft).text = text;
            }
        }
        Event::Intent(
            intent @ (Intent::SelectModel { .. }
            | Intent::SelectEffort { .. }
            | Intent::SelectServiceTier { .. }),
        ) => {
            let thread_id = match &intent {
                Intent::SelectModel { thread_id, .. }
                | Intent::SelectEffort { thread_id, .. }
                | Intent::SelectServiceTier { thread_id, .. } => thread_id,
                _ => unreachable!(),
            };
            let mut draft = previous
                .drafts
                .get(thread_id)
                .map(|draft| draft.as_ref().clone())
                .unwrap_or_default();
            let thread_id = match intent {
                Intent::SelectModel { thread_id, model } => {
                    if draft.model.as_deref() != Some(&model) {
                        draft.effort = None;
                        draft.service_tier = None;
                    }
                    draft.model = Some(model);
                    thread_id
                }
                Intent::SelectEffort { thread_id, effort } => {
                    draft.effort = Some(effort);
                    thread_id
                }
                Intent::SelectServiceTier {
                    thread_id,
                    service_tier,
                } => {
                    draft.service_tier = Some(service_tier);
                    thread_id
                }
                _ => unreachable!(),
            };
            if !previous.models.is_empty() {
                let (model, effort, tier) = supported_settings(&draft, &previous.models);
                let settings = (
                    model.map(str::to_owned),
                    effort.map(str::to_owned),
                    tier.map(str::to_owned),
                );
                (draft.model, draft.effort, draft.service_tier) = settings;
            }
            if previous
                .drafts
                .get(&thread_id)
                .is_none_or(|previous| **previous != draft)
            {
                Arc::make_mut(&mut next.drafts).insert(thread_id, Arc::new(draft));
            }
        }
        Event::Intent(intent) => return (next, vec![Effect::Execute(intent)]),
        Event::ItemLoaded {
            thread_id,
            turn_id,
            item,
        } => {
            next = upsert_item(previous, &thread_id, &turn_id, item);
            reconcile_pending(&mut next, &thread_id);
            return (next, Vec::new());
        }
        Event::Submitted {
            thread_id,
            client_user_message_id,
            draft,
            turn_id,
        } => {
            let draft = previous
                .pending_submissions
                .get(&client_user_message_id)
                .and_then(|pending| pending.clear_draft.as_ref())
                .unwrap_or(&draft);
            if let Some(current) = previous.drafts.get(&thread_id) {
                let clear_text = !current.text.is_empty() && current.text == draft.text;
                let sent_attachment = |attachment: &Attachment| {
                    draft
                        .attachments
                        .iter()
                        .any(|sent| sent.path == attachment.path)
                };
                if clear_text || current.attachments.iter().any(sent_attachment) {
                    let retained = Draft {
                        text: if clear_text {
                            String::new()
                        } else {
                            current.text.clone()
                        },
                        attachments: current
                            .attachments
                            .iter()
                            .filter(|attachment| !sent_attachment(attachment))
                            .cloned()
                            .collect(),
                        model: current.model.clone(),
                        effort: current.effort.clone(),
                        service_tier: current.service_tier.clone(),
                    };
                    Arc::make_mut(&mut next.drafts).insert(thread_id.clone(), Arc::new(retained));
                }
            }
            if let Some(pending) =
                Arc::make_mut(&mut next.pending_submissions).get_mut(&client_user_message_id)
            {
                let pending = Arc::make_mut(pending);
                pending.accepted = true;
                if pending.turn_id.is_none() {
                    pending.turn_id = turn_id;
                }
            }
            reconcile_pending(&mut next, &thread_id);
        }
        Event::ThreadRefreshed(incoming) => {
            let Some(id) = incoming.id.clone().filter(|id| !id.trim().is_empty()) else {
                next.error = Some("thread ID is missing".into());
                return (next, Vec::new());
            };
            let thread = previous
                .conversations
                .get(&id)
                .map_or_else(|| incoming.clone(), |current| refresh(current, &incoming));
            Arc::make_mut(&mut next.conversations).insert(id.clone(), Arc::new(thread));
            reconcile_pending(&mut next, &id);
        }
        Event::OlderLoaded {
            thread_id,
            thread,
            turn_id,
            cursor,
        } => {
            let id = &thread_id;
            if let Some(current) = previous.conversations.get(id) {
                match older(current, &thread, turn_id.as_deref(), cursor.as_deref()) {
                    Ok(merged) => {
                        Arc::make_mut(&mut next.conversations).insert(id.clone(), Arc::new(merged));
                        reconcile_pending(&mut next, id);
                    }
                    Err(error) => next.error = Some(error),
                }
            }
        }
        Event::ThreadsLoaded { request, threads } => {
            if request == previous.list_request {
                next.threads = Some(Arc::new(threads));
            }
        }
        Event::ModelsLoaded(models) => {
            for (id, previous_draft) in previous.drafts.iter() {
                let settings = supported_settings(previous_draft, &models);
                if settings
                    != (
                        previous_draft.model.as_deref(),
                        previous_draft.effort.as_deref(),
                        previous_draft.service_tier.as_deref(),
                    )
                {
                    let draft = Arc::make_mut(Arc::make_mut(&mut next.drafts).get_mut(id).unwrap());
                    draft.model = settings.0.map(str::to_owned);
                    draft.effort = settings.1.map(str::to_owned);
                    draft.service_tier = settings.2.map(str::to_owned);
                }
            }
            next.models = Arc::new(models);
        }
        Event::ServerRequest(request) => {
            Arc::make_mut(&mut next.requests).insert(request.id.to_string(), Arc::new(request));
        }
        Event::RequestResolved(id) => {
            Arc::make_mut(&mut next.requests).remove(&id.to_string());
        }
        Event::Notification { method, params } => return notification(previous, &method, params),
        Event::Connected => {
            next.connected = true;
            next.error = None;
        }
        Event::Disconnected(reason) => {
            next.connected = false;
            for terminal in Arc::make_mut(&mut next.terminals).values_mut() {
                if matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Running
                ) {
                    Arc::make_mut(terminal).phase = TerminalPhase::Failed(reason.clone());
                }
            }
            next.error = Some(reason);
        }
        Event::Failed(error) => next.error = Some(error),
    }
    (next, Vec::new())
}

fn supported_settings<'a>(
    draft: &'a Draft,
    models: &'a [Model],
) -> (Option<&'a str>, Option<&'a str>, Option<&'a str>) {
    let model = models
        .iter()
        .find(|model| Some(model.model.as_str()) == draft.model.as_deref())
        .or_else(|| models.iter().find(|model| model.is_default == Some(true)))
        .or_else(|| models.first());
    let Some(model) = model else {
        return (None, None, None);
    };
    let changed = draft.model.as_deref() != Some(&model.model);
    let effort = model
        .supported_reasoning_efforts
        .iter()
        .find(|effort| {
            !changed && Some(effort.reasoning_effort.as_str()) == draft.effort.as_deref()
        })
        .or_else(|| {
            model
                .supported_reasoning_efforts
                .iter()
                .find(|effort| effort.reasoning_effort == model.default_reasoning_effort)
        })
        .or_else(|| model.supported_reasoning_efforts.first());
    let supported_tier = |id: &str| {
        id == "default"
            || model
                .service_tiers
                .as_ref()
                .is_some_and(|tiers| tiers.iter().any(|tier| tier.id == id))
    };
    let tier = draft
        .service_tier
        .as_deref()
        .filter(|tier| !changed && supported_tier(tier))
        .or_else(|| {
            model
                .default_service_tier
                .as_deref()
                .filter(|tier| supported_tier(tier))
        })
        .unwrap_or("default");
    (
        Some(&model.model),
        effort.map(|effort| effort.reasoning_effort.as_str()),
        Some(tier),
    )
}

fn merge_fields(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = previous.clone();
    macro_rules! field { ($($field:ident),* $(,)?) => { $(if incoming.$field.is_some() { merged.$field = incoming.$field.clone(); })* }; }
    field!(
        status,
        items_view,
        items_has_more,
        items_next_cursor,
        deferred_item_ids,
        opening_user_message,
        started_at,
        completed_at,
        duration_ms,
        error
    );
    merged.extra.extend(incoming.extra.clone());
    merged
}

fn append_items(previous: &[Arc<Item>], incoming: &[Arc<Item>]) -> Vec<Arc<Item>> {
    let mut merged = previous.to_vec();
    for item in incoming {
        if let Some(index) = merged.iter().position(|current| current.id == item.id) {
            merged[index] = item.clone();
        } else {
            merged.push(item.clone());
        }
    }
    merged
}

/// Prepend older occurrences, matching overlap from the newest end exactly once.
fn prepend<T>(older: &[Arc<T>], newer: &[Arc<T>], id: impl Fn(&T) -> &str) -> Vec<Arc<T>> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for value in newer {
        *counts.entry(id(value)).or_default() += 1;
    }
    let mut prefix = Vec::with_capacity(older.len());
    for value in older.iter().rev() {
        if let Some(count) = counts.get_mut(id(value)).filter(|count| **count > 0) {
            *count -= 1;
        } else {
            prefix.push(value.clone());
        }
    }
    prefix.reverse();
    prefix.extend_from_slice(newer);
    prefix
}

pub(crate) fn older(
    previous: &Thread,
    incoming: &Thread,
    turn_id: Option<&str>,
    cursor: Option<&str>,
) -> Result<Thread, String> {
    if previous.id != incoming.id {
        return Err("thread ID does not match".into());
    }
    let mut merged = previous.clone();
    if let Some(turn_id) = turn_id {
        let Some(index) = previous
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rposition(|turn| turn.id == turn_id)
        else {
            return Ok(merged);
        };
        let current = &previous.turns.as_ref().unwrap()[index];
        if current
            .items_next_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref())
            != cursor
        {
            return Ok(merged);
        }
        let page = incoming
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rfind(|turn| turn.id == turn_id)
            .ok_or("history page is missing the requested turn")?;
        let next_cursor = page
            .items_next_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref());
        if next_cursor == cursor && (cursor.is_some() || page.items_has_more == Some(true)) {
            return Err("history cursor did not advance".into());
        }
        let mut turn = current.as_ref().clone();
        turn.items = Some(prepend(
            page.items.as_deref().unwrap_or_default(),
            current.items.as_deref().unwrap_or_default(),
            |item| &item.id,
        ));
        turn.items_has_more = Some(page.items_has_more.unwrap_or(false));
        turn.items_next_cursor = Some(next_cursor.map(str::to_owned));
        let mut deferred = page.deferred_item_ids.clone().unwrap_or_default();
        deferred.extend(current.deferred_item_ids.iter().flatten().cloned());
        deferred.retain(|id| {
            let loaded_in = |turn: &Turn| {
                turn.items
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|item| &item.id == id)
                    && !turn
                        .deferred_item_ids
                        .as_deref()
                        .unwrap_or_default()
                        .contains(id)
            };
            !loaded_in(current) && !loaded_in(page)
        });
        deferred.sort();
        deferred.dedup();
        turn.deferred_item_ids = Some(deferred);
        merged.turns.as_mut().unwrap()[index] = Arc::new(turn);
    } else {
        if previous
            .history_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref())
            != cursor
        {
            return Ok(merged);
        }
        let next_cursor = incoming
            .history_cursor
            .as_ref()
            .and_then(|cursor| cursor.as_deref());
        if next_cursor == cursor && cursor.is_some() {
            return Err("history cursor did not advance".into());
        }
        merged.turns = Some(prepend(
            incoming.turns.as_deref().unwrap_or_default(),
            previous.turns.as_deref().unwrap_or_default(),
            |turn| &turn.id,
        ));
        merged.history_cursor = Some(next_cursor.map(str::to_owned));
    }
    Ok(merged)
}

pub(crate) fn refresh(previous: &Thread, incoming: &Thread) -> Thread {
    if incoming.history_cursor.is_none() {
        return incoming.clone();
    }
    let current = previous.turns.as_deref().unwrap_or_default();
    let mut positions: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (index, turn) in current.iter().enumerate() {
        positions.entry(&turn.id).or_default().push_back(index);
    }
    let mut first = None;
    let mut turns = Vec::new();
    for incoming in incoming.turns.as_deref().unwrap_or_default() {
        if let Some(index) = positions
            .get_mut(incoming.id.as_str())
            .and_then(VecDeque::pop_front)
        {
            first = Some(first.map_or(index, |first: usize| first.min(index)));
            turns.push(Arc::new(refresh_turn(&current[index], incoming)));
        } else {
            turns.push(incoming.clone());
        }
    }
    let prefix = &current[..first.unwrap_or(current.len())];
    let mut all = Vec::with_capacity(prefix.len() + turns.len());
    all.extend_from_slice(prefix);
    all.extend(turns);
    let mut merged = incoming.clone();
    merged.turns = Some(all);
    if !prefix.is_empty() || first.is_some() {
        merged.history_cursor = previous.history_cursor.clone();
    }
    merged
}
fn refresh_turn(previous: &Turn, incoming: &Turn) -> Turn {
    let mut merged = merge_fields(previous, incoming);
    let deferred = incoming.deferred_item_ids.as_deref().unwrap_or_default();
    let mut items = previous.items.clone().unwrap_or_default();
    for item in incoming.items.as_deref().unwrap_or_default() {
        if let Some(index) = items.iter().position(|old| old.id == item.id) {
            if !deferred.contains(&item.id)
                || previous
                    .deferred_item_ids
                    .as_deref()
                    .unwrap_or_default()
                    .contains(&item.id)
            {
                items[index] = item.clone();
            }
        } else {
            items.push(item.clone());
        }
    }
    merged.items = Some(items);
    merged.deferred_item_ids = Some(
        deferred
            .iter()
            .filter(|id| {
                !previous
                    .items
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|item| &item.id == *id)
                    || previous
                        .deferred_item_ids
                        .as_deref()
                        .unwrap_or_default()
                        .contains(id)
            })
            .cloned()
            .collect(),
    );
    if previous.items_has_more != Some(true) {
        merged.items_has_more = Some(false);
        merged.items_next_cursor = Some(None);
    }
    merged
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationParams {
    thread_id: String,
    watch_id: Option<u64>,
    turn_id: Option<String>,
    turn: Option<Turn>,
    item: Option<Item>,
    item_id: Option<String>,
    status: Option<ThreadStatus>,
    delta: Option<String>,
    error: Option<Value>,
    #[serde(default)]
    will_retry: bool,
    review_id: Option<String>,
    review: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
fn notification(previous: &Snapshot, method: &str, params: Value) -> (Snapshot, Vec<Effect>) {
    if matches!(method, "process/outputDelta" | "process/exited") {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ProcessEvent {
            process_handle: String,
            delta_base64: Option<String>,
            cap_reached: Option<bool>,
            exit_code: Option<i32>,
        }
        let params: ProcessEvent = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return reduce(
                    previous,
                    Event::Failed(format!("invalid {method} notification: {error}")),
                );
            }
        };
        let Some(current) = previous.terminals.get(&params.process_handle) else {
            return (previous.clone(), Vec::new());
        };
        if matches!(
            current.phase,
            TerminalPhase::Closed | TerminalPhase::Exited(_)
        ) {
            return (previous.clone(), Vec::new());
        }
        let mut next = previous.clone();
        let terminal = Arc::make_mut(
            Arc::make_mut(&mut next.terminals)
                .get_mut(&params.process_handle)
                .unwrap(),
        );
        if method == "process/exited" {
            let Some(code) = params.exit_code else {
                return reduce(
                    previous,
                    Event::Failed("process exit code is missing".into()),
                );
            };
            terminal.phase = TerminalPhase::Exited(code);
        } else {
            let Some(data) = params.delta_base64 else {
                return reduce(previous, Event::Failed("process output is missing".into()));
            };
            terminal.sequence += 1;
            terminal.output.push_back(Arc::new(TerminalOutput {
                sequence: terminal.sequence,
                data,
                cap_reached: params.cap_reached.unwrap_or(false),
            }));
        }
        return (next, Vec::new());
    }
    if method == "serverRequest/resolved" {
        return reduce(
            previous,
            Event::RequestResolved(params["requestId"].clone()),
        );
    }
    let known = matches!(
        method,
        "host/thread/changed"
            | "host/thread/watchFailed"
            | "thread/status/changed"
            | "turn/started"
            | "turn/completed"
            | "item/started"
            | "item/completed"
            | "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "error"
            | "item/autoApprovalReview/started"
            | "item/autoApprovalReview/completed"
    );
    if !known {
        return (previous.clone(), Vec::new());
    }
    let params: NotificationParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            return reduce(
                previous,
                Event::Failed(format!("invalid {method} notification: {error}")),
            );
        }
    };
    if matches!(method, "host/thread/changed" | "host/thread/watchFailed")
        && (previous.navigation.watch_id != params.watch_id
            || previous.navigation.watch_thread_id.as_deref() != Some(&params.thread_id))
    {
        return (previous.clone(), Vec::new());
    }
    if method == "host/thread/changed" {
        return (
            previous.clone(),
            vec![Effect::Execute(Intent::ReadThread(params.thread_id))],
        );
    }
    if method == "host/thread/watchFailed" {
        return reduce(previous, Event::Failed("thread watch failed".into()));
    }
    let mut next = previous.clone();
    let current = previous.conversations.get(&params.thread_id);
    let late_start = method == "turn/started"
        && params.turn.as_ref().is_some_and(|incoming| {
            current
                .and_then(|thread| thread.turns.as_ref())
                .is_some_and(|turns| {
                    turns.iter().any(|turn| {
                        turn.id == incoming.id
                            && turn
                                .status
                                .as_deref()
                                .is_some_and(|status| status != "inProgress")
                    })
                })
        });
    let active = match method {
        "thread/status/changed" => params.status.as_ref().map(|status| status.kind == "active"),
        "turn/started" if !late_start => Some(true),
        "turn/completed" => Some(false),
        _ => None,
    };
    if let Some(active) = active {
        let unread = if active {
            false
        } else if method == "turn/completed"
            && params
                .turn
                .as_ref()
                .is_some_and(|turn| turn.status.as_deref() == Some("completed"))
            && previous.navigation.thread_id.as_deref() != Some(&params.thread_id)
        {
            true
        } else {
            previous.activity.unread.contains(&params.thread_id)
        };
        if previous.activity.active.get(&params.thread_id) != Some(&active)
            || previous.activity.unread.contains(&params.thread_id) != unread
        {
            let activity = Arc::make_mut(&mut next.activity);
            activity.active.insert(params.thread_id.clone(), active);
            if unread {
                activity.unread.insert(params.thread_id.clone());
            } else {
                activity.unread.remove(&params.thread_id);
            }
        }
    }
    let Some(current) = current else {
        return (next, Vec::new());
    };
    if method == "thread/status/changed" {
        if current.status == params.status {
            return (next, Vec::new());
        }
        let thread = Arc::make_mut(
            Arc::make_mut(&mut next.conversations)
                .get_mut(&params.thread_id)
                .unwrap(),
        );
        thread.status = params.status.clone();
        if let Some(list) = next.threads.as_mut().map(Arc::make_mut)
            && let Some(thread) = list
                .data
                .iter_mut()
                .find(|thread| thread.id.as_ref() == Some(&params.thread_id))
        {
            thread.status = params.status;
        }
        return (next, Vec::new());
    }
    let turn_id = params
        .turn
        .as_ref()
        .map(|turn| turn.id.as_str())
        .or(params.turn_id.as_deref());
    let Some(turn_id) = turn_id.filter(|id| !id.is_empty()) else {
        return (next, Vec::new());
    };
    let turn_index = current
        .turns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .rposition(|turn| turn.id == turn_id);
    if matches!(method, "turn/started" | "turn/completed") {
        let Some(incoming) = params.turn else {
            return (next, Vec::new());
        };
        let old = turn_index.map(|index| &current.turns.as_ref().unwrap()[index]);
        if method == "turn/started"
            && old.is_some_and(|old| {
                old.status
                    .as_deref()
                    .is_some_and(|status| status != "inProgress")
            })
        {
            return (next, Vec::new());
        }
        let mut merged = old.map_or_else(|| incoming.clone(), |old| merge_fields(old, &incoming));
        merged.status = Some(if method == "turn/started" {
            "inProgress".into()
        } else {
            incoming
                .status
                .clone()
                .unwrap_or_else(|| "completed".into())
        });
        if let Some(old) = old {
            if incoming.started_at == Some(None) {
                merged.started_at = old.started_at.clone();
            }
            if incoming.completed_at == Some(None) {
                merged.completed_at = old.completed_at.clone();
            }
            if incoming.duration_ms == Some(None) {
                merged.duration_ms = old.duration_ms;
            }
        }
        if let Some(items) = &incoming.items {
            let preserve = items.is_empty()
                || matches!(
                    incoming.items_view.as_deref(),
                    Some("summary" | "notLoaded")
                );
            merged.items = Some(if preserve {
                append_items(
                    old.and_then(|old| old.items.as_deref()).unwrap_or_default(),
                    items,
                )
            } else {
                items.clone()
            });
            if let Some(deferred) = &mut merged.deferred_item_ids {
                deferred.retain(|id| !items.iter().any(|item| &item.id == id));
            }
        }
        if incoming.error.is_none()
            && (merged.status.as_deref() == Some("completed")
                || old.is_some_and(|old| {
                    old.error
                        .as_ref()
                        .is_some_and(|error| error["willRetry"] == true)
                }) && merged.status.as_deref() != Some("inProgress"))
        {
            merged.error = None;
        }
        let thread = Arc::make_mut(
            Arc::make_mut(&mut next.conversations)
                .get_mut(&params.thread_id)
                .unwrap(),
        );
        let turns = thread.turns.get_or_insert_with(Vec::new);
        if let Some(index) = turn_index {
            turns[index] = Arc::new(merged);
        } else {
            turns.push(Arc::new(merged));
        }
        reconcile_pending(&mut next, &params.thread_id);
        return (next, Vec::new());
    }
    let Some(turn_index) = turn_index else {
        return (next, Vec::new());
    };
    let old = &current.turns.as_ref().unwrap()[turn_index];
    let mut item = params.item;
    let review = matches!(
        method,
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed"
    );
    let remove_review = review
        && params
            .review
            .as_ref()
            .is_some_and(|review| review["status"] == "approved");
    if review {
        let Some(id) = params.review_id else {
            return (next, Vec::new());
        };
        let mut extra = params.extra;
        extra.insert("threadId".into(), Value::String(params.thread_id.clone()));
        extra.insert("turnId".into(), Value::String(turn_id.into()));
        if let Some(value) = params.review {
            extra.insert("review".into(), value);
        }
        item = Some(Item {
            id,
            kind: Some("automaticApprovalReview".into()),
            extra,
            ..Default::default()
        });
    }
    let item_id = item
        .as_ref()
        .map(|item| item.id.as_str())
        .or(params.item_id.as_deref());
    let item_index = item_id.and_then(|id| {
        old.items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .position(|item| item.id == id)
    });
    if method == "error" {
        if params.will_retry && old.status.as_deref() != Some("inProgress") {
            return (next, Vec::new());
        }
        let mut error = match params.error.unwrap_or(Value::Null) {
            Value::Object(error) => error,
            message => Map::from_iter([("message".into(), message)]),
        };
        error.insert("willRetry".into(), Value::Bool(params.will_retry));
        let turn = mutable_turn(&mut next, &params.thread_id, turn_index);
        turn.error = Some(Value::Object(error));
    } else if matches!(method, "item/started" | "item/completed") || review {
        if remove_review && item_index.is_none() {
            return (next, Vec::new());
        }
        let Some(item) = item else {
            return (next, Vec::new());
        };
        let turn = mutable_turn(&mut next, &params.thread_id, turn_index);
        if let Some(deferred) = &mut turn.deferred_item_ids {
            deferred.retain(|id| id != &item.id);
        }
        let items = turn.items.get_or_insert_with(Vec::new);
        if remove_review {
            items.remove(item_index.unwrap());
        } else if let Some(index) = item_index {
            items[index] = Arc::new(item);
        } else {
            items.push(Arc::new(item));
        }
    } else {
        let Some(index) = item_index else {
            return (next, Vec::new());
        };
        let Some(delta) = params.delta.filter(|delta| !delta.is_empty()) else {
            return (next, Vec::new());
        };
        let expected = match method {
            "item/agentMessage/delta" => "agentMessage",
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => "reasoning",
            "item/commandExecution/outputDelta" => "commandExecution",
            "item/fileChange/outputDelta" => "fileChange",
            _ => return (next, Vec::new()),
        };
        if old.items.as_ref().unwrap()[index].kind.as_deref() != Some(expected) {
            return (next, Vec::new());
        }
        let item = Arc::make_mut(
            &mut mutable_turn(&mut next, &params.thread_id, turn_index)
                .items
                .as_mut()
                .unwrap()[index],
        );
        match expected {
            "agentMessage" => item.text.get_or_insert_with(String::new).push_str(&delta),
            "commandExecution" => item
                .aggregated_output
                .get_or_insert_with(String::new)
                .push_str(&delta),
            "reasoning" => append_text(item.extra.entry("summary").or_insert(Value::Null), &delta),
            "fileChange" => {
                let changes = item
                    .extra
                    .entry("changes")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if !changes.is_array() {
                    *changes = Value::Array(Vec::new());
                }
                let changes = changes.as_array_mut().unwrap();
                if changes.is_empty() {
                    changes.push(serde_json::json!({"path":"","kind":"update","diff":""}));
                }
                append_text(&mut changes.last_mut().unwrap()["diff"], &delta);
            }
            _ => unreachable!(),
        }
    }
    if matches!(method, "item/started" | "item/completed") {
        reconcile_pending(&mut next, &params.thread_id);
    }
    (next, Vec::new())
}
fn mutable_turn<'a>(snapshot: &'a mut Snapshot, thread_id: &str, index: usize) -> &'a mut Turn {
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(thread_id)
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[index])
}
fn append_text(value: &mut Value, delta: &str) {
    if !value.is_string() {
        let text = match value.take() {
            Value::Array(parts) => parts
                .into_iter()
                .filter_map(|part| match part {
                    Value::String(text) => Some(text),
                    Value::Object(mut object) => object
                        .remove("text")
                        .and_then(|text| text.as_str().map(str::to_owned)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        *value = Value::String(text);
    }
    if let Value::String(text) = value {
        text.push_str(delta);
    }
}

fn upsert_item(previous: &Snapshot, thread_id: &str, turn_id: &str, item: Item) -> Snapshot {
    let Some(thread) = previous.conversations.get(thread_id) else {
        return previous.clone();
    };
    let Some(index) = thread
        .turns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .rposition(|turn| turn.id == turn_id)
    else {
        return previous.clone();
    };
    let mut next = previous.clone();
    let turn = mutable_turn(&mut next, thread_id, index);
    if let Some(deferred) = &mut turn.deferred_item_ids {
        deferred.retain(|id| id != &item.id);
    }
    let items = turn.items.get_or_insert_with(Vec::new);
    if let Some(index) = items.iter().position(|old| old.id == item.id) {
        items[index] = Arc::new(item);
    } else {
        items.push(Arc::new(item));
    }
    next
}

fn submission(
    previous: &Snapshot,
    thread_id: Option<String>,
    draft_key: String,
    draft: Arc<Draft>,
    client_user_message_id: String,
    recovery_text: Option<String>,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    let active = thread_id
        .as_ref()
        .and_then(|id| previous.conversations.get(id))
        .and_then(|thread| thread.turns.as_ref())
        .and_then(|turns| {
            turns
                .iter()
                .rev()
                .find(|turn| turn.status.as_deref() == Some("inProgress"))
        });
    Arc::make_mut(&mut next.pending_submissions).insert(
        client_user_message_id.clone(),
        Arc::new(PendingSubmission {
            draft_key: draft_key.clone(),
            draft: draft.clone(),
            turn_id: active.map(|turn| turn.id.clone()),
            after_item_id: active
                .and_then(|turn| turn.items.as_ref())
                .and_then(|items| items.last())
                .map(|item| item.id.clone()),
            accepted: false,
            recovery_text,
            clear_draft: None,
        }),
    );
    let effect = match thread_id {
        Some(thread_id) => Effect::Submit {
            thread_id,
            client_user_message_id,
            draft,
        },
        None => Effect::StartSubmission {
            draft_key,
            cwd: (!previous.navigation.cwd.trim().is_empty())
                .then(|| previous.navigation.cwd.clone()),
            generation: previous.navigation.generation,
            client_user_message_id,
            draft,
        },
    };
    (next, vec![effect])
}

fn append_transcript(text: &mut String, transcript: &str) {
    text.reserve(transcript.len() + usize::from(!text.is_empty()));
    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push('\n');
    }
    text.push_str(transcript);
}

fn reconcile_pending(snapshot: &mut Snapshot, thread_id: &str) {
    let Some(thread) = snapshot.conversations.get(thread_id) else {
        return;
    };
    let echoed = |id: &String, pending: &Arc<PendingSubmission>| {
        pending.accepted
            && pending.draft_key == thread_id
            && thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .any(|item| {
                    item.kind.as_deref() == Some("userMessage")
                        && item.client_id.as_ref() == Some(id)
                })
    };
    if snapshot
        .pending_submissions
        .iter()
        .any(|(id, pending)| echoed(id, pending))
    {
        Arc::make_mut(&mut snapshot.pending_submissions).retain(|id, pending| !echoed(id, pending));
    }
}
