//! Bex's binary messages. A QUIC stream, rather than a request ID, owns each reply.
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

pub mod json_boundary;
mod requests;
pub use requests::{Call, ProviderCall, contracts};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Notification {
    #[serde(rename = "host/session/activity")]
    Activity {
        session: crate::session::SessionRef,
        active: bool,
        finished: bool,
    },
    #[serde(rename = "process/outputDelta")]
    Output {
        #[serde(rename = "processHandle")]
        handle: String,
        #[serde(rename = "deltaBase64", with = "bytes")]
        data: Vec<u8>,
    },
    #[serde(rename = "process/exited")]
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
    /// Opaque external-provider events for raw subscribers.
    Provider {
        method: String,
        #[serde(with = "json")]
        params: Value,
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
pub async fn write(send: &mut iroh::endpoint::SendStream, value: impl Serialize) -> io::Result<()> {
    write_frame(send, &encode(value)?).await
}
/// Oversized results fail this request without closing unrelated streams.
pub fn response_frame(response: Response) -> io::Result<Vec<u8>> {
    let bytes = encode(response)?;
    if bytes.len() <= MAX_FRAME_BYTES {
        return Ok(bytes);
    }
    encode(Response::<()>::Failure {
        error: serde_json::json!({"code":"response_too_large","message":"RPC response exceeds the transfer limit"}),
    })
}
pub async fn write_frame(send: &mut iroh::endpoint::SendStream, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds frame limit",
        ));
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(io::Error::other)?;
    send.write_all(bytes).await.map_err(io::Error::other)
}
pub struct Reader(FramedRead<iroh::endpoint::RecvStream, LengthDelimitedCodec>);
impl Reader {
    pub fn new(recv: iroh::endpoint::RecvStream) -> Self {
        Self(
            LengthDelimitedCodec::builder()
                .max_frame_length(MAX_FRAME_BYTES)
                .new_read(recv),
        )
    }
    pub async fn read_frame(&mut self) -> io::Result<Option<tokio_util::bytes::BytesMut>> {
        self.0.next().await.transpose()
    }
    /// FramedRead retains partial frames if this future loses a select.
    pub async fn read<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        let Some(bytes) = self.0.next().await.transpose()? else {
            return Ok(None);
        };
        decode(&bytes).map(Some)
    }
}

// Host handlers may return different native results; each serializes directly.
macro_rules! results {
    ($($variant:ident($(#[$attr:meta])* $ty:ty)),* $(,)?) => {
        #[derive(Debug, Serialize)]
        #[serde(untagged)]
        pub enum Body {$($variant($(#[$attr])* Box<$ty>)),*}
        $(impl From<$ty> for Body {fn from(value:$ty)->Self {Self::$variant(Box::new(value))}})*
    }
}
results! {
    Browser(crate::browser::BrowserFrame),
    Opened(crate::session::OpenedSession), Item(crate::client::ItemResponse),
    Thread(crate::models::ThreadResponse), Threads(crate::models::ThreadList),
    ComposerCatalog(crate::composer::ComposerCatalog),
    Models(crate::client::ModelPage), WorktreeSettings(crate::models::WorktreeSettings),
    Worktrees(Vec<crate::models::Worktree>), Review(crate::models::WorkspaceReview),
    Session(crate::session::SessionRef), Empty(crate::models::Empty),
    AccountUsage(crate::client::AccountUsage),
    Accounts(crate::client::Accounts), Selected(crate::client::AccountSelection),
    Login(crate::client::AccountLogin), LoginStatus(crate::client::AccountLoginStatus),
    Files(crate::models::FileList), File(crate::models::FileContent), Grant(crate::models::TransferGrant),
    Transcription(crate::client::Transcription),
    HostStatus(crate::models::HostStatus), Invitation(crate::models::Invitation),
    Remotes(Vec<crate::models::RemoteHost>), Remote(crate::models::RemoteHost),
    Started(crate::client::StartedTurn), Queued(crate::client::QueuedTurn),
    Unit(()), Text(String), Provider(#[serde(with = "json")] Value)
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Response<T = Body> {
    Success {
        result: T,
    },
    Failure {
        #[serde(with = "json")]
        error: Value,
    },
}
impl Response {
    pub fn from_result<T: Into<Body>, E: Serialize>(
        result: Result<T, E>,
    ) -> Result<Response, crate::peer::RpcMessageError> {
        Ok(match result {
            Ok(result) => Response::Success {
                result: result.into(),
            },
            Err(error) => Response::Failure {
                error: serde_json::to_value(error)?,
            },
        })
    }
    pub fn error(
        code: impl Serialize,
        message: &impl std::fmt::Display,
    ) -> Result<Response, crate::peer::RpcMessageError> {
        Self::from_result::<(), _>(Err(
            serde_json::json!({"code":code,"message":message.to_string()}),
        ))
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
