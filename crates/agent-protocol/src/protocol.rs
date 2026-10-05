//! Bex's binary messages. A QUIC stream, rather than a request ID, owns each reply.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;

mod requests;
pub use requests::{Call, contracts};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Notification {
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
        error: crate::conversation::ConversationError::ResponseTooLarge.into(),
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
    TurnDiff(crate::orchestration::TurnDiff),
    Dispatched(crate::orchestration::DispatchReceipt),
    ShellStream(::orchestration::ShellStreamItem), ThreadStream(::orchestration::ThreadStreamItem),
    Projection(::orchestration::ThreadProjection), TurnItem(Option<::orchestration::TurnItem>),
    ThreadHistory(::orchestration::ThreadHistoryPage), Search(Vec<::orchestration::SearchMatch>), Projects(Vec<crate::models::Project>),
    Committed(crate::conversation::Committed), Launched(crate::conversation::Launched),
    ThreadUpdate(crate::conversation::ThreadUpdate), ShellUpdate(crate::conversation::ShellUpdate),
    ThreadSnapshot(crate::conversation::ThreadSnapshot), HistoryRow(Option<crate::conversation::HistoryRow>),
    HistoryPage(crate::conversation::HistoryPage), SearchMatches(Vec<crate::conversation::SearchMatch>),
    Diff(crate::conversation::TurnDiff), SessionScan(crate::conversation::SessionScan),
    ImportCounts(crate::conversation::ImportCounts),
    Browser(crate::browser::BrowserFrame),
    PermissionSettings(crate::permissions::PermissionSettings),
    Models(crate::operations::ModelPage), WorktreeSettings(crate::models::WorktreeSettings),
    Worktrees(Vec<crate::models::Worktree>), Review(crate::models::WorkspaceReview),
    Empty(crate::models::Empty),
    AccountUsage(crate::operations::AccountUsage),
    Accounts(crate::operations::Accounts), Selected(crate::operations::AccountSelection),
    Login(crate::operations::AccountLogin), LoginStatus(crate::operations::AccountLoginStatus),
    Files(crate::models::FileList), File(crate::models::FileContent), Grant(crate::models::TransferGrant),
    Transcription(crate::operations::Transcription),
    HostStatus(crate::models::HostStatus), Invitation(crate::models::Invitation),
    Remotes(Vec<crate::models::RemoteHost>), Remote(crate::models::RemoteHost),
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
        }))
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
