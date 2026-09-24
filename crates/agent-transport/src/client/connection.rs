//! Bex requests own independent QUIC streams. Provider JSONL state stays in `peer`.
use crate::protocol;
use crate::{
    diagnostics::{ConnectionPhase as Phase, connection::Trace},
    framing,
    peer::{Delivery, PeerError},
    protocol::{Call, Response},
};
use serde::de::DeserializeOwned;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
pub(crate) const EVENTS: u8 = 0;
pub(crate) const CALL: u8 = 1;
pub(crate) const BLOB: u8 = 2;
pub(crate) const CLOSE: u8 = 3;
pub type Updates = framing::Reader;
pub type HostPeer = iroh::endpoint::SendStream;
pub struct HostRequest {
    pub call: Call,
    pub send: iroh::endpoint::SendStream,
    pub accepted_at: std::time::Instant,
    pub decoded_at: std::time::Instant,
}
pub struct Client {
    connection: iroh::endpoint::Connection,
    pub trace: Arc<Trace>,
    pub diagnostic_id: u64,
    permits: Arc<Semaphore>,
    timeout: Duration,
    initial_list: Mutex<Option<(crate::models::ListQuery, PendingReply)>>,
    _path_monitor: Option<tokio_util::task::AbortOnDropHandle<()>>,
}
struct PendingReply {
    response: tokio_util::task::AbortOnDropHandle<
        Result<(tokio_util::bytes::BytesMut, Updates), PeerError>,
    >,
    stream: u64,
    _permit: OwnedSemaphorePermit,
}
impl Client {
    fn path_sample(&self) -> (crate::diagnostics::ConnectionRoute, Option<u64>) {
        use crate::diagnostics::ConnectionRoute;
        self.connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map_or((ConnectionRoute::Unknown, None), |path| {
                let route = if path.is_ip() {
                    ConnectionRoute::Direct
                } else if path.is_relay() {
                    ConnectionRoute::Relay
                } else {
                    ConnectionRoute::Unknown
                };
                (route, Some(path.rtt().as_micros() as u64))
            })
    }
    pub fn connection_path(&self) -> (crate::diagnostics::ConnectionRoute, u64) {
        let (route, rtt) = self.path_sample();
        (route, rtt.unwrap_or(0) / 1000)
    }

    pub(crate) async fn connect(
        connection: iroh::endpoint::Connection,
        timeout: Duration,
        max_requests: usize,
        trace: Arc<Trace>,
        diagnostic_id: u64,
    ) -> Result<(Self, Updates), PeerError> {
        if max_requests == 0 {
            return Err(invalid("max_requests must be positive"));
        }
        let (mut send, recv) = connection.open_bi().await.map_err(invalid)?;
        send.write_all(&[EVENTS]).await.map_err(invalid)?;
        send.finish().map_err(invalid)?;
        trace.record(Phase::EventsOpened, diagnostic_id, u64::from(send.id()), 0);
        let path_monitor = trace.enabled().then(|| {
            let mut paths = connection.path_events();
            let weak_trace = Arc::downgrade(&trace);
            tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                use futures_util::StreamExt;
                use iroh::endpoint::PathEvent;
                while let Some(event) = paths.next().await {
                    let Some(trace) = weak_trace.upgrade() else {
                        break;
                    };
                    let (phase, value) = match event {
                        PathEvent::Opened { remote_addr, .. } => {
                            (Phase::PathOpened, route_code(&remote_addr))
                        }
                        PathEvent::Closed { remote_addr, .. } => {
                            (Phase::PathClosed, route_code(&remote_addr))
                        }
                        PathEvent::Selected { remote_addr, .. } => {
                            (Phase::PathSelected, route_code(&remote_addr))
                        }
                        PathEvent::Lagged { missed, .. } => (Phase::PathEventsDropped, missed),
                        _ => continue,
                    };
                    trace.record(phase, diagnostic_id, 0, value);
                }
            }))
        });
        let client = Self {
            connection,
            trace,
            diagnostic_id,
            permits: Arc::new(Semaphore::new(max_requests)),
            timeout,
            initial_list: Mutex::new(None),
            _path_monitor: path_monitor,
        };
        client.record_path();
        Ok((client, framing::Reader::new(recv)))
    }
    pub async fn close(&self) {
        // Observe shutdown before a short-lived CLI exits its runtime.
        tokio::select! {
            _ = self.connection.closed() => {},
            _ = tokio::time::timeout(Duration::from_secs(3), async {
                let (mut send, mut recv) = self.connection.open_bi().await.map_err(invalid)?;
                send.write_all(&[CLOSE]).await.map_err(invalid)?;
                send.finish().map_err(invalid)?;
                recv.read_exact(&mut [0u8; 1]).await.map_err(invalid)?;
                Ok::<(), PeerError>(())
            }) => {},
        }
        self.connection.close(0u8.into(), b"peer closed");
    }
    pub async fn request<T: DeserializeOwned>(&self, call: &Call) -> Result<T, PeerError> {
        self.request_stream(call).await.map(|(result, _)| result)
    }
    pub async fn request_stream<T: DeserializeOwned>(
        &self,
        call: &Call,
    ) -> Result<(T, Updates), PeerError> {
        let (initial, updates) = self.call_stream(call).await?;
        let started = std::time::Instant::now();
        let result = protocol::decode(&initial)
            .map_err(invalid)
            .and_then(response);
        if self.trace.active() && !matches!(call, Call::ConnectionPerformance(_)) {
            self.trace.record(
                Phase::ResponseDecoded,
                self.diagnostic_id,
                updates.stream_id(),
                started.elapsed().as_micros() as u64,
            );
        }
        result.map(|result| (result, updates))
    }
    /// Send the first title read while storage-scope verification is in flight.
    /// Its ordinary caller consumes the reply once, with the original deadline.
    pub async fn start_initial_list(
        &self,
        query: crate::models::ListQuery,
    ) -> Result<(), PeerError> {
        let reply = self
            .start_call(&Call::ListThreads(
                agent_protocol::operations::ListThreads::new(query.clone()),
            ))
            .await?;
        *self.initial_list.lock().unwrap() = Some((query, reply));
        Ok(())
    }
    async fn start_call(&self, call: &Call) -> Result<PendingReply, PeerError> {
        let started = std::time::Instant::now();
        let deadline = Instant::now() + self.timeout;
        let measured = self.trace.active() && !matches!(call, Call::ConnectionPerformance(_));
        let work = async {
            let permit = self
                .permits
                .clone()
                .acquire_owned()
                .await
                .map_err(invalid)?;
            let permit_wait = started.elapsed().as_micros() as u64;
            let (mut send, recv) = self.connection.open_bi().await.map_err(invalid)?;
            let stream = u64::from(send.id());
            if measured {
                self.trace.record(
                    Phase::RequestSlotWait,
                    self.diagnostic_id,
                    stream,
                    permit_wait,
                );
                self.trace.record(
                    Phase::RequestOpened,
                    self.diagnostic_id,
                    stream,
                    started.elapsed().as_micros() as u64,
                );
            }
            send.write_all(&[CALL]).await.map_err(invalid)?;
            let encoding = std::time::Instant::now();
            let bytes = protocol::encode(call).map_err(invalid)?;
            if measured {
                self.trace.record(
                    Phase::RequestEncoded,
                    self.diagnostic_id,
                    stream,
                    encoding.elapsed().as_micros() as u64,
                );
            }
            framing::write_frame(&mut send, &bytes)
                .await
                .map_err(invalid)?;
            send.finish().map_err(invalid)?;
            if measured {
                self.trace.record(
                    Phase::RequestSent,
                    self.diagnostic_id,
                    stream,
                    bytes.len() as u64,
                );
            }
            let mut reader = if measured {
                framing::Reader::observed(recv, self.trace.clone(), self.diagnostic_id)
            } else {
                framing::Reader::new(recv)
            };
            let method = call.method().to_owned();
            let response = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                let read = async {
                    let initial = reader
                        .read_frame()
                        .await
                        .map_err(invalid)?
                        .ok_or_else(|| invalid("response stream ended before its result"))?;
                    Ok((initial, reader))
                };
                tokio::time::timeout_at(deadline, read)
                    .await
                    .unwrap_or_else(|_| Err(PeerError::RequestTimeout { method }))
            }));
            Ok(PendingReply {
                response,
                stream,
                _permit: permit,
            })
        };
        tokio::select! {
            reason = self.connection.closed() => Err(PeerError::ConnectionClosed(reason.to_string())),
            result = tokio::time::timeout_at(deadline, work) => result.unwrap_or_else(|_| Err(PeerError::RequestTimeout {method: call.method().into()})),
        }
    }
    async fn call_stream(
        &self,
        call: &Call,
    ) -> Result<(tokio_util::bytes::BytesMut, Updates), PeerError> {
        let initial = if let Call::ListThreads(params) = call {
            // A changed query discards the old read instead of publishing it or
            // retaining a semaphore slot for the lifetime of the connection.
            self.initial_list
                .lock()
                .unwrap()
                .take()
                .filter(|(query, _)| *query == params.query)
                .map(|(_, reply)| reply)
        } else {
            None
        };
        let mut reply = match initial {
            Some(reply) => reply,
            None => self.start_call(call).await?,
        };
        let stream = reply.stream;
        let measured = self.trace.active() && !matches!(call, Call::ConnectionPerformance(_));
        if measured {
            self.trace
                .record(Phase::ReplyAdopted, self.diagnostic_id, stream, 0);
        }
        let result = tokio::select! {
            reason = self.connection.closed() => Err(PeerError::ConnectionClosed(reason.to_string())),
            result = &mut reply.response => result.map_err(invalid).and_then(|result| result),
        };
        if measured {
            if result.is_err() {
                self.trace
                    .record(Phase::RequestFailed, self.diagnostic_id, stream, 0);
            }
            self.record_path();
        }
        result
    }
    pub async fn collect_connection_diagnostics(
        &self,
        mut performance: crate::diagnostics::ConnectionPerformance,
    ) {
        if !self.trace.enabled() {
            return;
        }
        // Freeze capture before sending, so export cannot generate its own timeline.
        tokio::time::sleep(crate::diagnostics::connection::WINDOW + Duration::from_secs(1)).await;
        performance.timeline = self.trace.snapshot();
        if matches!(
            tokio::time::timeout(Duration::from_secs(10), self.call(&performance)).await,
            Ok(Ok(_))
        ) {
            self.trace.acknowledge(&performance).await;
        }
        for report in self.trace.pending_reports().await {
            if matches!(
                tokio::time::timeout(Duration::from_secs(10), self.call(&report)).await,
                Ok(Ok(_))
            ) {
                self.trace.acknowledge(&report).await;
            } else {
                break;
            }
        }
    }
    pub fn record_path(&self) {
        if !self.trace.active() {
            return;
        }
        use crate::diagnostics::ConnectionRoute;
        let (route, rtt) = self.path_sample();
        let phase = match route {
            ConnectionRoute::Direct => Phase::PathDirect,
            ConnectionRoute::Relay => Phase::PathRelay,
            ConnectionRoute::Unknown => Phase::PathUnknown,
        };
        self.trace.record(phase, self.diagnostic_id, 0, 0);
        if let Some(rtt) = rtt {
            self.trace
                .record(Phase::RttMicros, self.diagnostic_id, 0, rtt);
        }
        let stats = self.connection.stats();
        self.trace.record(
            Phase::LostPackets,
            self.diagnostic_id,
            0,
            stats.lost_packets,
        );
        self.trace
            .record(Phase::LostBytes, self.diagnostic_id, 0, stats.lost_bytes);
        self.trace.record(
            Phase::CryptoFramesSent,
            self.diagnostic_id,
            0,
            stats.frame_tx.crypto,
        );
        self.trace.record(
            Phase::CryptoFramesReceived,
            self.diagnostic_id,
            0,
            stats.frame_rx.crypto,
        );
        self.trace.record(
            Phase::SentPackets,
            self.diagnostic_id,
            0,
            stats.udp_tx.datagrams,
        );
        self.trace.record(
            Phase::ReceivedPackets,
            self.diagnostic_id,
            0,
            stats.udp_rx.datagrams,
        );
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.connection.close(0u8.into(), b"peer dropped");
    }
}
fn route_code(address: &iroh::TransportAddr) -> u64 {
    match address {
        iroh::TransportAddr::Ip(_) => 1,
        iroh::TransportAddr::Relay(_) => 2,
        _ => 0,
    }
}
fn response<T>(response: Response<T>) -> Result<T, PeerError> {
    match response {
        Response::Success { result } => Ok(result),
        Response::Failure { error } => Err(PeerError::Remote {
            delivery: serde_json::from_value(error["delivery"].clone())
                .unwrap_or(Delivery::Unknown),
            error: error.to_string(),
            sequence: None,
        }),
    }
}
fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Endpoint, Identity, IncomingRequest, Relays, Trust};

    #[tokio::test]
    async fn reconnect_uploads_previous_capture_and_only_then_removes_it() {
        use crate::diagnostics::{ConnectionPerformance, connection::Trace};
        tokio::time::timeout(Duration::from_secs(45), async {
            let directory = tempfile::tempdir().unwrap();
            let old = Trace::new();
            old.persist(directory.path().into()).unwrap();
            old.record(Phase::ResumeStart, 17, 0, 0);
            old.record(Phase::ResumeFailed, 17, 0, 1000);
            assert_eq!(old.pending_reports().await.len(), 1);
            let old_id = old.id;
            drop(old);

            let trace = Trace::new();
            trace.persist(directory.path().into()).unwrap();
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let endpoint =
                Endpoint::bind_recording(Identity::generate(), Relays::Disabled, trace.clone())
                    .await
                    .unwrap();
            let trust = Trust {
                allowed: [endpoint.node_id()].into(),
                ..Default::default()
            };
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(endpoint.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let (peer, events) = tokio::join!(
                session.open_peer(Duration::from_secs(2), 1),
                incoming.accept_peer()
            );
            let (peer, _updates) = peer.unwrap();
            let _events = events.unwrap();
            let upload = peer.collect_connection_diagnostics(ConnectionPerformance {
                connection_id: peer.diagnostic_id,
                ..Default::default()
            });
            let receiver = async {
                for recovered in [false, true] {
                    let IncomingRequest::Call(request) = incoming.accept_request().await.unwrap()
                    else {
                        panic!("diagnostic call expected")
                    };
                    let Call::ConnectionPerformance(report) = request.call else {
                        panic!("diagnostic report expected")
                    };
                    assert_eq!(report.recovered, recovered);
                    if recovered {
                        assert_eq!(report.timeline.id, old_id);
                        assert_eq!(report.attempt_id, 17);
                        assert!(
                            report
                                .timeline
                                .events
                                .iter()
                                .any(|event| event.phase == Phase::ResumeFailed)
                        );
                        assert!(
                            trace
                                .pending_reports()
                                .await
                                .iter()
                                .any(|report| report.timeline.id == old_id)
                        );
                    }
                    let mut send = request.send;
                    framing::write(
                        &mut send,
                        Response::Success {
                            result: crate::models::Empty {},
                        },
                    )
                    .await
                    .unwrap();
                    send.finish().unwrap();
                }
            };
            tokio::join!(upload, receiver);
            assert!(trace.pending_reports().await.is_empty());
            session.close();
            incoming.close();
            endpoint.close().await;
            host.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn early_reply_is_measured_before_adoption_without_exposing_its_payload() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let trust = Trust { allowed: [client.node_id()].into(), ..Default::default() };
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(client.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let (remote, events) = tokio::join!(session.open_peer(Duration::from_secs(2), 1), incoming.accept_peer());
            let (remote, _updates) = remote.unwrap();
            let _events = events.unwrap();
            let call = Call::SessionScope(crate::models::Empty {});
            let mut pending = remote.start_call(&call).await.unwrap();
            let crate::transport::IncomingRequest::Call(mut request) = incoming.accept_request().await.unwrap() else { panic!("call expected") };
            let stream = u64::from(request.send.id());
            framing::write(&mut request.send, Response::Success { result: "private-payload" }).await.unwrap();
            request.send.finish().unwrap();
            loop {
                if remote.trace.snapshot().events.iter().any(|event| event.phase == Phase::ResponseReceived && event.stream == stream) { break; }
                tokio::task::yield_now().await;
            }
            // The owner has not adopted/awaited this reply, but wire reading is done.
            let snapshot = remote.trace.snapshot();
            assert!(snapshot.events.iter().any(|event| event.phase == Phase::ResponseFirstRead && event.stream == stream));
            assert!(!snapshot.events.iter().any(|event| event.phase == Phase::ReplyAdopted));
            assert!(!serde_json::to_string(&snapshot).unwrap().contains("private-payload"));
            let (bytes, _) = (&mut pending.response).await.unwrap().unwrap();
            assert!(matches!(protocol::decode::<Response<String>>(&bytes).unwrap(), Response::Success { result } if result == "private-payload"));
            drop(pending);
            assert_eq!(remote.permits.available_permits(), 1);
            session.close(); incoming.close(); client.close().await; host.close().await;
        }).await.unwrap();
    }

    #[tokio::test]
    async fn pipelined_read_keeps_its_deadline_and_releases_its_request_slot() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let trust = Trust {
                allowed: [client.node_id()].into(),
                ..Default::default()
            };
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(client.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let (remote, events) = tokio::join!(
                session.open_peer(Duration::from_millis(100), 1),
                incoming.accept_peer()
            );
            let (remote, _updates) = remote.unwrap();
            let _events = events.unwrap();
            let query = crate::models::ListQuery::default();
            remote.start_initial_list(query.clone()).await.unwrap();
            let IncomingRequest::Call(pending) = incoming.accept_request().await.unwrap() else {
                panic!("title read expected")
            };
            assert!(matches!(pending.call, Call::ListThreads(_)));
            tokio::time::sleep(Duration::from_millis(120)).await;
            assert!(matches!(
                tokio::time::timeout(
                    Duration::from_millis(50),
                    remote.call(&agent_protocol::operations::ListThreads::new(query))
                )
                .await
                .unwrap(),
                Err(PeerError::RequestTimeout { .. })
            ));
            assert_eq!(remote.permits.available_permits(), 1);
            tokio::time::timeout(Duration::from_millis(100), pending.send.stopped())
                .await
                .unwrap()
                .unwrap();
            session.close();
            incoming.close();
            client.close().await;
            host.close().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unread_response_cancellation_and_timeout_do_not_block_other_calls() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let trust = Trust {
                allowed: [client.node_id()].into(),
                ..Default::default()
            };
            let ticket = host.ticket();
            let (session, incoming) = tokio::join!(client.connect(&ticket), host.accept());
            let session = session.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
            let (remote, host_peer) = tokio::join!(
                session.open_peer(Duration::from_secs(3), 8),
                incoming.accept_peer()
            );
            let (remote, mut events) = remote.unwrap();
            let host_peer = host_peer.unwrap();
            let serving = tokio::spawn(async move {
                let mut host_peer = host_peer;
                for code in 0..5000 {
                    framing::write(
                        &mut host_peer,
                        protocol::Notification::Exited {
                            handle: "terminal".into(),
                            code,
                        },
                    )
                    .await
                    .unwrap();
                }
                let mut requests = tokio::task::JoinSet::new();
                loop {
                    match incoming.accept_request().await.unwrap() {
                        IncomingRequest::Call(request) => {
                            requests.spawn(async move {
                                let mut send = request.send;
                                if request.call.method() == "malformed" {
                                    let _ = framing::write_frame(&mut send, &[0]).await;
                                    return;
                                }
                                if request.call.method() == "silent" {
                                    let _ = send.stopped().await;
                                    return;
                                }
                                let body = if request.call.method() == "large" {
                                    "x".repeat(8 * 1024 * 1024)
                                } else {
                                    "pong".into()
                                };
                                let _ = crate::framing::write(
                                    &mut send,
                                    crate::protocol::Response::Success { result: body },
                                )
                                .await;
                            });
                        }
                        IncomingRequest::Close => break,
                        IncomingRequest::Blob(_) => panic!("unexpected blob"),
                    }
                }
                requests.abort_all();
                while requests.join_next().await.is_some() {}
            });
            let (mut send, mut recv) = remote.connection.open_bi().await.unwrap();
            send.write_all(&[CALL]).await.unwrap();
            crate::framing::write(
                &mut send,
                crate::protocol::Call::Provider(crate::protocol::ProviderCall {
                    method: "large".into(),
                    params: serde_json::json!({}),
                }),
            )
            .await
            .unwrap();
            send.finish().unwrap();
            recv.read_exact(&mut [0u8; 1]).await.unwrap();
            assert_eq!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "ping".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .unwrap(),
                "pong"
            );
            drop((send, recv));
            assert!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "malformed".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .is_err()
            );
            assert!(matches!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "silent".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await,
                Err(PeerError::RequestTimeout { .. })
            ));
            assert_eq!(
                remote
                    .request::<String>(&crate::protocol::Call::Provider(
                        crate::protocol::ProviderCall {
                            method: "ping".into(),
                            params: serde_json::json!({})
                        }
                    ))
                    .await
                    .unwrap(),
                "pong"
            );
            // A slow consumer keeps every event in order, beyond the former
            // broadcast capacity, while independent RPCs keep completing.
            for expected in 0..5000 {
                let Some(protocol::Notification::Exited { code, .. }) =
                    events.read().await.unwrap()
                else {
                    panic!("missing terminal event")
                };
                assert_eq!(code, expected);
            }
            remote.close().await;
            serving.await.unwrap();
            client.close().await;
            host.close().await;
        })
        .await
        .unwrap();
    }
}
