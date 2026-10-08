//! Ordered device-frame decoding for the desktop panel.
//!
//! The Host delivers complete access units. This owner keeps the access-unit
//! order and codec state until a keyframe arrives, then hands a bounded image
//! to GPUI. JPEG/PNG frames decode directly; H.264 is decoded by the packaged
//! ffmpeg runtime selected by `AGENT_FFMPEG_EXECUTABLE` (or its platform
//! default). No frame is silently replaced with a still screenshot.

use agent_core::view::device::DeviceVideoFrameView;
use host_daemon::device_stream::jpeg_bounds;
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

const MAX_H264_BYTES: usize = 8 * 1024 * 1024;
type DeviceStreamKey = (String, String, String, String, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeviceImageFormat {
    Jpeg,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DecodedDeviceImage {
    pub host_id: String,
    pub session_epoch: String,
    pub sequence: u64,
    pub device_id: String,
    pub screen_id: u8,
    pub format: DeviceImageFormat,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub(super) struct DeviceVideoDecoder {
    latest_sequence: BTreeMap<DeviceStreamKey, u64>,
    awaiting_keyframe: BTreeSet<DeviceStreamKey>,
    descriptions: BTreeMap<DeviceStreamKey, Vec<u8>>,
    access_units: BTreeMap<DeviceStreamKey, Vec<u8>>,
    error: Option<String>,
}

impl DeviceVideoDecoder {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(super) fn retain_sessions(&mut self, sessions: &BTreeSet<(String, String, String, String)>) {
        self.latest_sequence.retain(|key, _| {
            sessions.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), key.3.clone()))
        });
        self.awaiting_keyframe.retain(|key| {
            sessions.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), key.3.clone()))
        });
        self.descriptions.retain(|key, _| {
            sessions.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), key.3.clone()))
        });
        self.access_units.retain(|key, _| {
            sessions.contains(&(key.0.clone(), key.1.clone(), key.2.clone(), key.3.clone()))
        });
        if self.latest_sequence.is_empty() {
            self.error = None;
        }
    }

    pub(super) fn push(&mut self, frames: &[DeviceVideoFrameView]) -> Vec<DecodedDeviceImage> {
        self.push_with(frames, decode_h264_with_ffmpeg)
    }

    fn push_with<F>(
        &mut self,
        frames: &[DeviceVideoFrameView],
        mut decode_h264: F,
    ) -> Vec<DecodedDeviceImage>
    where
        F: FnMut(&[u8]) -> Result<Vec<u8>, String>,
    {
        let mut decoded = Vec::new();
        let mut ordered = frames.to_vec();
        ordered.sort_by_key(|frame| (frame.screen_id.unwrap_or(0), frame.sequence));
        for frame in ordered {
            let screen = frame.screen_id.unwrap_or(0);
            let stream_key = (
                frame.thread_id.clone(),
                frame.host_id.clone(),
                frame.device_id.clone(),
                frame.session_epoch.clone(),
                screen,
            );
            if self
                .latest_sequence
                .get(&stream_key)
                .is_some_and(|sequence| frame.sequence <= *sequence)
            {
                continue;
            }
            let new_stream = self
                .latest_sequence
                .insert(stream_key.clone(), frame.sequence)
                .is_none();
            match frame.encoding.as_str() {
                "jpeg" | "mjpeg" => {
                    if let Some(bytes) = jpeg_bounds(&frame.payload) {
                        self.error = None;
                        decoded.push(DecodedDeviceImage {
                            host_id: frame.host_id,
                            session_epoch: frame.session_epoch,
                            sequence: frame.sequence,
                            device_id: frame.device_id,
                            screen_id: screen,
                            format: DeviceImageFormat::Jpeg,
                            bytes: frame.payload[bytes].to_vec(),
                        });
                    } else {
                        self.error = Some("Device stream returned an invalid JPEG frame".into());
                    }
                }
                "png" => {
                    if !frame.payload.is_empty() {
                        self.error = None;
                        decoded.push(DecodedDeviceImage {
                            host_id: frame.host_id,
                            session_epoch: frame.session_epoch,
                            sequence: frame.sequence,
                            device_id: frame.device_id,
                            screen_id: screen,
                            format: DeviceImageFormat::Png,
                            bytes: frame.payload,
                        });
                    }
                }
                "avcc-description" => {
                    self.descriptions
                        .insert(stream_key.clone(), description_to_annex_b(&frame.payload));
                    self.access_units.remove(&stream_key);
                    self.awaiting_keyframe.insert(stream_key);
                }
                "h264" | "semu" => {
                    if new_stream {
                        self.awaiting_keyframe.insert(stream_key.clone());
                    }
                    if self.awaiting_keyframe.contains(&stream_key) && !frame.keyframe {
                        continue;
                    }
                    if frame.keyframe {
                        let mut stream = self.descriptions.remove(&stream_key).unwrap_or_default();
                        stream.extend_from_slice(&to_annex_b(&frame.payload));
                        self.access_units.insert(stream_key.clone(), stream);
                        self.awaiting_keyframe.remove(&stream_key);
                    } else {
                        let stream = self.access_units.entry(stream_key.clone()).or_default();
                        stream.extend_from_slice(&to_annex_b(&frame.payload));
                    }
                    let stream = self.access_units.get(&stream_key).cloned().unwrap_or_default();
                    if stream.len() > MAX_H264_BYTES {
                        self.access_units.remove(&stream_key);
                        self.awaiting_keyframe.insert(stream_key);
                        self.error = Some("Device video decoder dropped an oversized access-unit buffer".into());
                        continue;
                    }
                    match decode_h264(&stream) {
                        Ok(bytes) => {
                            if let Some(bounds) = jpeg_bounds(&bytes) {
                                self.error = None;
                                decoded.push(DecodedDeviceImage {
                                    host_id: frame.host_id,
                                    session_epoch: frame.session_epoch,
                                    sequence: frame.sequence,
                                    device_id: frame.device_id,
                                    screen_id: screen,
                                    format: DeviceImageFormat::Jpeg,
                                    bytes: bytes[bounds].to_vec(),
                                });
                            } else {
                                self.error = Some("Device video decoder returned an invalid image".into());
                            }
                        }
                        Err(error) => {
                            self.access_units.remove(&stream_key);
                            self.awaiting_keyframe.insert(stream_key);
                            self.error = Some(error);
                        }
                    }
                }
                _ => self.error = Some(format!("Device stream encoding is unsupported: {}", frame.encoding)),
            }
        }
        decoded
    }
}

fn to_annex_b(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes.to_vec();
    }
    let mut result = Vec::with_capacity(bytes.len().saturating_add(16));
    let mut offset = 0;
    while offset.saturating_add(4) <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if length == 0 || offset.saturating_add(length) > bytes.len() {
            return prefixed_annex_b(bytes);
        }
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(&bytes[offset..offset + length]);
        offset += length;
    }
    if offset != bytes.len() || result.is_empty() {
        prefixed_annex_b(bytes)
    } else {
        result
    }
}

fn prefixed_annex_b(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len().saturating_add(4));
    result.extend_from_slice(&[0, 0, 0, 1]);
    result.extend_from_slice(bytes);
    result
}

fn description_to_annex_b(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return bytes.to_vec();
    }
    if bytes.len() < 7 || bytes[0] != 1 {
        return Vec::new();
    }
    let mut offset = 5;
    let sps_count = (bytes[offset] & 0x1f) as usize;
    offset += 1;
    let mut result = Vec::new();
    for _ in 0..sps_count {
        let Some(length_bytes) = bytes.get(offset..offset + 2) else { return Vec::new() };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(sps) = bytes.get(offset..offset + length) else { return Vec::new() };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(sps);
        offset += length;
    }
    let Some(&pps_count) = bytes.get(offset) else { return result };
    offset += 1;
    for _ in 0..pps_count as usize {
        let Some(length_bytes) = bytes.get(offset..offset + 2) else { return Vec::new() };
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        offset += 2;
        let Some(pps) = bytes.get(offset..offset + length) else { return Vec::new() };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(pps);
        offset += length;
    }
    result
}

fn decode_h264_with_ffmpeg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let executable = std::env::var_os("AGENT_FFMPEG_EXECUTABLE").unwrap_or_else(|| "ffmpeg".into());
    let mut child = Command::new(executable)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-dn",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Device video decoder runtime is unavailable".to_owned())?;
    if let Some(mut stdin) = child.stdin.take() {
        std::io::Write::write_all(&mut stdin, bytes)
            .map_err(|_| "Device video decoder could not feed the access unit".to_owned())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "Device video decoder did not finish".to_owned())?;
    if !output.status.success() {
        return Err("Device video decoder rejected the H.264 stream".into());
    }
    if output.stdout.len() > MAX_H264_BYTES {
        return Err("Device video decoder returned an oversized image stream".into());
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u64, encoding: &str, payload: Vec<u8>, keyframe: bool) -> DeviceVideoFrameView {
        DeviceVideoFrameView {
            thread_id: "thread".into(),
            host_id: "host".into(),
            device_id: "device".into(),
            platform: "android".into(),
            payload,
            encoding: encoding.into(),
            width: 2,
            height: 2,
            sequence,
            timestamp_us: Some(sequence),
            keyframe,
            screen_id: Some(1),
            session_epoch: "session".into(),
        }
    }

    #[test]
    fn ordered_decoder_waits_for_a_keyframe_and_keeps_deltas() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = Vec::new();
        let image = decoder.push_with(
            &[
                frame(2, "h264", vec![0, 0, 1, 0x41, 2], false),
                frame(1, "h264", vec![0, 0, 1, 0x65, 1], true),
                frame(3, "h264", vec![0, 0, 1, 0x41, 3], false),
            ],
            |bytes| {
                calls.push(bytes.to_vec());
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert_eq!(image.last().map(|image| image.sequence), Some(3));
        assert_eq!(calls.len(), 2);
        assert!(calls[1].windows(2).any(|window| window == [0x41, 3]));
    }

    #[test]
    fn decoder_accepts_sequence_restart_after_session_epoch_changes() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = Vec::new();
        let mut first = frame(9, "h264", vec![0, 0, 1, 0x65], true);
        let image = decoder.push_with(&[first.clone()], |bytes| {
            calls.push(bytes.to_vec());
            Ok(vec![0xff, 0xd8, 0xff, 0xd9])
        });
        assert_eq!(image.len(), 1);
        first.session_epoch = "next-session".into();
        first.sequence = 1;
        let image = decoder.push_with(&[first], |bytes| {
            calls.push(bytes.to_vec());
            Ok(vec![0xff, 0xd8, 0xff, 0xd9])
        });
        assert_eq!(image.len(), 1);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn retaining_active_sessions_drops_closed_stream_state() {
        let mut decoder = DeviceVideoDecoder::default();
        let first = frame(9, "h264", vec![0, 0, 1, 0x65], true);
        let _ = decoder.push_with(&[first], |_| Ok(vec![0xff, 0xd8, 0xff, 0xd9]));
        assert_eq!(decoder.latest_sequence.len(), 1);
        decoder.retain_sessions(&BTreeSet::new());
        assert!(decoder.latest_sequence.is_empty());
        assert!(decoder.awaiting_keyframe.is_empty());
        assert!(decoder.descriptions.is_empty());
        assert!(decoder.access_units.is_empty());
    }

    #[test]
    fn new_stream_deltas_are_ignored_until_a_keyframe() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut calls = 0;
        let images = decoder.push_with(
            &[frame(1, "h264", vec![0, 0, 1, 0x41], false)],
            |_| {
                calls += 1;
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert!(images.is_empty());
        assert_eq!(calls, 0);
        let images = decoder.push_with(
            &[frame(2, "h264", vec![0, 0, 1, 0x65], true)],
            |_| {
                calls += 1;
                Ok(vec![0xff, 0xd8, 1, 0xff, 0xd9])
            },
        );
        assert_eq!(images.len(), 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn avcc_description_becomes_annex_b_parameter_sets() {
        let description = [1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 2, 0x67, 1, 1, 0, 2, 0x68, 2];
        assert_eq!(description_to_annex_b(&description), vec![
            0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2,
        ]);
    }

    #[test]
    fn oversized_stream_resets_until_next_keyframe() {
        let mut decoder = DeviceVideoDecoder::default();
        let huge = vec![0; MAX_H264_BYTES];
        let frame = frame(1, "h264", huge, true);
        assert!(decoder.push_with(&[frame], |_| Ok(Vec::new())).is_empty());
        assert!(decoder.awaiting_keyframe.contains(&(
            "thread".into(),
            "host".into(),
            "device".into(),
            1,
        )));
    }

    #[test]
    fn independent_devices_keep_independent_sequence_and_decoder_state() {
        let mut decoder = DeviceVideoDecoder::default();
        let mut other = frame(1, "jpeg", vec![0xff, 0xd8, 2, 0xff, 0xd9], true);
        other.host_id = "other-host".into();
        other.device_id = "other-device".into();
        let images = decoder.push_with(
            &[frame(1, "jpeg", vec![0xff, 0xd8, 1, 0xff, 0xd9], true), other],
            |_| unreachable!("JPEG streams do not invoke H.264 decoding"),
        );
        assert_eq!(images.len(), 2);
        assert!(images.iter().any(|image| image.host_id == "host"));
        assert!(images.iter().any(|image| image.host_id == "other-host"));
    }
}
