use crate::{AgentError, Draft, JsonValue, ListQuery, WorktreeSettings, error};
use agent_core::{models, state::PendingSubmission as Pending};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(uniffi::Object)]
pub struct Snapshot(pub(crate) Arc<agent_core::state::Snapshot>);
#[derive(uniffi::Record)]
pub struct Navigation {
    pub thread_id: Option<String>,
    pub cwd: String,
    pub draft_key: String,
    pub generation: u64,
}
#[derive(uniffi::Record)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<String>,
}
#[derive(uniffi::Record)]
pub struct ThreadSummary {
    pub id: String,
    pub name: String,
    pub preview: String,
    pub cwd: String,
    pub project_id: Option<String>,
    pub active: bool,
    pub unread: bool,
}
#[derive(uniffi::Record)]
pub struct ThreadList {
    pub threads: Vec<ThreadSummary>,
    pub projects: Vec<Project>,
    pub more_project_ids: Vec<String>,
    pub has_more_chats: bool,
    pub has_more_projects: bool,
}
#[derive(uniffi::Record)]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub default_reasoning_effort: String,
    pub efforts: Vec<String>,
    pub service_tiers: Vec<String>,
    pub default_service_tier: Option<String>,
    pub is_default: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct Request {
    pub id: JsonValue,
    pub key: String,
    pub method: String,
    pub kind: RequestKind,
    pub title: String,
    pub body: String,
    pub decision_labels: Vec<String>,
    pub decisions: Vec<JsonValue>,
    pub params: HashMap<String, JsonValue>,
}
#[derive(Clone, uniffi::Enum)]
pub enum RequestKind {
    CommandApproval,
    FileApproval,
    Permissions,
    Questions,
    Elicitation,
    Tool,
    Other,
}
pub(crate) type PendingItems = Vec<(String, Arc<Pending>)>;
pub(crate) fn same_pending(a: &PendingItems, b: &PendingItems) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((x, p), (y, q))| x == y && Arc::ptr_eq(p, q))
}
fn pending(id: &str, p: &Pending) -> PendingSubmission {
    PendingSubmission {
        id: id.into(),
        draft_key: p.draft_key.clone(),
        draft: p.draft.as_ref().into(),
        turn_id: p.turn_id.clone(),
        after_item_id: p.after_item_id.clone(),
        accepted: p.accepted,
    }
}
#[derive(uniffi::Record)]
pub struct PendingSubmission {
    pub id: String,
    pub draft_key: String,
    pub draft: Draft,
    pub turn_id: Option<String>,
    pub after_item_id: Option<String>,
    pub accepted: bool,
}
#[derive(uniffi::Record)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
}
#[derive(uniffi::Record)]
pub struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}
#[derive(uniffi::Record)]
pub struct FileContent {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub bom: bool,
    pub line_ending: String,
    pub size: u64,
}
#[derive(uniffi::Record)]
pub struct FileDraft {
    pub revision: String,
    pub text: String,
}
#[derive(uniffi::Record)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}
#[derive(uniffi::Record)]
pub struct Review {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<ChangedFile>,
    pub diff: String,
}
#[derive(uniffi::Record)]
pub struct Account {
    pub id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}
#[derive(uniffi::Record)]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected_id: Option<String>,
    pub error: Option<String>,
}
#[derive(uniffi::Record)]
pub struct AccountLogin {
    pub login_id: String,
    pub user_code: String,
    pub verification_url: String,
}
#[derive(uniffi::Record)]
pub struct AccountLoginStatus {
    pub completed: bool,
    pub account_id: Option<String>,
}

#[uniffi::export]
impl Snapshot {
    pub fn list_unchanged(&self, other: Arc<Snapshot>) -> bool {
        let same_list = match (&self.0.threads, &other.0.threads) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_list && Arc::ptr_eq(&self.0.activity, &other.0.activity)
    }
    pub fn models_unchanged(&self, other: Arc<Snapshot>) -> bool {
        Arc::ptr_eq(&self.0.models, &other.0.models)
    }
    pub fn requests_unchanged(&self, other: Arc<Snapshot>) -> bool {
        Arc::ptr_eq(&self.0.requests, &other.0.requests)
    }
    #[uniffi::constructor]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self(Arc::default()))
    }
    #[uniffi::constructor]
    pub fn restore(bytes: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        let value = if bytes.is_empty() {
            Default::default()
        } else {
            serde_json::from_slice(&bytes).map_err(error)?
        };
        Ok(Arc::new(Self(Arc::new(value))))
    }
    pub fn serialize(&self) -> Result<Vec<u8>, AgentError> {
        serde_json::to_vec(&self.0).map_err(error)
    }
    pub fn connected(&self) -> bool {
        self.0.connected
    }
    pub fn error(&self) -> Option<String> {
        self.0.error.clone()
    }
    pub fn navigation(&self) -> Navigation {
        let n = &self.0.navigation;
        Navigation {
            thread_id: n.thread_id.clone(),
            cwd: n.cwd.clone(),
            draft_key: n.draft_key.clone(),
            generation: n.generation,
        }
    }
    pub fn draft(&self, key: String) -> Draft {
        self.0
            .drafts
            .get(&key)
            .map(|d| d.as_ref().into())
            .unwrap_or_else(|| Draft::from(&agent_core::state::Draft::default()))
    }
    pub fn list_query(&self) -> ListQuery {
        let q = &self.0.list_query;
        ListQuery {
            project_limit: q.project_limit as u32,
            chat_limit: q.chat_limit as u32,
            project_thread_limits: q
                .project_thread_limits
                .iter()
                .map(|(k, v)| (k.clone(), *v as u32))
                .collect(),
            search_term: q.search_term.clone(),
        }
    }
    pub fn thread_list(&self) -> Option<ThreadList> {
        let list = self.0.threads.as_ref()?;
        Some(ThreadList {
            threads: list
                .data
                .iter()
                .map(|t| {
                    let id = t.id.clone().unwrap_or_default();
                    let active =
                        self.0.activity.active.get(&id).copied().unwrap_or_else(|| {
                            t.status.as_ref().is_some_and(|s| s.kind == "active")
                        });
                    let unread = self.0.activity.unread.contains(&id);
                    ThreadSummary {
                        id,
                        name: t.name.clone().unwrap_or_default(),
                        preview: t
                            .extra
                            .get("preview")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        cwd: t.cwd.clone().unwrap_or_default(),
                        project_id: t.project_id.clone().flatten(),
                        active,
                        unread,
                    }
                })
                .collect(),
            projects: list
                .projects
                .iter()
                .map(|p| Project {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    roots: p.roots.iter().map(|r| r.path.clone()).collect(),
                })
                .collect(),
            more_project_ids: list.more_project_ids.clone(),
            has_more_chats: list.has_more_chats,
            has_more_projects: list.has_more_projects,
        })
    }
    pub fn models(&self) -> Vec<Model> {
        self.0
            .models
            .iter()
            .map(|m| Model {
                id: m.id.clone(),
                model: m.model.clone(),
                display_name: m.display_name.clone(),
                default_reasoning_effort: m.default_reasoning_effort.clone(),
                efforts: m
                    .supported_reasoning_efforts
                    .iter()
                    .map(|e| e.reasoning_effort.clone())
                    .collect(),
                service_tiers: m
                    .service_tiers
                    .iter()
                    .flatten()
                    .map(|t| t.id.clone())
                    .collect(),
                default_service_tier: m.default_service_tier.clone(),
                is_default: m.is_default.unwrap_or(false),
            })
            .collect()
    }
    pub fn conversation(&self, id: String) -> Option<Arc<Thread>> {
        self.0.conversations.get(&id).map(|t| {
            Arc::new(Thread(
                t.clone(),
                self.0
                    .pending_submissions
                    .iter()
                    .filter(|(_, p)| p.draft_key == id)
                    .map(|(id, p)| (id.clone(), p.clone()))
                    .collect(),
                self.0.requests.clone(),
            ))
        })
    }
    pub fn requests(&self) -> Vec<Request> {
        self.0
            .requests
            .iter()
            .map(|(key, r)| crate::presentation::request(key, r))
            .collect()
    }
    pub fn pending_submissions(&self) -> Vec<PendingSubmission> {
        self.0
            .pending_submissions
            .iter()
            .map(|(id, p)| PendingSubmission {
                id: id.clone(),
                draft_key: p.draft_key.clone(),
                draft: p.draft.as_ref().into(),
                turn_id: p.turn_id.clone(),
                after_item_id: p.after_item_id.clone(),
                accepted: p.accepted,
            })
            .collect()
    }
    pub fn directory(&self) -> Option<FileList> {
        self.0.workspace.directory.as_ref().map(|d| FileList {
            path: d.path.clone(),
            entries: d
                .entries
                .iter()
                .map(|e| FileEntry {
                    name: e.name.clone(),
                    path: e.path.clone(),
                    directory: e.directory,
                    size: e.size,
                })
                .collect(),
            truncated: d.truncated,
        })
    }
    pub fn file(&self) -> Option<FileContent> {
        self.0.workspace.file.as_ref().map(|f| FileContent {
            path: f.path.clone(),
            revision: f.revision.clone(),
            text: f.text.clone(),
            bom: f.bom,
            line_ending: f.line_ending.clone(),
            size: f.size,
        })
    }
    pub fn file_draft(&self, path: String) -> Option<FileDraft> {
        self.0.file_drafts.get(&path).map(|f| FileDraft {
            revision: f.revision.clone(),
            text: f.text.clone(),
        })
    }
    pub fn review(&self) -> Option<Review> {
        self.0.workspace.review.as_ref().map(|r| Review {
            branch: r.branch.clone(),
            additions: r.additions,
            deletions: r.deletions,
            diff: r.diff.clone(),
            files: r
                .files
                .iter()
                .map(|f| ChangedFile {
                    path: f.path.clone(),
                    status: f.status.clone(),
                    additions: f.additions,
                    deletions: f.deletions,
                })
                .collect(),
        })
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.0
            .workspace
            .settings
            .as_ref()
            .map(|s| WorktreeSettings {
                create_on_new_session: s.create_on_new_session,
                copy_on_create: s.copy_on_create,
                copy_paths: s.copy_paths.clone(),
                worktree_directory: s.worktree_directory.clone(),
            })
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.0.account.accounts.as_ref().map(|a| Accounts {
            accounts: a
                .accounts
                .iter()
                .map(|v| Account {
                    id: v.id.clone(),
                    email: v.email.clone(),
                    plan_type: v.plan_type.clone(),
                })
                .collect(),
            selected_id: a.selected_id.clone(),
            error: a.error.clone(),
        })
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.0.account.login.as_ref().map(|l| AccountLogin {
            login_id: l.login_id.clone(),
            user_code: l.user_code.clone(),
            verification_url: l.verification_url.clone(),
        })
    }
    pub fn account_login_status(&self) -> Option<AccountLoginStatus> {
        self.0
            .account
            .login_status
            .as_ref()
            .map(|s| AccountLoginStatus {
                completed: s.completed,
                account_id: s.account_id.clone(),
            })
    }
}

#[derive(uniffi::Object)]
pub struct Thread(
    pub(crate) Arc<models::Thread>,
    pub(crate) PendingItems,
    pub(crate) Arc<BTreeMap<String, Arc<agent_core::client::ServerRequest>>>,
);
#[uniffi::export]
impl Thread {
    pub fn unchanged(&self, other: Arc<Thread>) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            && same_pending(&self.1, &other.1)
            && Arc::ptr_eq(&self.2, &other.2)
    }
    pub fn id(&self) -> String {
        self.0.id.clone().unwrap_or_default()
    }
    pub fn title(&self) -> String {
        self.0.name.clone().unwrap_or_default()
    }
    pub fn cwd(&self) -> String {
        self.0.cwd.clone().unwrap_or_default()
    }
    pub fn history_cursor(&self) -> Option<String> {
        self.0.history_cursor.clone().flatten()
    }
    pub fn turns(&self) -> Vec<Arc<Turn>> {
        self.0
            .turns
            .iter()
            .flatten()
            .map(|t| {
                Arc::new(Turn(
                    t.clone(),
                    self.1
                        .iter()
                        .filter(|(_, p)| p.turn_id.as_deref() == Some(t.id.as_str()))
                        .cloned()
                        .collect(),
                ))
            })
            .collect()
    }
    pub fn queued_submissions(&self) -> Vec<PendingSubmission> {
        self.1
            .iter()
            .filter(|(_, p)| p.turn_id.is_none())
            .map(|(id, p)| pending(id, p))
            .collect()
    }
}
#[derive(uniffi::Object)]
pub struct Turn(Arc<models::Turn>, PendingItems);
#[uniffi::export]
impl Turn {
    pub fn unchanged(&self, other: Arc<Turn>) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && same_pending(&self.1, &other.1)
    }
    pub fn id(&self) -> String {
        self.0.id.clone()
    }
    pub fn status(&self) -> String {
        self.0.status.clone().unwrap_or_default()
    }
    pub fn error(&self) -> Option<JsonValue> {
        self.0.error.as_ref().map(Into::into)
    }
    pub fn has_older_items(&self) -> bool {
        self.0.items_has_more.unwrap_or(false)
    }
    pub fn items_cursor(&self) -> Option<String> {
        self.0.items_next_cursor.clone().flatten()
    }
    pub fn deferred_item_ids(&self) -> Vec<String> {
        self.0.deferred_item_ids.clone().unwrap_or_default()
    }
    pub fn opening_user_message(&self) -> Option<Arc<Item>> {
        self.0
            .opening_user_message
            .as_ref()
            .map(|i| Arc::new(Item(i.clone())))
    }
    pub fn items(&self) -> Vec<Arc<Item>> {
        self.0
            .items
            .iter()
            .flatten()
            .map(|i| Arc::new(Item(i.clone())))
            .collect()
    }
}
#[derive(uniffi::Object)]
pub struct Item(Arc<models::Item>);
#[derive(Clone, uniffi::Record)]
pub struct ItemPresentation {
    pub id: String,
    pub native_id: Option<String>,
    pub body: String,
    pub image_sources: Vec<String>,
    pub deferred: bool,
    pub kind: String,
    pub title: String,
    pub collapsible: bool,
    pub visible: bool,
}
#[uniffi::export]
impl Item {
    pub fn unchanged(&self, other: Arc<Item>) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn id(&self) -> String {
        self.0.id.clone()
    }
    pub fn kind(&self) -> String {
        self.0.kind.clone().unwrap_or_default()
    }
    pub fn text(&self) -> Option<String> {
        self.0.text.clone()
    }
    pub fn status(&self) -> Option<String> {
        self.0.status.clone()
    }
    pub fn command(&self) -> Option<String> {
        self.0.command.clone()
    }
    pub fn aggregated_output(&self) -> Option<String> {
        self.0.aggregated_output.clone()
    }
    pub fn saved_path(&self) -> Option<String> {
        self.0.saved_path.clone()
    }
    pub fn result(&self) -> Option<JsonValue> {
        self.0.result.as_ref().map(Into::into)
    }
    pub fn client_id(&self) -> Option<String> {
        self.0.client_id.clone()
    }
    pub fn field(&self, name: String) -> Option<JsonValue> {
        self.0.extra.get(&name).map(Into::into)
    }
    /// Complete typed payload for the expanded unknown-item inspector.
    pub fn fields(&self) -> HashMap<String, JsonValue> {
        let mut fields: HashMap<_, _> = self
            .0
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), value.into()))
            .collect();
        for (key, value) in [
            ("id", Some(self.0.id.as_str())),
            ("type", self.0.kind.as_deref()),
            ("text", self.0.text.as_deref()),
            ("status", self.0.status.as_deref()),
            ("command", self.0.command.as_deref()),
            ("aggregatedOutput", self.0.aggregated_output.as_deref()),
            ("savedPath", self.0.saved_path.as_deref()),
            ("clientId", self.0.client_id.as_deref()),
        ] {
            if let Some(value) = value {
                fields.insert(
                    key.into(),
                    JsonValue::String {
                        value: value.into(),
                    },
                );
            }
        }
        if let Some(value) = &self.0.result {
            fields.insert("result".into(), value.into());
        }
        fields
    }
    pub fn expanded_body(&self) -> String {
        conversation_presentation::body::expanded_body(&self.0)
    }
}
