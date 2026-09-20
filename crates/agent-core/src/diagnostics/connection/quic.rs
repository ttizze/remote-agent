//! Reduce qlog to fixed numeric fields in memory. Raw qlog never reaches disk or the wire.
use super::{ConnectionPhase as Phase, NetworkDetail, Trace};
use noq_proto::{QlogConfig, QlogFactory, Side};
use serde_json::Value;
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, Weak},
    time::Instant,
};

impl Trace {
    pub fn quic_config(self: &Arc<Self>, connection: u64) -> iroh::endpoint::QuicTransportConfig {
        iroh::endpoint::QuicTransportConfig::builder()
            .qlog_factory(Arc::new(Factory {
                trace: Arc::downgrade(self),
                connection,
            }))
            .build()
    }
}

pub(super) struct Factory {
    pub trace: Weak<Trace>,
    pub connection: u64,
}

impl QlogFactory for Factory {
    fn for_connection(
        &self,
        side: Side,
        _remote: SocketAddr,
        cid: noq_proto::ConnectionId,
        _now: Instant,
    ) -> Option<QlogConfig> {
        let trace = self.trace.upgrade()?;
        if !trace.packets() || !trace.active() {
            return None;
        }
        let digest = ring::digest::digest(&ring::digest::SHA256, &cid);
        let id = u64::from_le_bytes(digest.as_ref()[..8].try_into().unwrap());
        trace.record(
            Phase::QuicTraceLinked,
            self.connection,
            id,
            if side == Side::Client { 1 } else { 2 },
        );
        let mut config = QlogConfig::new(Box::new(Writer {
            trace: self.trace.clone(),
            id,
            buffer: Vec::new(),
            oversized: false,
        }));
        config.start_time(trace.origin);
        Some(config)
    }
}

struct Writer {
    trace: Weak<Trace>,
    id: u64,
    buffer: Vec<u8>,
    oversized: bool,
}

impl io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // serde may split a JSON record at any byte. Bound even malformed records.
        for &byte in bytes {
            match byte {
                0x1e => {
                    self.buffer.clear();
                    self.oversized = false;
                }
                b'\n' => {
                    if let Some(trace) = self.trace.upgrade() {
                        if self.oversized {
                            trace.record(Phase::QuicTraceMalformed, self.id, 0, 1);
                        } else if trace.active() {
                            match serde_json::from_slice::<Value>(&self.buffer) {
                                Ok(event) => capture(&trace, self.id, &event),
                                Err(_) => trace.record(Phase::QuicTraceMalformed, self.id, 0, 2),
                            }
                        }
                    }
                    self.buffer.clear();
                }
                _ if !self.oversized => {
                    if self.buffer.len() < 256 * 1024 {
                        self.buffer.push(byte);
                    } else {
                        self.buffer.clear();
                        self.oversized = true;
                    }
                }
                _ => (),
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn number(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}
fn micros(value: &Value) -> u64 {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.)
        .map_or(0, |v| (v * 1000.).round() as u64)
}

fn capture(trace: &Trace, id: u64, event: &Value) {
    let source_at_us = micros(&event["time"]);
    let data = &event["data"];
    let name = event["name"].as_str().unwrap_or("");
    let phase = match name {
        "quic:packet_sent" => Phase::QuicPacketSent,
        "quic:packet_received" => Phase::QuicPacketReceived,
        "quic:packet_lost" => Phase::QuicPacketLost,
        "quic:timer_updated" => {
            let timers = [
                "loss_timeout",
                "ack",
                "path_validation",
                "pacing",
                "nat_traversal_probe_retry",
                "path_challenge_lost",
                "no_available_path",
                "idle_timeout",
                "keep_alive",
                "path_idle",
                "path_keep_alive",
                "discard_path",
                "close",
                "key_discard",
                "push_new_cid",
            ];
            let kind = timers
                .iter()
                .position(|s| Some(*s) == data["timer_type"].as_str())
                .map_or(0, |i| i as u64 + 1);
            let value = match data["event_type"].as_str() {
                Some("set") => 1,
                Some("expired") => 2,
                Some("cancelled") => 3,
                _ => 0,
            };
            trace.record_detail(
                Phase::QuicTimer,
                id,
                0,
                value,
                Some(NetworkDetail {
                    kind,
                    path: number(&data["path_id"]),
                    length: micros(&data["delta"]),
                    source_at_us,
                    ..Default::default()
                }),
            );
            return;
        }
        "quic:metrics_updated" => {
            for (i, key) in [
                "latest_rtt",
                "smoothed_rtt",
                "rtt_variance",
                "min_rtt",
                "pto_count",
                "congestion_window",
                "bytes_in_flight",
                "ssthresh",
            ]
            .into_iter()
            .enumerate()
            {
                if !data[key].is_null() {
                    let value = if i < 4 {
                        micros(&data[key])
                    } else {
                        number(&data[key])
                    };
                    trace.record_detail(
                        Phase::QuicMetric,
                        id,
                        0,
                        value,
                        Some(NetworkDetail {
                            kind: i as u64 + 1,
                            path: number(&data["path_id"]),
                            source_at_us,
                            ..Default::default()
                        }),
                    );
                }
            }
            return;
        }
        _ => return,
    };
    let header = &data["header"];
    let detail = NetworkDetail {
        space: match header["packet_type"].as_str() {
            Some("initial") => 1,
            Some("handshake") => 2,
            Some("0RTT") => 3,
            Some("1RTT") => 4,
            _ => 0,
        },
        packet: number(&header["packet_number"]),
        packet_valid: header["packet_number"].as_u64().is_some(),
        path: number(&header["path_id"]),
        length: number(&header["length"]),
        source_at_us,
        ..Default::default()
    };
    trace.record_detail(phase, id, 0, 0, Some(detail));
    if phase == Phase::QuicPacketLost {
        return;
    }
    let frame_phase = if phase == Phase::QuicPacketSent {
        Phase::QuicFrameSent
    } else {
        Phase::QuicFrameReceived
    };
    for frame in data["frames"].as_array().into_iter().flatten() {
        // Do not copy raw.data, addresses, connection IDs, tokens or error strings.
        let kind = match frame["frame_type"].as_str() {
            Some("stream") => 1,
            Some("ack") => 2,
            Some("crypto") => 3,
            Some("handshake_done") => 4,
            Some("path_challenge") => 5,
            Some("path_response") => 6,
            Some("ping") => 7,
            Some("data_blocked") => 8,
            Some("stream_data_blocked") => 9,
            Some("streams_blocked") => 10,
            Some("max_data") => 11,
            Some("max_stream_data") => 12,
            Some("max_streams") => 13,
            Some("path_ack") => 14,
            _ => continue,
        };
        let mut frame_detail = NetworkDetail {
            kind,
            offset: number(&frame["offset"]),
            length: number(&frame["raw"]["length"]),
            ..detail
        };
        let stream = frame["stream_id"]
            .as_u64()
            .or_else(|| frame["path_id"].as_u64())
            .unwrap_or(0);
        if kind == 2 || kind == 14 {
            for range in frame["acked_ranges"].as_array().into_iter().flatten() {
                if let Some(first) = range[0].as_u64() {
                    frame_detail.offset = first;
                    frame_detail.length = range[1].as_u64().unwrap_or(first);
                    trace.record_detail(
                        frame_phase,
                        id,
                        stream,
                        micros(&frame["ack_delay"]),
                        Some(frame_detail),
                    );
                }
            }
        } else {
            let value = frame["maximum"]
                .as_u64()
                .or_else(|| frame["limit"].as_u64())
                .unwrap_or_else(|| u64::from(frame["fin"].as_bool().unwrap_or(false)));
            trace.record_detail(frame_phase, id, stream, value, Some(frame_detail));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fragmented_qlog_keeps_stream_ranges_and_discards_private_fields() {
        let trace = Trace::new();
        let mut writer = Writer {
            trace: Arc::downgrade(&trace),
            id: 42,
            buffer: Vec::new(),
            oversized: false,
        };
        let event = serde_json::json!({"time":12.345,"name":"quic:packet_received","data":{"header":{"packet_type":"1RTT","packet_number":8,"length":100},"frames":[{"frame_type":"stream","stream_id":4,"offset":7,"raw":{"length":66,"data":"PRIVATE_PAYLOAD"}},{"frame_type":"ack","acked_ranges":[[1],[3,6]]},{"frame_type":"new_connection_id","connection_id":"PRIVATE_ID","stateless_reset_token":"PRIVATE_TOKEN"}],"remote":"PRIVATE_ADDRESS"}});
        let bytes = format!("\u{1e}{event}\n");
        for byte in bytes.as_bytes() {
            writer.write_all(&[*byte]).unwrap();
        }
        let snapshot = trace.snapshot();
        assert_eq!(snapshot.events.len(), 4);
        let frame = &snapshot.events[1];
        assert_eq!(
            (
                frame.stream,
                frame.detail.unwrap().offset,
                frame.detail.unwrap().length
            ),
            (4, 7, 66)
        );
        assert_eq!(frame.detail.unwrap().source_at_us, 12345);
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("PRIVATE")
        );
        writer.write_all(&vec![b'x'; 256 * 1024 + 1]).unwrap();
        writer.write_all(b"\n").unwrap();
        assert_eq!(
            trace.snapshot().events.last().unwrap().phase,
            Phase::QuicTraceMalformed
        );
    }
}
