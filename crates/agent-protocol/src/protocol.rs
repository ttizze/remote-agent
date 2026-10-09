//! Bex's binary messages. A QUIC stream, rather than a request ID, owns each reply.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::io;

pub mod json_boundary;
mod requests;
pub use requests::{Call, contracts};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Notification {
    #[serde(rename = "host/taskActivity/changed")]
    TaskActivity {
        state: crate::live_activity::TaskActivityState,
    },
    #[serde(rename = "host/session/activity")]
    Activity {
        session: crate::session::SessionRef,
        active: bool,
        finished: bool,
    },
    #[serde(rename = "host/terminal/output")]
    Output {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "deltaBase64", with = "bytes")]
        data: Vec<u8>,
    },
    #[serde(rename = "host/terminal/exited")]
    Exited {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "exitCode")]
        code: i32,
    },
    #[serde(rename = "host/terminal/failed")]
    TerminalFailed {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "message")]
        reason: String,
    },
    #[serde(rename = "host/terminal/restored")]
    TerminalRestored {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "deltaBase64", with = "bytes")]
        data: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "host/terminal/detached")]
    TerminalDetached {
        #[serde(rename = "processHandle")]
        handle: String,
    },
    #[serde(rename = "host/session/renamed")]
    SessionRenamed {
        session: crate::session::SessionRef,
        name: Option<String>,
        revision: u64,
    },
    #[serde(rename = "host/session/updated")]
    SessionUpdated {
        thread: Box<crate::models::Thread>,
        project: Option<crate::models::Project>,
    },
}

pub fn encode(value: impl Serialize) -> io::Result<Vec<u8>> {
    postcard::to_allocvec(&value).map_err(io::Error::other)
}
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    let (value, remaining) = postcard::take_from_bytes(bytes).map_err(io::Error::other)?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes in frame",
        ));
    }
    Ok(value)
}
pub fn response_frame(response: Response) -> io::Result<Vec<u8>> {
    let bytes = encode(response)?;
    if bytes.len() <= MAX_FRAME_BYTES {
        return Ok(bytes);
    }
    encode(Response::<()>::Failure {
        error: crate::error::RpcFailure {
            code: "response_too_large".into(),
            message: "RPC response exceeds the transfer limit".into(),
            delivery: crate::error::Delivery::Unknown,
            execution: None,
        },
    })
}
// Host handlers return domain results; each serializes directly.
macro_rules! results {
    ($($variant:ident($(#[$attr:meta])* $ty:ty)),* $(,)?) => {
        #[derive(Debug, Serialize)]
        #[serde(untagged)]
        pub enum Body {$($variant($(#[$attr])* Box<$ty>)),*}
        $(impl From<$ty> for Body {fn from(value:$ty)->Self {Self::$variant(Box::new(value))}})*
    }
}
results! {
    TaskActivity(crate::live_activity::TaskActivityState),
    LiveActivity(crate::live_activity::LiveActivityRegistration),
    Browser(crate::browser::BrowserFrame),
    Opened(crate::session::OpenedSession), Item(crate::operations::ItemResponse),
    History(crate::session::HistoryPage),
    Thread(crate::models::ThreadResponse), Threads(crate::models::ThreadList),
    Decorations(crate::operations::ListDecorations),
    Agents(Vec<crate::models::AgentObservation>),
    ComposerCatalog(crate::composer::ComposerCatalog),
    PermissionSettings(crate::permissions::PermissionSettings),
    Models(crate::operations::ModelPage), WorktreeSettings(crate::models::WorktreeSettings),
    Worktrees(Vec<crate::models::Worktree>), Review(crate::models::WorkspaceReview),
    Session(crate::session::SessionRef), Empty(crate::models::Empty),
    AccountUsage(crate::operations::AccountUsage),
    Accounts(crate::operations::Accounts), Selected(crate::operations::AccountSelection),
    Login(crate::operations::AccountLogin), LoginStatus(crate::operations::AccountLoginStatus),
    Files(crate::models::FileList), File(crate::models::FileContent), Grant(crate::models::TransferGrant),
    Transcription(crate::operations::Transcription),
    HostStatus(crate::models::HostStatus), Invitation(crate::models::Invitation),
    Remotes(Vec<crate::models::RemoteHost>), Remote(crate::models::RemoteHost),
    Submission(crate::operations::SubmissionReceipt),
    Unit(()), Text(String)
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Response<T = Body> {
    Success { result: T },
    Failure { error: crate::error::RpcFailure },
}
impl Response {
    pub fn from_result<T: Into<Body>, E: Into<crate::error::RpcFailure>>(
        result: Result<T, E>,
    ) -> Response {
        match result {
            Ok(result) => Response::Success {
                result: result.into(),
            },
            Err(error) => Response::Failure {
                error: error.into(),
            },
        }
    }
    pub fn error(code: impl ToString, message: &impl std::fmt::Display) -> Response {
        Self::from_result::<(), _>(Err(crate::error::RpcFailure {
            code: code.to_string(),
            message: message.to_string(),
            delivery: crate::error::Delivery::Unknown,
            execution: None,
        }))
    }
}
impl<T: Serialize> Response<T> {
    /// Used only by JSON CLI output and provider fixtures.
    pub fn into_value(self) -> Value {
        match self {
            Self::Success { result } => serde_json::json!({"result":result}),
            Self::Failure { error } => serde_json::json!({"error":error}),
        }
    }
}

/// Only intrinsically open provider/tool values use JSON; native records do not.
pub mod json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
    pub fn serialize<T: Serialize, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            value.serialize(serializer)
        } else {
            serde_json::to_string(value)
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer)
        }
    }
    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        if deserializer.is_human_readable() {
            T::deserialize(deserializer)
        } else {
            serde_json::from_str(&String::deserialize(deserializer)?)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// JSON APIs require base64; QUIC transports the original bytes.
pub mod bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<T: AsRef<[u8]> + ?Sized, S: Serializer>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.collect_str(&base64::display::Base64Display::new(
                value.as_ref(),
                &STANDARD,
            ))
        } else {
            serializer.serialize_bytes(value.as_ref())
        }
    }
    pub fn deserialize<'de, T: From<Vec<u8>>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        let bytes = if deserializer.is_human_readable() {
            STANDARD
                .decode(String::deserialize(deserializer)?)
                .map_err(serde::de::Error::custom)?
        } else {
            Vec::<u8>::deserialize(deserializer)?
        };
        Ok(bytes.into())
    }
}
