//! Manages client-side connections to the relay server.
//!
//! based on tailscale/derp/derp_client.go

use std::{
    pin::Pin,
    task::{ready, Context, Poll},
};

use iroh_base::SecretKey;
use n0_error::{anyerr, ensure, stack_error, AnyError};
use n0_future::{Sink, Stream};
use tracing::trace;

use super::KeyCache;
#[cfg(not(wasm_browser))]
use crate::client::streams::{MaybeTlsStream, ProxyStream};
use crate::{
    http::ProtocolVersion,
    protos::{
        handshake,
        relay::{ClientToRelayMsg, Error as ProtoError, RelayToClientMsg},
        streams::WsBytesFramed,
    },
    MAX_PACKET_SIZE,
};

/// Error for sending messages to the relay server.
#[stack_error(derive, add_meta, from_sources, std_sources)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum SendError {
    /// Error returned from the underlying WebSocket stream while sending.
    ///
    /// The concrete error type is `tokio_websockets::Error` on native targets and
    /// `ws_stream_wasm::WsErr` on `wasm_browser` targets. Use [`AnyError::downcast_ref`] to
    /// recover it. Note that the concrete downcast type is not covered by any semver
    /// guarantees and may change between releases.
    #[error("Stream error")]
    StreamError { source: AnyError },
    #[error("Exceeds max packet size ({MAX_PACKET_SIZE}): {size}")]
    ExceedsMaxPacketSize { size: usize },
    #[error("Attempted to send empty packet")]
    EmptyPacket {},
}

/// Errors when receiving messages from the relay server.
#[stack_error(derive, add_meta, from_sources, std_sources)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum RecvError {
    #[error(transparent)]
    Protocol { source: ProtoError },
    /// Error returned from the underlying WebSocket stream while receiving.
    ///
    /// The concrete error type is `tokio_websockets::Error` on native targets and
    /// `ws_stream_wasm::WsErr` on `wasm_browser` targets. Use [`AnyError::downcast_ref`] to
    /// recover it. Note that the concrete downcast type is not covered by any semver
    /// guarantees and may change between releases.
    #[error("Stream error")]
    StreamError { source: AnyError },
}

/// A connection to a relay server.
///
/// This holds a connection to a relay server.  It is:
///
/// - A [`Stream`] for [`RelayToClientMsg`] to receive from the server.
/// - A [`Sink`] for [`ClientToRelayMsg`] to send to the server.
#[derive(derive_more::Debug)]
pub(crate) struct Conn {
    capture: tracing::Span,
    #[cfg(not(wasm_browser))]
    #[debug("tokio_websockets::WebSocketStream")]
    pub(crate) conn: WsBytesFramed<MaybeTlsStream<ProxyStream>>,
    #[cfg(wasm_browser)]
    #[debug("ws_stream_wasm::WsStream")]
    pub(crate) conn: WsBytesFramed,
    pub(crate) key_cache: KeyCache,
    pub(crate) protocol_version: ProtocolVersion,
}

impl Conn {
    /// Constructs a new websocket connection, including the initial server handshake.
    pub(crate) async fn new(
        #[cfg(not(wasm_browser))] io: tokio_websockets::WebSocketStream<
            MaybeTlsStream<ProxyStream>,
        >,
        #[cfg(wasm_browser)] io: ws_stream_wasm::WsStream,
        key_cache: KeyCache,
        secret_key: &SecretKey,
        protocol_version: ProtocolVersion,
    ) -> Result<Self, handshake::Error> {
        let mut conn = WsBytesFramed { io };

        // exchange information with the server
        trace!("server_handshake: started");
        tracing::trace!(target: "bex.net.stage", phase = "relay_auth_start");
        handshake::clientside(&mut conn, secret_key).await?;
        trace!("server_handshake: done");
        tracing::trace!(target: "bex.net.stage", phase = "relay_auth_ready");

        Ok(Self {
            capture: if tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE) {
                tracing::Span::current()
            } else {
                tracing::Span::none()
            },
            conn,
            key_cache,
            protocol_version,
        })
    }

    #[cfg(all(test, feature = "server"))]
    pub(crate) fn test(io: tokio::io::DuplexStream, protocol_version: ProtocolVersion) -> Self {
        use crate::protos::relay::MAX_FRAME_SIZE;
        Self {
            capture: if tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE) {
                tracing::Span::current()
            } else {
                tracing::Span::none()
            },
            conn: WsBytesFramed {
                io: tokio_websockets::ClientBuilder::new()
                    .limits(
                        tokio_websockets::Limits::default().max_payload_len(Some(MAX_FRAME_SIZE)),
                    )
                    .take_over(MaybeTlsStream::Test(io)),
            },
            key_cache: KeyCache::test(),
            protocol_version,
        }
    }
}

impl Stream for Conn {
    type Item = Result<RelayToClientMsg, RecvError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let _entered = this.capture.enter();
        let polled = Pin::new(&mut this.conn).poll_next(cx);
        if polled.is_pending() {
            tracing::trace!(target: "bex.net.packet", phase = "relay_read_pending");
        }
        match ready!(polled) {
            Some(Ok(msg)) => {
                let message =
                    RelayToClientMsg::from_bytes(msg, &this.key_cache, this.protocol_version);
                match &message {
                    Ok(RelayToClientMsg::Datagrams { datagrams, .. }) => {
                        record_datagrams("relay_datagram_received", datagrams)
                    }
                    Ok(RelayToClientMsg::Pong(data)) => {
                        tracing::trace!(target: "bex.net.packet", phase = "relay_pong_received", value = fingerprint(data))
                    }
                    Err(_) => tracing::trace!(target: "bex.net.packet", phase = "relay_read_error"),
                    _ => (),
                }
                Poll::Ready(Some(message.map_err(Into::into)))
            }
            Some(Err(e)) => {
                tracing::trace!(target: "bex.net.packet", phase = "relay_read_error");
                Poll::Ready(Some(Err(anyerr!(e).into())))
            }
            None => Poll::Ready(None),
        }
    }
}

impl Sink<ClientToRelayMsg> for Conn {
    type Error = SendError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = &mut *self;
        let _entered = this.capture.enter();
        let result = Pin::new(&mut this.conn).poll_ready(cx).map_err(Into::into);
        tracing::trace!(target: "bex.net.packet", phase = match &result { Poll::Pending => "relay_send_pending", Poll::Ready(Ok(_)) => "relay_send_ready", Poll::Ready(Err(_)) => "relay_write_error" });
        result
    }

    fn start_send(mut self: Pin<&mut Self>, frame: ClientToRelayMsg) -> Result<(), Self::Error> {
        let this = &mut *self;
        let _entered = this.capture.enter();
        let size = frame.encoded_len();
        ensure!(
            size <= MAX_PACKET_SIZE,
            SendError::ExceedsMaxPacketSize { size }
        );
        if let ClientToRelayMsg::Datagrams { datagrams, .. } = &frame {
            ensure!(!datagrams.contents.is_empty(), SendError::EmptyPacket);
            record_datagrams("relay_datagram_sent", datagrams);
        }
        if let ClientToRelayMsg::Ping(data) = &frame {
            tracing::trace!(target: "bex.net.packet", phase = "relay_ping_sent", value = fingerprint(data));
        }

        Pin::new(&mut this.conn)
            .start_send(frame.to_bytes().freeze())
            .map_err(|error| {
                tracing::trace!(target: "bex.net.packet", phase = "relay_write_error");
                error.into()
            })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = &mut *self;
        let _entered = this.capture.enter();
        let result = Pin::new(&mut this.conn).poll_flush(cx).map_err(Into::into);
        tracing::trace!(target: "bex.net.packet", phase = match &result { Poll::Pending => "relay_flush_pending", Poll::Ready(Ok(_)) => "relay_flush_ready", Poll::Ready(Err(_)) => "relay_write_error" });
        result
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.conn).poll_close(cx).map_err(Into::into)
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(blake3::hash(bytes).as_bytes()[..8].try_into().unwrap())
}

fn record_datagrams(phase: &'static str, datagrams: &crate::protos::relay::Datagrams) {
    if !tracing::event_enabled!(target: "bex.net.packet", tracing::Level::TRACE) {
        return;
    }
    let size = datagrams
        .segment_size
        .map_or(datagrams.contents.len(), |size| usize::from(size.get()))
        .max(1);
    for packet in datagrams.contents.chunks(size) {
        tracing::trace!(target: "bex.net.packet", phase, value = fingerprint(packet), length = packet.len() as u64);
    }
}
