//! Shared request records and their typed RPC contracts.
use crate::error::PeerError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The method, parameters and result are one contract.
pub trait RpcMethod: Serialize {
    type Contract: crate::protocol::contracts::Contract<Output = Self::Output>;
    type Output: DeserializeOwned + Serialize;
    fn method(&self) -> &'static str {
        <Self::Contract as crate::protocol::contracts::Contract>::METHOD
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError>;
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        self.params()
            .map(<Self::Contract as crate::protocol::contracts::Contract>::call)
    }
}
#[macro_export]
macro_rules! rpc_contract {
    ($variant:ident) => {
        type Contract = $crate::protocol::contracts::$variant;
        type Output = <$crate::protocol::contracts::$variant as $crate::protocol::contracts::Contract>::Output;
    };
}
pub use crate::rpc_contract;
#[macro_export]
macro_rules! rpc_method {
    ($name:ty, $variant:ident, |$this:ident| $params:expr) => {
        impl $crate::operations::RpcMethod for $name {
            $crate::operations::rpc_contract!($variant);
            fn params(&$this) -> Result<<Self::Contract as $crate::protocol::contracts::Contract>::Params, $crate::error::PeerError> {
                Ok($params)
            }
        }
    };
}
pub use crate::rpc_method;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pair {
    pub invitation: uuid::Uuid,
}

#[derive(Debug, Serialize, Clone)]
pub struct ReadHostStatus {}
rpc_method!(ReadHostStatus, HostStatus, |self| crate::models::Empty {});

#[derive(Debug, Serialize, Clone)]
pub struct ListRemoteHosts {}
rpc_method!(ListRemoteHosts, ListRemotes, |self| crate::models::Empty {});
#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct RegisterRemoteHost {
    pub ticket: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "T: AsRef<[u8]>", deserialize = "T: From<Vec<u8>>"))]
pub struct Transcribe<T = Vec<u8>> {
    pub preparation: Option<String>,
    #[serde(with = "crate::protocol::bytes")]
    pub audio: T,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Transcription {
    pub text: String,
}
impl<T: AsRef<[u8]>> RpcMethod for Transcribe<T> {
    crate::operations::rpc_contract!(Transcribe);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(Transcribe {
            preparation: self.preparation.clone(),
            audio: self.audio.as_ref().to_vec(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictationPreparation {
    pub id: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub revision: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: crate::provider::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub label: String,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}

impl UsageWindow {
    pub fn from_used(label: String, used: f64, resets_at: Option<i64>) -> Option<Self> {
        used.is_finite().then(|| Self {
            label,
            remaining_percent: (100.0 - used.clamp(0.0, 100.0)).floor() as u32,
            resets_at,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected: std::collections::HashMap<crate::provider::ProviderKind, String>,
    pub error: Option<String>,
}

impl Accounts {
    pub fn is_selected(&self, account: &Account) -> bool {
        self.selected.get(&account.provider) == Some(&account.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {
    pub provider: crate::provider::ProviderKind,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SubmitAccountLogin {
    pub provider: crate::provider::ProviderKind,
    pub id: String,
    pub code: String,
}
impl std::fmt::Debug for SubmitAccountLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitAccountLogin")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection {
    pub provider: crate::provider::ProviderKind,
    pub selected_id: String,
    pub persistence_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLogin {
    pub provider: crate::provider::ProviderKind,
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStatus {
    pub completed: bool,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}
impl TerminalSize {
    /// 1 to 1000 columns and 1 to 500 rows.
    pub fn validate(self) -> Result<(), String> {
        if !(1..=1000).contains(&self.cols) || !(1..=500).contains(&self.rows) {
            return Err("terminal size must be 1 to 1000 columns and 1 to 500 rows".into());
        }
        Ok(())
    }
}

// Shared Host/Client request records; Store behavior lives in state::operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProject {
    pub cwd: String,
}

/// Omitted fields stay unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProject {
    pub project_id: String,
    #[serde(default)]
    pub scripts: Option<Vec<crate::models::ProjectScript>>,
    /// `Some(None)` clears the saved icon path.
    #[serde(default)]
    pub favicon_path: Option<Option<String>>,
}

/// Opens a thread's terminal or attaches to it; the caller then receives its output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTerminal {
    pub thread: agent_domain::ThreadId,
    /// Chosen by the client, e.g. `term-1`; at most 128 characters.
    pub terminal_id: String,
    /// Required when the terminal does not exist yet. A different directory or
    /// environment restarts an exited terminal only.
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub size: TerminalSize,
    /// Variables added to the shell's environment.
    pub env: std::collections::BTreeMap<String, String>,
    /// Starts an exited terminal again instead of showing its last screen.
    pub restart_if_not_running: bool,
}
impl StartTerminal {
    pub fn handle(&self) -> String {
        thread_terminal_handle_for(self.thread.as_str(), &self.terminal_id)
    }
    pub fn validate(&self) -> Result<(), String> {
        validate_terminal_id(&self.terminal_id)?;
        validate_terminal_env(&self.env)?;
        if self.cwd.as_deref().is_some_and(|cwd| cwd.trim().is_empty()) {
            return Err("terminal directory is empty".into());
        }
        self.size.validate()
    }
}

/// Clears a thread's terminal: its history and every attached screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearTerminal {
    pub thread: agent_domain::ThreadId,
    pub terminal_id: String,
}
impl ClearTerminal {
    pub fn handle(&self) -> String {
        thread_terminal_handle_for(self.thread.as_str(), &self.terminal_id)
    }
}

/// Starts a thread's terminal again in `cwd` with an empty history, stopping
/// its shell when one runs; opens the terminal when it does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartTerminal {
    pub thread: agent_domain::ThreadId,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub size: TerminalSize,
    /// Variables added to the shell's environment.
    pub env: std::collections::BTreeMap<String, String>,
}
impl RestartTerminal {
    pub fn handle(&self) -> String {
        thread_terminal_handle_for(self.thread.as_str(), &self.terminal_id)
    }
    pub fn validate(&self) -> Result<(), String> {
        validate_terminal_id(&self.terminal_id)?;
        validate_terminal_env(&self.env)?;
        if self.cwd.trim().is_empty() {
            return Err("terminal directory is empty".into());
        }
        self.size.validate()
    }
}

pub fn validate_terminal_id(terminal_id: &str) -> Result<(), String> {
    if terminal_id.trim().is_empty()
        || terminal_id.trim() != terminal_id
        || terminal_id.chars().count() > 128
    {
        return Err("terminal id must be 1 to 128 trimmed characters".into());
    }
    Ok(())
}

/// Keys like shell variables (at most 128 characters), values up to 8192
/// characters, at most 128 entries.
pub fn validate_terminal_env(
    env: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let key = |key: &str| {
        key.len() <= 128
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if env.len() > 128
        || env
            .iter()
            .any(|(name, value)| !key(name) || value.chars().count() > 8192)
    {
        return Err("invalid terminal environment".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalStatus {
    Starting,
    Running,
    Exited,
    Error,
}

/// What lists and tabs show of a thread's terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub thread: agent_domain::ThreadId,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub has_running_subprocess: bool,
    /// The running command's name, otherwise the tab name (`Terminal 1`).
    pub label: String,
    pub updated_at: agent_domain::Timestamp,
}

/// `host/terminal/subscribeMetadata`: every terminal first, then changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TerminalMetadataEvent {
    Snapshot {
        terminals: Vec<TerminalSummary>,
    },
    Upsert {
        terminal: TerminalSummary,
    },
    Remove {
        thread: agent_domain::ThreadId,
        terminal_id: String,
    },
}

/// `Terminal 3` for `term-3` or `terminal-3`; other ids are their own name.
pub fn terminal_label(terminal_id: &str) -> String {
    let lower = terminal_id.to_ascii_lowercase();
    let digits = lower
        .strip_prefix("terminal-")
        .or_else(|| lower.strip_prefix("term-"))
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
    match digits {
        Some(digits) => format!("Terminal {digits}"),
        None => terminal_id.to_owned(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    pub provider: crate::provider::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutAccount {
    pub provider: crate::provider::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    pub provider: crate::provider::ProviderKind,
    #[serde(rename = "loginId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    pub provider: crate::provider::ProviderKind,
    #[serde(rename = "loginId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    pub attachment_mime_type: Option<String>,
    pub directory: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWrite {
    pub process_handle: String,
    #[serde(rename = "deltaBase64", with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKill {
    pub process_handle: String,
    /// Also deletes the terminal's history.
    pub delete_history: bool,
}
/// Shared xterm palette, also used for Host-owned terminal-query replies.
pub fn terminal_color(index: u16) -> u32 {
    const PALETTE: [u32; 16] = [
        0x181818, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let index = u32::from(index);
    match index {
        0..=15 => PALETTE[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            (v << 16) | (v << 8) | v
        }
        257 => 0x181818,
        _ => 0xe5e5e5,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFiles {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRemoteHost {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeDevice {
    #[serde(rename = "nodeId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountUsage {
    pub provider: crate::provider::ProviderKind,
    #[serde(rename = "accountId")]
    pub id: String,
}

/// The prefix every terminal of a thread shares; the Host's cleanup matches it.
pub fn thread_terminal_handle(thread: &str) -> String {
    format!("terminal:{thread}")
}

/// One terminal of a thread, e.g. `term-1` or a setup script's `setup-{id}`.
pub fn thread_terminal_handle_for(thread: &str, terminal_id: &str) -> String {
    format!("{}:{terminal_id}", thread_terminal_handle(thread))
}

/// Shared terminal identity for a workspace, used by cleanup and every client.
pub fn terminal_handle(cwd: &str) -> String {
    format!(
        "bex-terminal-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, cwd.as_bytes())
    )
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    use crate::protocol::Call;
    use serde_json::json;

    // contracts/terminal.ts TerminalClearInput, TerminalRestartInput and the
    // deleteHistory flag of TerminalCloseInput.
    #[test]
    fn clear_restart_and_kill_requests_keep_their_wire_shape() {
        let thread = agent_domain::ThreadId::new("thread-1").unwrap();
        let clear = ClearTerminal {
            thread: thread.clone(),
            terminal_id: "term-1".into(),
        };
        assert_eq!(
            serde_json::to_value(&clear).unwrap(),
            json!({"thread": "thread-1", "terminalId": "term-1"})
        );
        assert_eq!(clear.handle(), "terminal:thread-1:term-1");
        assert_eq!(Call::ClearTerminal(clear).method(), "host/terminal/clear");
        let restart = RestartTerminal {
            thread,
            terminal_id: "term-1".into(),
            cwd: " /work ".into(),
            worktree_path: None,
            size: TerminalSize { cols: 80, rows: 24 },
            env: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(&restart).unwrap(),
            json!({
                "thread": "thread-1", "terminalId": "term-1", "cwd": " /work ",
                "worktreePath": null, "size": {"cols": 80, "rows": 24}, "env": {},
            })
        );
        assert!(restart.validate().is_ok());
        for invalid in [
            RestartTerminal {
                cwd: "  ".into(),
                ..restart.clone()
            },
            RestartTerminal {
                terminal_id: String::new(),
                ..restart.clone()
            },
            RestartTerminal {
                size: TerminalSize { cols: 0, rows: 24 },
                ..restart.clone()
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
        assert_eq!(
            Call::RestartTerminal(restart).method(),
            "host/terminal/restart"
        );
        assert_eq!(
            serde_json::to_value(TerminalKill {
                process_handle: "terminal:thread-1:term-1".into(),
                delete_history: true,
            })
            .unwrap(),
            json!({"processHandle": "terminal:thread-1:term-1", "deleteHistory": true})
        );
    }

    // The icon path update distinguishes unchanged, cleared and set, and the icon
    // reply carries its bytes only when the client's copy is stale.
    #[test]
    fn project_favicon_requests_keep_their_wire_shape() {
        use crate::models::{ProjectFavicon, ReadProjectFavicon};
        use crate::protocol::{Response, decode, encode};
        for favicon_path in [None, Some(None), Some(Some("brand/logo.svg".to_owned()))] {
            let update = UpdateProject {
                project_id: "p".into(),
                scripts: None,
                favicon_path: favicon_path.clone(),
            };
            let Call::UpdateProject(decoded) =
                decode::<Call>(&encode(Call::UpdateProject(update)).unwrap()).unwrap()
            else {
                panic!("an update decodes as an update");
            };
            assert_eq!(decoded.favicon_path, favicon_path);
        }
        let read = ReadProjectFavicon {
            project_id: "p".into(),
            known_hash: Some("ab".into()),
        };
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            json!({"projectId": "p", "knownHash": "ab"})
        );
        assert_eq!(Call::ProjectFavicon(read).method(), "host/project/favicon");
        for data in [None, Some(vec![0, 1, 255])] {
            let favicon = ProjectFavicon {
                hash: "ab".into(),
                file_name: "vab-favicon.svg".into(),
                mime_type: "image/svg+xml".into(),
                data,
            };
            let bytes = encode(Response::from_result::<_, crate::error::RpcFailure>(Ok(
                Some(favicon.clone()),
            )))
            .unwrap();
            let Response::Success { result } =
                decode::<Response<Option<ProjectFavicon>>>(&bytes).unwrap()
            else {
                panic!("the icon decodes");
            };
            assert_eq!(result, Some(favicon.clone()));
            let value = serde_json::to_value(&favicon).unwrap();
            assert_eq!(
                serde_json::from_value::<ProjectFavicon>(value).unwrap(),
                favicon
            );
        }
        assert_eq!(
            serde_json::to_value(ProjectFavicon {
                hash: "ab".into(),
                file_name: "vab-a.png".into(),
                mime_type: "image/png".into(),
                data: Some(vec![1, 2, 3]),
            })
            .unwrap()["data"],
            json!("AQID")
        );
    }

    #[test]
    fn a_thread_terminal_handle_extends_the_thread_prefix_with_its_id() {
        let handle = thread_terminal_handle_for("thread:1", "setup-install");
        assert_eq!(handle, "terminal:thread:1:setup-install");
        assert!(handle.starts_with(&format!("{}:", thread_terminal_handle("thread:1"))));
    }
}
