//! Bounded parsers for the two live device transports.
//!
//! The helper owns the encoders and the Host owns the transport boundary.  A
//! client therefore receives complete, typed access units instead of an
//! unauthenticated hub socket or an accidental screenshot poll.

use agent_protocol::device::DeviceFrameEncoding;

pub const MAX_STREAM_CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFrame {
    pub payload: Vec<u8>,
    pub encoding: DeviceFrameEncoding,
    pub keyframe: bool,
    pub timestamp_us: Option<u64>,
    pub screen_id: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvccChunkKind {
    Description,
    Keyframe,
    Delta,
    Seed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvccChunk {
    pub kind: AvccChunkKind,
    pub payload: Vec<u8>,
}

/// Consume complete `u32be length, u8 tag, payload` envelopes from a
/// fragmented serve-sim stream. Unknown tags are dropped after their length
/// has been validated, keeping the parser aligned for future helper metadata.
#[derive(Debug, Default)]
pub struct AvccDemuxer {
    buffer: Vec<u8>,
}

impl AvccDemuxer {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<AvccChunk>, String> {
        if bytes.len() > MAX_STREAM_CHUNK || self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_CHUNK {
            return Err("device AVCC stream chunk exceeds the Host limit".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut offset = 0;
        let mut chunks = Vec::new();
        while self.buffer.len().saturating_sub(offset) >= 4 {
            let length = u32::from_be_bytes(self.buffer[offset..offset + 4].try_into().unwrap()) as usize;
            if length > MAX_STREAM_CHUNK {
                return Err("device AVCC envelope length is invalid".into());
            }
            if length == 0 {
                offset += 4;
                continue;
            }
            if self.buffer.len().saturating_sub(offset + 4) < length {
                break;
            }
            let tag = self.buffer[offset + 4];
            let payload_start = offset + 5;
            let payload_end = offset + 4 + length;
            if payload_start <= payload_end {
                let kind = match tag {
                    1 => Some(AvccChunkKind::Description),
                    2 => Some(AvccChunkKind::Keyframe),
                    3 => Some(AvccChunkKind::Delta),
                    4 => Some(AvccChunkKind::Seed),
                    _ => None,
                };
                if let Some(kind) = kind {
                    chunks.push(AvccChunk {
                        kind,
                        payload: self.buffer[payload_start..payload_end].to_vec(),
                    });
                }
            }
            offset = payload_end;
        }
        if offset != 0 {
            self.buffer.drain(..offset);
        }
        Ok(chunks)
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// Split Android's `SEMU` header from the Annex-B H.264 access unit.
pub fn parse_semu_packet(bytes: &[u8]) -> TransportFrame {
    if bytes.len() >= 16
        && bytes[0..4] == *b"SEMU"
        && bytes[4] == 1
    {
        let timestamp_us = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
        return TransportFrame {
            payload: bytes[16..].to_vec(),
            encoding: DeviceFrameEncoding::Semu,
            keyframe: bytes[5] & 1 != 0,
            timestamp_us: Some(timestamp_us),
            screen_id: None,
        };
    }
    let keyframe = annex_b_has_keyframe(bytes);
    TransportFrame {
        payload: bytes.to_vec(),
        encoding: DeviceFrameEncoding::H264,
        keyframe,
        timestamp_us: None,
        screen_id: None,
    }
}

pub fn annex_b_has_keyframe(bytes: &[u8]) -> bool {
    annex_b_nal_types(bytes).contains(&5)
}

pub fn annex_b_nal_types(bytes: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        let start = if bytes[index..].starts_with(&[0, 0, 1]) {
            Some(index + 3)
        } else if index + 4 <= bytes.len() && bytes[index..].starts_with(&[0, 0, 0, 1]) {
            Some(index + 4)
        } else {
            None
        };
        if let Some(start) = start {
            if start < bytes.len() {
                types.push(bytes[start] & 0x1f);
            }
            index = start;
        } else {
            index += 1;
        }
    }
    types
}

pub fn jpeg_bounds(bytes: &[u8]) -> Option<(usize, usize)> {
    let start = bytes.windows(2).position(|window| window == [0xff, 0xd8])?;
    let end = bytes[start + 2..]
        .windows(2)
        .position(|window| window == [0xff, 0xd9])?
        + start
        + 4;
    Some((start, end))
}

/// A bounded transport recorder.  AVCC recordings use the helper's
/// length-prefixed tag stream and SEMU recordings keep their native header;
/// MJPEG recordings use a multipart response body.  This means an attachment
/// can be handed to the same decoder as the live transport without a private
/// recording envelope.
#[derive(Debug, Clone)]
pub struct RawFrameRecorder {
    format: agent_protocol::device::DeviceRecordingFormat,
    started_at: String,
    frame_count: u64,
    byte_count: u64,
    bytes: Vec<u8>,
    finished: bool,
}

impl RawFrameRecorder {
    pub fn new(format: agent_protocol::device::DeviceRecordingFormat, started_at: String) -> Self {
        Self { format, started_at, frame_count: 0, byte_count: 0, bytes: Vec::new(), finished: false }
    }

    pub fn push(&mut self, frame: &TransportFrame) -> Result<(), String> {
        if self.finished {
            return Err("device recording has already been finalized".into());
        }
        if matches!(self.format, agent_protocol::device::DeviceRecordingFormat::Avcc)
            && matches!(frame.encoding, DeviceFrameEncoding::Semu)
        {
            if self.bytes.len().saturating_add(16).saturating_add(frame.payload.len()) > MAX_STREAM_CHUNK {
                return Err("device recording exceeded the Host byte limit".into());
            }
            self.bytes.extend_from_slice(b"SEMU");
            self.bytes.extend_from_slice(&[1, u8::from(frame.keyframe), 0, 0]);
            self.bytes.extend_from_slice(&frame.timestamp_us.unwrap_or_default().to_be_bytes());
            self.bytes.extend_from_slice(&frame.payload);
            self.frame_count = self.frame_count.saturating_add(1);
            self.byte_count = self.bytes.len() as u64;
            return Ok(());
        }
        match self.format {
            agent_protocol::device::DeviceRecordingFormat::Mjpeg => {
                if !matches!(frame.encoding, DeviceFrameEncoding::Mjpeg | DeviceFrameEncoding::Jpeg) {
                    return Err("device MJPEG recording received a non-JPEG frame".into());
                }
                let (start, end) = jpeg_bounds(&frame.payload)
                    .ok_or_else(|| "device MJPEG recording received an invalid JPEG frame".to_owned())?;
                let payload = &frame.payload[start..end];
                let header = format!(
                    "--remote-agent-device\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                if self.bytes.len().saturating_add(header.len()).saturating_add(payload.len()).saturating_add(2) > MAX_STREAM_CHUNK {
                    return Err("device recording exceeded the Host byte limit".into());
                }
                self.bytes.extend_from_slice(header.as_bytes());
                self.bytes.extend_from_slice(payload);
                self.bytes.extend_from_slice(b"\r\n");
            }
            agent_protocol::device::DeviceRecordingFormat::Avcc => {
                if !matches!(
                    frame.encoding,
                    DeviceFrameEncoding::AvccDescription | DeviceFrameEncoding::H264 | DeviceFrameEncoding::Jpeg
                ) {
                    return Err("device AVCC recording received an unsupported frame".into());
                }
                let length = u32::try_from(frame.payload.len()).map_err(|_| "device frame is too large")?;
                if self.bytes.len().saturating_add(5).saturating_add(frame.payload.len()) > MAX_STREAM_CHUNK {
                    return Err("device recording exceeded the Host byte limit".into());
                }
                self.bytes.extend_from_slice(&length.to_be_bytes());
                self.bytes.push(match frame.encoding {
                    DeviceFrameEncoding::AvccDescription => 1,
                    DeviceFrameEncoding::H264 => if frame.keyframe { 2 } else { 3 },
                    DeviceFrameEncoding::Jpeg => 4,
                    DeviceFrameEncoding::Mjpeg | DeviceFrameEncoding::Semu | DeviceFrameEncoding::Png => unreachable!(),
                });
                self.bytes.extend_from_slice(&frame.payload);
            }
            agent_protocol::device::DeviceRecordingFormat::RawFrames => {
                if self.bytes.len().saturating_add(frame.payload.len()) > MAX_STREAM_CHUNK {
                    return Err("device recording exceeded the Host byte limit".into());
                }
                self.bytes.extend_from_slice(&frame.payload);
            }
        }
        self.frame_count = self.frame_count.saturating_add(1);
        self.byte_count = self.bytes.len() as u64;
        Ok(())
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.format == agent_protocol::device::DeviceRecordingFormat::Mjpeg && !self.bytes.is_empty() {
            const END: &[u8] = b"--remote-agent-device--\r\n";
            if self.bytes.len().saturating_add(END.len()) <= MAX_STREAM_CHUNK {
                self.bytes.extend_from_slice(END);
                self.byte_count = self.bytes.len() as u64;
            }
        }
    }

    pub fn format(&self) -> agent_protocol::device::DeviceRecordingFormat { self.format }
    pub fn started_at(&self) -> &str { &self.started_at }
    pub fn frame_count(&self) -> u64 { self.frame_count }
    pub fn byte_count(&self) -> u64 { self.byte_count }
    pub fn bytes(&self) -> &[u8] { &self.bytes }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.finish();
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_demuxer_handles_fragmented_and_unknown_envelopes() {
        let mut input = Vec::new();
        input.extend_from_slice(&4_u32.to_be_bytes());
        input.extend_from_slice(&[2, 1, 2, 3]);
        input.extend_from_slice(&3_u32.to_be_bytes());
        input.extend_from_slice(&[99, 7, 8]);
        let mut demuxer = AvccDemuxer::default();
        assert!(demuxer.push(&input[..5]).unwrap().is_empty());
        let chunks = demuxer.push(&input[5..]).unwrap();
        assert_eq!(chunks, [AvccChunk { kind: AvccChunkKind::Keyframe, payload: vec![1, 2, 3] }]);
    }

    #[test]
    fn semu_metadata_and_annex_b_keyframes_are_preserved() {
        let mut packet = b"SEMU".to_vec();
        packet.extend_from_slice(&[1, 1, 0, 0]);
        packet.extend_from_slice(&42_u64.to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        let frame = parse_semu_packet(&packet);
        assert_eq!(frame.timestamp_us, Some(42));
        assert!(frame.keyframe);
        assert_eq!(frame.payload, [0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn jpeg_bounds_extracts_one_mjpeg_image() {
        assert_eq!(jpeg_bounds(&[1, 0xff, 0xd8, 2, 0xff, 0xd9, 3]), Some((1, 6)));
    }

    #[test]
    fn avcc_recorder_keeps_the_helper_transport_envelope() {
        let mut recorder = RawFrameRecorder::new(agent_protocol::device::DeviceRecordingFormat::Avcc, "now".into());
        recorder.push(&TransportFrame { payload: vec![1, 2], encoding: DeviceFrameEncoding::H264, keyframe: true, timestamp_us: Some(7), screen_id: None }).unwrap();
        assert_eq!(recorder.frame_count(), 1);
        assert_eq!(recorder.byte_count(), 7);
        assert_eq!(recorder.bytes(), &[0, 0, 0, 2, 2, 1, 2]);
    }

    #[test]
    fn mjpeg_recordings_are_a_bounded_multipart_stream() {
        let mut recorder = RawFrameRecorder::new(agent_protocol::device::DeviceRecordingFormat::Mjpeg, "now".into());
        recorder.push(&TransportFrame {
            payload: vec![0xff, 0xd8, 1, 0xff, 0xd9],
            encoding: DeviceFrameEncoding::Mjpeg,
            keyframe: true,
            timestamp_us: None,
            screen_id: None,
        }).unwrap();
        let body = String::from_utf8_lossy(recorder.bytes());
        assert!(body.starts_with("--remote-agent-device\r\nContent-Type: image/jpeg\r\nContent-Length: 5\r\n\r\n"));
        assert!(recorder.bytes().ends_with(b"\r\n"));
        let bytes = recorder.into_bytes();
        assert!(bytes.ends_with(b"--remote-agent-device--\r\n"));
    }

    #[test]
    fn semu_recordings_keep_the_native_header_timestamp_and_keyframe() {
        let mut recorder = RawFrameRecorder::new(agent_protocol::device::DeviceRecordingFormat::Avcc, "now".into());
        recorder.push(&TransportFrame {
            payload: vec![0, 0, 0, 1, 0x65],
            encoding: DeviceFrameEncoding::Semu,
            keyframe: true,
            timestamp_us: Some(42),
            screen_id: None,
        }).unwrap();
        assert_eq!(&recorder.bytes()[..16], b"SEMU\x01\x01\0\0\0\0\0\0\0\0\0\0\0*" );
    }

    #[test]
    fn recorder_rejects_a_frame_that_would_cross_the_host_bound() {
        let mut recorder = RawFrameRecorder::new(agent_protocol::device::DeviceRecordingFormat::RawFrames, "now".into());
        let frame = TransportFrame {
            payload: vec![0; MAX_STREAM_CHUNK - 4],
            encoding: DeviceFrameEncoding::Png,
            keyframe: true,
            timestamp_us: None,
            screen_id: None,
        };
        assert!(recorder.push(&frame).is_err());
        assert_eq!(recorder.frame_count(), 0);
        assert_eq!(recorder.byte_count(), 0);
    }
}
