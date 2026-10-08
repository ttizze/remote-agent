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
        if bytes.len() > MAX_STREAM_CHUNK
            || self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_CHUNK
        {
            return Err("device AVCC stream chunk exceeds the Host limit".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut offset = 0;
        let mut chunks = Vec::new();
        while self.buffer.len().saturating_sub(offset) >= 4 {
            let length =
                u32::from_be_bytes(self.buffer[offset..offset + 4].try_into().unwrap()) as usize;
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
}

/// Split Android's `SEMU` header from the Annex-B H.264 access unit.
pub fn parse_semu_packet(bytes: &[u8]) -> TransportFrame {
    if bytes.len() >= 16 && bytes[0..4] == *b"SEMU" && bytes[4] == 1 {
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

/// A bounded recorder for the device transport. H.264 access units are
/// finalized as one playable fragmented MP4 with an independent track for
/// each Duo screen.
#[derive(Debug, Clone)]
pub struct Mp4Recorder {
    started_at: String,
    frame_count: u64,
    byte_count: u64,
    bytes: Vec<u8>,
    finished: bool,
    video_tracks: std::collections::BTreeMap<Option<u8>, RecordedVideoTrack>,
    video_payload_bytes: usize,
    finish_error: Option<String>,
}

#[derive(Debug, Clone)]
struct RecordedVideoSample {
    payload: Vec<u8>,
    timestamp_us: Option<u64>,
    keyframe: bool,
}

#[derive(Debug, Clone, Default)]
struct RecordedVideoTrack {
    description: Option<Vec<u8>>,
    samples: Vec<RecordedVideoSample>,
}

impl Mp4Recorder {
    pub fn new(started_at: String) -> Self {
        Self {
            started_at,
            frame_count: 0,
            byte_count: 0,
            bytes: Vec::new(),
            finished: false,
            video_tracks: std::collections::BTreeMap::new(),
            video_payload_bytes: 0,
            finish_error: None,
        }
    }

    pub fn push(&mut self, frame: &TransportFrame) -> Result<(), String> {
        if self.finished {
            return Err("device recording has already been finalized".into());
        }
        if !matches!(
            frame.encoding,
            DeviceFrameEncoding::AvccDescription
                | DeviceFrameEncoding::H264
                | DeviceFrameEncoding::Semu
        ) {
            return Err("device MP4 recording received a non-H.264 frame".into());
        }
        self.append_playable_video_frame(frame)?;
        self.frame_count = self.frame_count.saturating_add(1);
        self.byte_count = self.video_payload_bytes as u64;
        Ok(())
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        match build_fragmented_mp4(&self.video_tracks) {
            Ok(bytes) => {
                self.byte_count = bytes.len() as u64;
                self.bytes = bytes;
            }
            Err(error) => self.finish_error = Some(error),
        }
    }

    pub fn started_at(&self) -> &str {
        &self.started_at
    }
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }
    pub fn finish_error(&self) -> Option<&str> {
        self.finish_error.as_deref()
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.finish();
        self.bytes
    }

    fn append_playable_video_frame(&mut self, frame: &TransportFrame) -> Result<(), String> {
        match frame.encoding {
            DeviceFrameEncoding::AvccDescription => {
                if frame.payload.len() > MAX_STREAM_CHUNK {
                    return Err("device recording codec description exceeded the Host limit".into());
                }
                let track = self.video_tracks.entry(frame.screen_id).or_default();
                if let Some(description) = &track.description {
                    if description != &frame.payload {
                        return Err(
                            "device recording codec description changed during capture".into()
                        );
                    }
                } else {
                    track.description = Some(frame.payload.clone());
                }
            }
            DeviceFrameEncoding::Jpeg => {
                return Err("device MP4 recording received a JPEG seed".into());
            }
            DeviceFrameEncoding::H264 | DeviceFrameEncoding::Semu => {
                let keyframe = frame.keyframe || annex_b_has_keyframe(&frame.payload);
                let mut payload = length_prefixed_h264(&frame.payload)?;
                if payload.is_empty() {
                    return Err("device recording received an empty H.264 frame".into());
                }
                let track = self.video_tracks.entry(frame.screen_id).or_default();
                if track.samples.is_empty()
                    && !frame.keyframe
                    && !annex_b_has_keyframe(&frame.payload)
                {
                    return Err("device recording started with a delta frame".into());
                }
                if let Some(metadata) = frame_metadata(frame) {
                    let sei = h264_user_data_sei(metadata.as_bytes());
                    let length = u32::try_from(sei.len())
                        .map_err(|_| "device recording metadata is too large")?;
                    let mut prefixed = Vec::with_capacity(sei.len().saturating_add(4));
                    prefixed.extend_from_slice(&length.to_be_bytes());
                    prefixed.extend_from_slice(&sei);
                    payload.splice(0..0, prefixed);
                }
                if self.video_payload_bytes.saturating_add(payload.len())
                    > MAX_STREAM_CHUNK.saturating_sub(4096)
                {
                    return Err("device recording exceeded the Host byte limit".into());
                }
                self.video_payload_bytes = self.video_payload_bytes.saturating_add(payload.len());
                track.samples.push(RecordedVideoSample {
                    payload,
                    timestamp_us: frame.timestamp_us,
                    keyframe,
                });
            }
            DeviceFrameEncoding::Mjpeg | DeviceFrameEncoding::Png => unreachable!(),
        }
        self.byte_count = self.video_payload_bytes as u64;
        Ok(())
    }
}

fn frame_metadata(frame: &TransportFrame) -> Option<String> {
    (frame.timestamp_us.is_some() || frame.screen_id.is_some()).then(|| {
        format!(
            "remote-agent-device timestamp={} screen={}",
            frame
                .timestamp_us
                .map_or_else(|| "none".into(), |timestamp| timestamp.to_string()),
            frame
                .screen_id
                .map_or_else(|| "none".into(), |screen| screen.to_string())
        )
    })
}

fn h264_user_data_sei(metadata: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x06];
    let mut write_size = |mut size: usize| {
        while size >= 255 {
            bytes.push(255);
            size -= 255;
        }
        bytes.push(size as u8);
    };
    write_size(5);
    write_size(16 + metadata.len());
    bytes.extend_from_slice(b"remote-agent-sei");
    bytes.extend_from_slice(metadata);
    bytes.push(0x80);
    bytes
}

fn length_prefixed_h264(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let nals = split_annex_b(bytes)
        .or_else(|| split_length_prefixed(bytes))
        .unwrap_or_else(|| {
            if bytes.is_empty() {
                Vec::new()
            } else {
                vec![bytes.to_vec()]
            }
        });
    if nals.iter().any(Vec::is_empty) {
        return Err("device recording contained an empty H.264 NAL unit".into());
    }
    let mut result = Vec::with_capacity(bytes.len().saturating_add(nals.len().saturating_mul(4)));
    for nal in nals {
        let length = u32::try_from(nal.len()).map_err(|_| "device H.264 NAL is too large")?;
        result.extend_from_slice(&length.to_be_bytes());
        result.extend_from_slice(&nal);
    }
    Ok(result)
}

fn split_annex_b(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let length = if bytes[index..].starts_with(&[0, 0, 1]) {
            Some(3)
        } else if index + 4 <= bytes.len() && bytes[index..].starts_with(&[0, 0, 0, 1]) {
            Some(4)
        } else {
            None
        };
        if let Some(length) = length {
            starts.push((index, index + length));
            index += length;
        } else {
            index += 1;
        }
    }
    if starts.is_empty() {
        return None;
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (position, (_, start)) in starts.iter().enumerate() {
        let mut end = starts
            .get(position + 1)
            .map_or(bytes.len(), |(next, _)| *next);
        while end > *start && bytes[end - 1] == 0 {
            end -= 1;
        }
        if *start < end {
            nals.push(bytes[*start..end].to_vec());
        }
    }
    Some(nals)
}

fn split_length_prefixed(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut nals = Vec::new();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        offset = offset.checked_add(4)?;
        let end = offset.checked_add(length)?;
        if length == 0 || end > bytes.len() {
            return None;
        }
        nals.push(bytes[offset..end].to_vec());
        offset = end;
    }
    (offset == bytes.len() && !nals.is_empty()).then_some(nals)
}

fn avcc_description_from_samples(samples: &[RecordedVideoSample]) -> Option<Vec<u8>> {
    let mut sps = None;
    let mut pps = None;
    for sample in samples {
        let Some(nals) = split_length_prefixed(&sample.payload) else {
            continue;
        };
        for nal in nals {
            match nal.first().map(|value| value & 0x1f) {
                Some(7) if sps.is_none() => sps = Some(nal),
                Some(8) if pps.is_none() => pps = Some(nal),
                _ => {}
            }
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }
    let sps = sps?;
    let pps = pps?;
    let profile = *sps.get(1).unwrap_or(&0x42);
    let compatibility = *sps.get(2).unwrap_or(&0);
    let level = *sps.get(3).unwrap_or(&0x1e);
    let mut description = vec![1, profile, compatibility, level, 0xff, 0xe1];
    description.extend_from_slice(&u16::try_from(sps.len()).ok()?.to_be_bytes());
    description.extend_from_slice(&sps);
    description.push(1);
    description.extend_from_slice(&u16::try_from(pps.len()).ok()?.to_be_bytes());
    description.extend_from_slice(&pps);
    Some(description)
}

fn avcc_sps(description: &[u8]) -> Option<Vec<u8>> {
    if description.len() < 7 || description[0] != 1 {
        return None;
    }
    let mut offset = 6;
    let sps_count = (description[5] & 0x1f) as usize;
    if sps_count == 0 {
        return None;
    }
    let length = u16::from_be_bytes(description.get(offset..offset + 2)?.try_into().ok()?) as usize;
    offset += 2;
    Some(description.get(offset..offset + length)?.to_vec())
}

pub(crate) fn avcc_dimensions(description: &[u8]) -> Option<(u32, u32)> {
    avcc_sps(description).and_then(|sps| sps_dimensions(&sps))
}

pub(crate) fn h264_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let nals = split_annex_b(bytes).unwrap_or_else(|| vec![bytes.to_vec()]);
    nals.into_iter()
        .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == 7))
        .and_then(|sps| sps_dimensions(&sps))
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>, String> {
    let size = u32::try_from(payload.len().saturating_add(8))
        .map_err(|_| "device recording MP4 box is too large")?;
    let mut result = Vec::with_capacity(size as usize);
    result.extend_from_slice(&size.to_be_bytes());
    result.extend_from_slice(kind);
    result.extend_from_slice(payload);
    Ok(result)
}

fn mp4_full_box(
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    let mut full = Vec::with_capacity(payload.len().saturating_add(4));
    full.push(version);
    let flag_bytes = (flags & 0x00ff_ffff).to_be_bytes();
    full.extend_from_slice(&flag_bytes[1..]);
    full.extend_from_slice(payload);
    mp4_box(kind, &full)
}

fn mp4_u16(value: u16, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn mp4_u32(value: u32, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn mp4_u64(value: u64, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn mp4_fixed(value: u32, output: &mut Vec<u8>) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn mp4_matrix(output: &mut Vec<u8>) {
    for value in [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        mp4_fixed(value, output);
    }
}

fn sample_durations(samples: &[RecordedVideoSample]) -> Vec<u32> {
    let default_duration = 16_667_u64;
    let fallback = samples
        .windows(2)
        .filter_map(|pair| pair[1].timestamp_us.zip(pair[0].timestamp_us))
        .map(|(next, current)| next.saturating_sub(current))
        .find(|duration| *duration != 0)
        .unwrap_or(default_duration)
        .clamp(1, u32::MAX as u64);
    samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            sample
                .timestamp_us
                .zip(samples.get(index + 1).and_then(|next| next.timestamp_us))
                .map(|(current, next)| {
                    next.saturating_sub(current).clamp(1, u32::MAX as u64) as u32
                })
                .unwrap_or(fallback as u32)
        })
        .collect()
}

#[derive(Debug, Clone)]
struct PreparedVideoTrack {
    id: u32,
    description: Vec<u8>,
    samples: Vec<RecordedVideoSample>,
    width: u32,
    height: u32,
}

fn build_fragmented_mp4(
    tracks: &std::collections::BTreeMap<Option<u8>, RecordedVideoTrack>,
) -> Result<Vec<u8>, String> {
    let mut prepared = Vec::with_capacity(tracks.len());
    for track in tracks.values() {
        if track.samples.is_empty() {
            continue;
        }
        if !track.samples[0].keyframe {
            return Err("device recording started with a delta frame".into());
        }
        let description = if let Some(description) = &track.description {
            description.clone()
        } else {
            avcc_description_from_samples(&track.samples).ok_or_else(|| {
                "device recording did not provide an H.264 codec description".to_owned()
            })?
        };
        let sps = avcc_sps(&description)
            .ok_or_else(|| "device recording codec description was invalid".to_owned())?;
        let (width, height) = sps_dimensions(&sps).ok_or_else(|| {
            "device recording codec description had invalid dimensions".to_owned()
        })?;
        prepared.push(PreparedVideoTrack {
            id: u32::try_from(prepared.len().saturating_add(1))
                .map_err(|_| "too many device recording screens")?,
            description,
            samples: track.samples.clone(),
            width,
            height,
        });
    }
    if prepared.is_empty() {
        return Err("device recording ended before an H.264 frame arrived".into());
    }
    let mut media_data = Vec::new();
    let mut media_offsets = Vec::with_capacity(prepared.len());
    for track in &prepared {
        media_offsets.push(media_data.len());
        for sample in &track.samples {
            media_data.extend_from_slice(&sample.payload);
        }
    }
    let ftyp = mp4_box(
        b"ftyp",
        &[
            b"isom".as_slice(),
            &0x0000_0200_u32.to_be_bytes(),
            b"isom".as_slice(),
            b"iso6".as_slice(),
            b"mp41".as_slice(),
            b"avc1".as_slice(),
        ]
        .concat(),
    )?;
    let moov = build_mp4_moov(&prepared)?;
    let moof = build_mp4_moof(&prepared, &media_offsets)?;
    let mdat = mp4_box(b"mdat", &media_data)?;
    let mut output = Vec::with_capacity(
        ftyp.len()
            .saturating_add(moov.len())
            .saturating_add(moof.len())
            .saturating_add(mdat.len()),
    );
    output.extend_from_slice(&ftyp);
    output.extend_from_slice(&moov);
    output.extend_from_slice(&moof);
    output.extend_from_slice(&mdat);
    if output.len() > MAX_STREAM_CHUNK {
        return Err("device recording exceeded the Host limit after MP4 finalization".into());
    }
    Ok(output)
}

fn build_mp4_moov(tracks: &[PreparedVideoTrack]) -> Result<Vec<u8>, String> {
    let mut mvhd_payload = Vec::new();
    mp4_u32(0, &mut mvhd_payload);
    mp4_u32(0, &mut mvhd_payload);
    mp4_u32(1_000_000, &mut mvhd_payload);
    mp4_u32(0, &mut mvhd_payload);
    mp4_fixed(0x0001_0000, &mut mvhd_payload);
    mp4_u16(0x0100, &mut mvhd_payload);
    mp4_u16(0, &mut mvhd_payload);
    mvhd_payload.extend_from_slice(&[0; 8]);
    mp4_matrix(&mut mvhd_payload);
    mvhd_payload.extend_from_slice(&[0; 24]);
    mp4_u32(
        u32::try_from(tracks.len().saturating_add(1))
            .map_err(|_| "too many device recording tracks")?,
        &mut mvhd_payload,
    );
    let mvhd = mp4_full_box(b"mvhd", 0, 0, &mvhd_payload)?;

    let mut tracks_payload = Vec::new();
    let mut trex_payload = Vec::new();
    for track in tracks {
        tracks_payload.extend_from_slice(&build_mp4_trak(track)?);
        trex_payload.extend_from_slice(&build_mp4_trex(track)?);
    }
    let mvex = mp4_box(b"mvex", &trex_payload)?;
    tracks_payload.extend_from_slice(&mvex);
    mp4_box(b"moov", &[mvhd, tracks_payload].concat())
}

fn build_mp4_trak(track: &PreparedVideoTrack) -> Result<Vec<u8>, String> {
    let mut tkhd_payload = Vec::new();
    mp4_u32(0, &mut tkhd_payload);
    mp4_u32(0, &mut tkhd_payload);
    mp4_u32(track.id, &mut tkhd_payload);
    mp4_u32(0, &mut tkhd_payload);
    mp4_u32(0, &mut tkhd_payload);
    tkhd_payload.extend_from_slice(&[0; 8]);
    mp4_u16(0, &mut tkhd_payload);
    mp4_u16(0, &mut tkhd_payload);
    mp4_u16(0, &mut tkhd_payload);
    mp4_u16(0, &mut tkhd_payload);
    mp4_matrix(&mut tkhd_payload);
    mp4_fixed(track.width.saturating_mul(1 << 16), &mut tkhd_payload);
    mp4_fixed(track.height.saturating_mul(1 << 16), &mut tkhd_payload);
    let tkhd = mp4_full_box(b"tkhd", 0, 7, &tkhd_payload)?;

    let mut mdhd_payload = Vec::new();
    mp4_u32(0, &mut mdhd_payload);
    mp4_u32(0, &mut mdhd_payload);
    mp4_u32(1_000_000, &mut mdhd_payload);
    mp4_u32(0, &mut mdhd_payload);
    mp4_u16(0x55c4, &mut mdhd_payload);
    mp4_u16(0, &mut mdhd_payload);
    let mdhd = mp4_full_box(b"mdhd", 0, 0, &mdhd_payload)?;

    let mut hdlr_payload = Vec::new();
    mp4_u32(0, &mut hdlr_payload);
    hdlr_payload.extend_from_slice(b"vide");
    hdlr_payload.extend_from_slice(&[0; 12]);
    hdlr_payload.extend_from_slice(b"Device video\0");
    let hdlr = mp4_full_box(b"hdlr", 0, 0, &hdlr_payload)?;

    let mut vmhd_payload = Vec::new();
    mp4_u16(0, &mut vmhd_payload);
    mp4_u16(0, &mut vmhd_payload);
    mp4_u16(0, &mut vmhd_payload);
    mp4_u16(0, &mut vmhd_payload);
    let vmhd = mp4_full_box(b"vmhd", 0, 1, &vmhd_payload)?;

    let url = mp4_full_box(b"url ", 0, 1, &[])?;
    let mut dref_payload = Vec::new();
    mp4_u32(1, &mut dref_payload);
    dref_payload.extend_from_slice(&url);
    let dref = mp4_full_box(b"dref", 0, 0, &dref_payload)?;
    let dinf = mp4_box(b"dinf", &dref)?;

    let avcc = mp4_box(b"avcC", &track.description)?;
    let mut avc1_payload = Vec::new();
    avc1_payload.extend_from_slice(&[0; 6]);
    mp4_u16(1, &mut avc1_payload);
    avc1_payload.extend_from_slice(&[0; 16]);
    mp4_u16(
        u16::try_from(track.width).map_err(|_| "device recording width is too large")?,
        &mut avc1_payload,
    );
    mp4_u16(
        u16::try_from(track.height).map_err(|_| "device recording height is too large")?,
        &mut avc1_payload,
    );
    mp4_fixed(0x0048_0000, &mut avc1_payload);
    mp4_fixed(0x0048_0000, &mut avc1_payload);
    mp4_u32(0, &mut avc1_payload);
    mp4_u16(1, &mut avc1_payload);
    avc1_payload.extend_from_slice(&[0; 32]);
    mp4_u16(0x0018, &mut avc1_payload);
    mp4_u16(0xffff, &mut avc1_payload);
    avc1_payload.extend_from_slice(&avcc);
    let avc1 = mp4_box(b"avc1", &avc1_payload)?;
    let mut stsd_payload = Vec::new();
    mp4_u32(1, &mut stsd_payload);
    stsd_payload.extend_from_slice(&avc1);
    let stsd = mp4_full_box(b"stsd", 0, 0, &stsd_payload)?;
    let stts = mp4_full_box(b"stts", 0, 0, &0_u32.to_be_bytes())?;
    let stsc = mp4_full_box(b"stsc", 0, 0, &0_u32.to_be_bytes())?;
    let mut stsz_payload = Vec::new();
    mp4_u32(0, &mut stsz_payload);
    mp4_u32(0, &mut stsz_payload);
    let stsz = mp4_full_box(b"stsz", 0, 0, &stsz_payload)?;
    let stco = mp4_full_box(b"stco", 0, 0, &0_u32.to_be_bytes())?;
    let stbl = mp4_box(b"stbl", &[stsd, stts, stsc, stsz, stco].concat())?;
    let minf = mp4_box(b"minf", &[vmhd, dinf, stbl].concat())?;
    let mdia = mp4_box(b"mdia", &[mdhd, hdlr, minf].concat())?;
    mp4_box(b"trak", &[tkhd, mdia].concat())
}

fn build_mp4_trex(track: &PreparedVideoTrack) -> Result<Vec<u8>, String> {
    let mut trex_payload = Vec::new();
    mp4_u32(track.id, &mut trex_payload);
    mp4_u32(1, &mut trex_payload);
    mp4_u32(16_667, &mut trex_payload);
    mp4_u32(0, &mut trex_payload);
    mp4_u32(0x0101_0000, &mut trex_payload);
    mp4_full_box(b"trex", 0, 0, &trex_payload)
}

fn build_mp4_moof(
    tracks: &[PreparedVideoTrack],
    media_offsets: &[usize],
) -> Result<Vec<u8>, String> {
    let mut mfhd_payload = Vec::new();
    mp4_u32(1, &mut mfhd_payload);
    let mfhd = mp4_full_box(b"mfhd", 0, 0, &mfhd_payload)?;

    let make_moof = |data_offsets: &[i32]| -> Result<Vec<u8>, String> {
        let mut trafs = Vec::new();
        for (track, data_offset) in tracks.iter().zip(data_offsets) {
            let mut tfhd_payload = Vec::new();
            mp4_u32(track.id, &mut tfhd_payload);
            let tfhd = mp4_full_box(b"tfhd", 0, 0x020000, &tfhd_payload)?;
            let mut tfdt_payload = Vec::new();
            mp4_u64(0, &mut tfdt_payload);
            let tfdt = mp4_full_box(b"tfdt", 1, 0, &tfdt_payload)?;
            let durations = sample_durations(&track.samples);
            let mut trun_payload = Vec::new();
            mp4_u32(
                u32::try_from(track.samples.len())
                    .map_err(|_| "too many device recording frames")?,
                &mut trun_payload,
            );
            trun_payload.extend_from_slice(&data_offset.to_be_bytes());
            for (duration, sample) in durations.iter().zip(&track.samples) {
                mp4_u32(*duration, &mut trun_payload);
                mp4_u32(
                    u32::try_from(sample.payload.len())
                        .map_err(|_| "device recording frame is too large")?,
                    &mut trun_payload,
                );
                mp4_u32(
                    if sample.keyframe {
                        0x0200_0000
                    } else {
                        0x0101_0000
                    },
                    &mut trun_payload,
                );
            }
            let trun = mp4_full_box(b"trun", 0, 0x000701, &trun_payload)?;
            trafs.extend_from_slice(&mp4_box(b"traf", &[tfhd, tfdt, trun].concat())?);
        }
        mp4_box(b"moof", &[mfhd.clone(), trafs].concat())
    };
    let initial_offsets = vec![0; tracks.len()];
    let initial = make_moof(&initial_offsets)?;
    let base = initial.len().saturating_add(8);
    let data_offsets = media_offsets
        .iter()
        .map(|offset| {
            i32::try_from(base.saturating_add(*offset))
                .map_err(|_| "device recording MP4 offset is too large")
        })
        .collect::<Result<Vec<_>, _>>()?;
    make_moof(&data_offsets)
}

fn sps_dimensions(sps: &[u8]) -> Option<(u32, u32)> {
    if sps.len() < 4 || sps[0] & 0x1f != 7 {
        return None;
    }
    let rbsp = remove_emulation_prevention(&sps[1..]);
    let mut bits = BitReader::new(&rbsp);
    let profile = bits.read_bits(8)?;
    let _constraints = bits.read_bits(8)?;
    let _level = bits.read_bits(8)?;
    let _sps_id = bits.read_ue()?;
    let mut chroma_format = 1_u32;
    let mut separate_colour_plane = false;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    ) {
        chroma_format = bits.read_ue()?;
        if chroma_format == 3 {
            separate_colour_plane = bits.read_bit()?;
        }
        let _bit_depth_luma = bits.read_ue()?;
        let _bit_depth_chroma = bits.read_ue()?;
        let _qpprime = bits.read_bit()?;
        if bits.read_bit()? {
            let count = if chroma_format != 3 { 8 } else { 12 };
            for index in 0..count {
                if bits.read_bit()? {
                    skip_scaling_list(&mut bits, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let _log2_frame_num = bits.read_ue()?;
    let pic_order_type = bits.read_ue()?;
    if pic_order_type == 0 {
        let _ = bits.read_ue()?;
    } else if pic_order_type == 1 {
        let _ = bits.read_bit()?;
        let _ = bits.read_se()?;
        let _ = bits.read_se()?;
        let count = bits.read_ue()?;
        for _ in 0..count {
            let _ = bits.read_se()?;
        }
    }
    let _refs = bits.read_ue()?;
    let _gaps = bits.read_bit()?;
    let width_mbs = bits.read_ue()?.saturating_add(1);
    let height_map_units = bits.read_ue()?.saturating_add(1);
    let frame_mbs_only = bits.read_bit()?;
    if !frame_mbs_only {
        let _ = bits.read_bit()?;
    }
    let _direct_8x8 = bits.read_bit()?;
    let crop = bits.read_bit()?;
    let (crop_left, crop_right, crop_top, crop_bottom) = if crop {
        (
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
        )
    } else {
        (0, 0, 0, 0)
    };
    let chroma_array = if separate_colour_plane {
        0
    } else {
        chroma_format
    };
    let crop_unit_x = if matches!(chroma_array, 0 | 3) { 1 } else { 2 };
    let crop_unit_y = if chroma_array == 0 {
        2 - u32::from(frame_mbs_only)
    } else if chroma_array == 1 {
        4 - 2 * u32::from(frame_mbs_only)
    } else {
        2 - u32::from(frame_mbs_only)
    };
    let width = width_mbs
        .saturating_mul(16)
        .saturating_sub((crop_left.saturating_add(crop_right)).saturating_mul(crop_unit_x));
    let height = height_map_units
        .saturating_mul(16)
        .saturating_mul(2 - u32::from(frame_mbs_only))
        .saturating_sub((crop_top.saturating_add(crop_bottom)).saturating_mul(crop_unit_y));
    (width > 0 && height > 0).then_some((width, height))
}

fn skip_scaling_list(bits: &mut BitReader<'_>, size: usize) -> Option<()> {
    let mut last = 8_i32;
    let mut next = 8_i32;
    for _ in 0..size {
        if next != 0 {
            let delta = bits.read_se()?;
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Some(())
}

fn remove_emulation_prevention(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    for &byte in bytes {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        output.push(byte);
        if byte == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    output
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit: 0 }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let byte = *self.bytes.get(self.bit / 8)?;
        let value = byte & (0x80 >> (self.bit % 8)) != 0;
        self.bit += 1;
        Some(value)
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeroes = 0;
        while !self.read_bit()? {
            leading_zeroes += 1;
            if leading_zeroes > 31 {
                return None;
            }
        }
        let suffix = self.read_bits(leading_zeroes)?;
        Some(
            (1_u32 << leading_zeroes)
                .saturating_sub(1)
                .saturating_add(suffix),
        )
    }

    fn read_se(&mut self) -> Option<i32> {
        let value = self.read_ue()? as i32;
        Some(if value & 1 == 0 {
            -(value / 2)
        } else {
            (value + 1) / 2
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENH264_STATIC: &[u8] = include_bytes!("fixtures/openh264/Static.264");

    fn fixture_nal(nal_type: u8) -> Vec<u8> {
        split_annex_b(OPENH264_STATIC)
            .expect("OpenH264 fixture must contain Annex-B NAL units")
            .into_iter()
            .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == nal_type))
            .unwrap_or_else(|| panic!("OpenH264 fixture is missing NAL type {nal_type}"))
    }

    fn annex_b(nal: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0, 0, 0, 1];
        bytes.extend_from_slice(nal);
        bytes
    }

    fn valid_description() -> Vec<u8> {
        let sps = fixture_nal(7);
        let pps = fixture_nal(8);
        let mut description = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
        description.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        description.extend_from_slice(&sps);
        description.push(1);
        description.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        description.extend_from_slice(&pps);
        description
    }

    fn keyframe() -> Vec<u8> {
        annex_b(&fixture_nal(5))
    }

    fn delta() -> Vec<u8> {
        annex_b(&fixture_nal(1))
    }

    #[derive(Debug)]
    struct ParsedMp4Box<'a> {
        kind: [u8; 4],
        start: usize,
        size: usize,
        payload: &'a [u8],
    }

    fn parse_mp4(bytes: &[u8]) -> Result<Vec<ParsedMp4Box<'_>>, String> {
        let mut boxes = Vec::new();
        walk_mp4_boxes(bytes, 0, &mut boxes)?;
        Ok(boxes)
    }

    fn walk_mp4_boxes<'a>(
        bytes: &'a [u8],
        base: usize,
        output: &mut Vec<ParsedMp4Box<'a>>,
    ) -> Result<(), String> {
        let mut offset = 0;
        while offset < bytes.len() {
            if bytes.len().saturating_sub(offset) < 8 {
                return Err("MP4 parser found a truncated box header".into());
            }
            let size = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "MP4 box size is invalid")?,
            ) as usize;
            if size < 8 || size > bytes.len().saturating_sub(offset) {
                return Err("MP4 parser found an invalid box size".into());
            }
            let kind = bytes[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| "MP4 box type is invalid")?;
            let payload = &bytes[offset + 8..offset + size];
            output.push(ParsedMp4Box {
                kind,
                start: base + offset,
                size,
                payload,
            });
            if let Some(child_offset) = mp4_child_offset(&kind, payload)? {
                walk_mp4_boxes(
                    &payload[child_offset..],
                    base + offset + 8 + child_offset,
                    output,
                )?;
            }
            offset += size;
        }
        Ok(())
    }

    fn mp4_child_offset(kind: &[u8; 4], payload: &[u8]) -> Result<Option<usize>, String> {
        let offset = match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"dinf" | b"stbl" | b"mvex" | b"moof"
            | b"traf" => 0,
            b"stsd" | b"dref" => 8,
            b"avc1" => 78,
            _ => return Ok(None),
        };
        if payload.len() < offset {
            return Err(format!(
                "MP4 {kind:?} payload is shorter than its child header"
            ));
        }
        Ok(Some(offset))
    }

    fn count_boxes(boxes: &[ParsedMp4Box<'_>], wanted: &[u8; 4]) -> usize {
        boxes.iter().filter(|item| &item.kind == wanted).count()
    }

    fn read_u32(bytes: &[u8]) -> Result<u32, String> {
        bytes
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| "MP4 field is truncated".into())
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ParsedSample {
        duration: u32,
        size: u32,
        flags: u32,
    }

    fn parse_trun(item: &ParsedMp4Box<'_>) -> Result<(i32, Vec<ParsedSample>), String> {
        if item.kind != *b"trun" || item.payload.len() < 12 {
            return Err("MP4 trun is truncated".into());
        }
        let flags = u32::from_be_bytes([0, item.payload[1], item.payload[2], item.payload[3]]);
        if flags != 0x000701 {
            return Err(format!(
                "MP4 trun flags are {flags:#x}, expected sample offsets and fields"
            ));
        }
        let count = read_u32(&item.payload[4..8])? as usize;
        let data_offset = i32::from_be_bytes(
            item.payload[8..12]
                .try_into()
                .map_err(|_| "MP4 trun offset is invalid")?,
        );
        let expected = 12_usize.saturating_add(count.saturating_mul(12));
        if item.payload.len() != expected {
            return Err("MP4 trun sample table has an invalid length".into());
        }
        let mut samples = Vec::with_capacity(count);
        for chunk in item.payload[12..].as_chunks::<12>().0 {
            samples.push(ParsedSample {
                duration: read_u32(&chunk[..4])?,
                size: read_u32(&chunk[4..8])?,
                flags: read_u32(&chunk[8..12])?,
            });
        }
        Ok((data_offset, samples))
    }

    fn parse_tfhd_track_id(item: &ParsedMp4Box<'_>) -> Result<u32, String> {
        if item.kind != *b"tfhd" || item.payload.len() < 8 {
            return Err("MP4 tfhd is truncated".into());
        }
        let flags = u32::from_be_bytes([0, item.payload[1], item.payload[2], item.payload[3]]);
        if flags != 0x020000 {
            return Err("MP4 tfhd is missing default-base-is-moof".into());
        }
        read_u32(&item.payload[4..8])
    }

    fn sample_nal_types(
        mdat: &[u8],
        start: usize,
        samples: &[ParsedSample],
    ) -> Result<Vec<Vec<u8>>, String> {
        let mut cursor = start;
        let mut all_types = Vec::with_capacity(samples.len());
        for sample in samples {
            let end = cursor.saturating_add(sample.size as usize);
            if end > mdat.len() {
                return Err("MP4 trun sample extends beyond mdat".into());
            }
            let mut offset = cursor;
            let mut types = Vec::new();
            while offset < end {
                if end.saturating_sub(offset) < 4 {
                    return Err("MP4 sample has a truncated AVC length prefix".into());
                }
                let length = read_u32(&mdat[offset..offset + 4])? as usize;
                offset += 4;
                let nal_end = offset.saturating_add(length);
                if length == 0 || nal_end > end {
                    return Err("MP4 sample has an invalid AVC NAL length".into());
                }
                types.push(mdat[offset] & 0x1f);
                offset = nal_end;
            }
            if offset != end || types.is_empty() {
                return Err("MP4 sample has no complete AVC NAL units".into());
            }
            all_types.push(types);
            cursor = end;
        }
        Ok(all_types)
    }

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
        assert_eq!(
            chunks,
            [AvccChunk {
                kind: AvccChunkKind::Keyframe,
                payload: vec![1, 2, 3]
            }]
        );
    }

    #[test]
    fn openh264_fixture_contains_complete_reference_access_units() {
        assert_eq!(OPENH264_STATIC.len(), 9_125);
        let nals = split_annex_b(OPENH264_STATIC).unwrap();
        assert_eq!(nals.len(), 12);
        assert!(fixture_nal(5).len() > 1_000);
        assert!(fixture_nal(1).len() > 700);
        assert_eq!(sps_dimensions(&fixture_nal(7)), Some((152, 100)));
    }

    #[test]
    fn semu_metadata_and_annex_b_keyframes_are_preserved() {
        let mut packet = b"SEMU".to_vec();
        packet.extend_from_slice(&[1, 1, 0, 0]);
        packet.extend_from_slice(&42_u64.to_be_bytes());
        packet.extend_from_slice(&keyframe());
        let frame = parse_semu_packet(&packet);
        assert_eq!(frame.timestamp_us, Some(42));
        assert!(frame.keyframe);
        assert_eq!(frame.payload, keyframe());
    }

    #[test]
    fn jpeg_bounds_extracts_one_mjpeg_image() {
        assert_eq!(
            jpeg_bounds(&[1, 0xff, 0xd8, 2, 0xff, 0xd9, 3]),
            Some((1, 6))
        );
    }

    #[test]
    fn mp4_recording_finalizes_with_playable_track_metadata() {
        let mut recorder = Mp4Recorder::new("now".into());
        recorder
            .push(&TransportFrame {
                payload: valid_description(),
                encoding: DeviceFrameEncoding::AvccDescription,
                keyframe: true,
                timestamp_us: None,
                screen_id: None,
            })
            .unwrap();
        recorder
            .push(&TransportFrame {
                payload: keyframe(),
                encoding: DeviceFrameEncoding::H264,
                keyframe: true,
                timestamp_us: Some(42),
                screen_id: None,
            })
            .unwrap();
        recorder
            .push(&TransportFrame {
                payload: delta(),
                encoding: DeviceFrameEncoding::H264,
                keyframe: false,
                timestamp_us: Some(59_000),
                screen_id: None,
            })
            .unwrap();
        assert_eq!(recorder.frame_count(), 3);
        let bytes = recorder.into_bytes();
        assert_eq!(&bytes[4..8], b"ftyp");
        let boxes = parse_mp4(&bytes).unwrap();
        assert_eq!(count_boxes(&boxes, b"trak"), 1);
        assert_eq!(count_boxes(&boxes, b"avcC"), 1);
        assert_eq!(count_boxes(&boxes, b"traf"), 1);
        let avcc = boxes.iter().find(|item| item.kind == *b"avcC").unwrap();
        assert_eq!(avcc.payload, valid_description());
        let moof = boxes.iter().find(|item| item.kind == *b"moof").unwrap();
        let mdat = boxes.iter().find(|item| item.kind == *b"mdat").unwrap();
        let tfhd = boxes.iter().find(|item| item.kind == *b"tfhd").unwrap();
        assert_eq!(parse_tfhd_track_id(tfhd).unwrap(), 1);
        let trun = boxes.iter().find(|item| item.kind == *b"trun").unwrap();
        let (data_offset, samples) = parse_trun(trun).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].duration, 58_958);
        assert_eq!(samples[1].duration, 58_958);
        assert_eq!(samples[0].flags, 0x0200_0000);
        assert_eq!(samples[1].flags, 0x0101_0000);
        assert_eq!(moof.start as i32 + data_offset, (mdat.start + 8) as i32);
        let mdat_payload = &bytes[mdat.start + 8..mdat.start + mdat.size];
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.size as usize)
                .sum::<usize>(),
            mdat_payload.len()
        );
        assert_eq!(
            sample_nal_types(mdat_payload, 0, &samples).unwrap(),
            [vec![6, 5], vec![6, 1]]
        );
        assert_eq!(sps_dimensions(&fixture_nal(7)), Some((152, 100)));
    }

    #[test]
    fn mp4_recording_keeps_duo_screens_in_separate_tracks() {
        let mut recorder = Mp4Recorder::new("now".into());
        for screen_id in [Some(1), Some(3)] {
            recorder
                .push(&TransportFrame {
                    payload: valid_description(),
                    encoding: DeviceFrameEncoding::AvccDescription,
                    keyframe: true,
                    timestamp_us: None,
                    screen_id,
                })
                .unwrap();
            recorder
                .push(&TransportFrame {
                    payload: keyframe(),
                    encoding: DeviceFrameEncoding::H264,
                    keyframe: true,
                    timestamp_us: Some(1),
                    screen_id,
                })
                .unwrap();
        }
        let bytes = recorder.into_bytes();
        let boxes = parse_mp4(&bytes).unwrap();
        assert_eq!(count_boxes(&boxes, b"trak"), 2);
        assert_eq!(count_boxes(&boxes, b"avcC"), 2);
        assert_eq!(count_boxes(&boxes, b"traf"), 2);
        let moof = boxes.iter().find(|item| item.kind == *b"moof").unwrap();
        let mdat = boxes.iter().find(|item| item.kind == *b"mdat").unwrap();
        let track_ids = boxes
            .iter()
            .filter(|item| item.kind == *b"tfhd")
            .map(|item| parse_tfhd_track_id(item).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(track_ids, [1, 2]);
        let mdat_payload = &bytes[mdat.start + 8..mdat.start + mdat.size];
        let mut media_offset = 0;
        for trun in boxes.iter().filter(|item| item.kind == *b"trun") {
            let (data_offset, samples) = parse_trun(trun).unwrap();
            assert_eq!(samples.len(), 1);
            assert_eq!(samples[0].duration, 16_667);
            assert_eq!(samples[0].flags, 0x0200_0000);
            assert_eq!(
                moof.start as i32 + data_offset,
                (mdat.start + 8 + media_offset) as i32
            );
            assert_eq!(
                sample_nal_types(mdat_payload, media_offset, &samples).unwrap(),
                [vec![6, 5]]
            );
            media_offset += samples[0].size as usize;
        }
        assert_eq!(media_offset, mdat_payload.len());
    }

    #[test]
    fn mp4_recording_rejects_an_initial_delta_and_invalid_description() {
        let mut recorder = Mp4Recorder::new("now".into());
        recorder
            .push(&TransportFrame {
                payload: valid_description(),
                encoding: DeviceFrameEncoding::AvccDescription,
                keyframe: true,
                timestamp_us: None,
                screen_id: None,
            })
            .unwrap();
        assert!(
            recorder
                .push(&TransportFrame {
                    payload: delta(),
                    encoding: DeviceFrameEncoding::H264,
                    keyframe: false,
                    timestamp_us: None,
                    screen_id: None
                })
                .is_err()
        );

        let mut invalid = Mp4Recorder::new("now".into());
        invalid
            .push(&TransportFrame {
                payload: vec![
                    1, 0x42, 0, 0x1f, 0xff, 0xe1, 0, 1, 0x67, 0x42, 1, 0, 1, 0x68,
                ],
                encoding: DeviceFrameEncoding::AvccDescription,
                keyframe: true,
                timestamp_us: None,
                screen_id: None,
            })
            .unwrap();
        invalid
            .push(&TransportFrame {
                payload: keyframe(),
                encoding: DeviceFrameEncoding::H264,
                keyframe: true,
                timestamp_us: None,
                screen_id: None,
            })
            .unwrap();
        invalid.finish();
        assert!(invalid.finish_error().is_some());
        assert!(invalid.into_bytes().is_empty());
    }

    #[test]
    fn recorder_rejects_a_frame_that_would_cross_the_host_bound() {
        let mut recorder = Mp4Recorder::new("now".into());
        let frame = TransportFrame {
            payload: vec![0; MAX_STREAM_CHUNK],
            encoding: DeviceFrameEncoding::H264,
            keyframe: true,
            timestamp_us: None,
            screen_id: None,
        };
        assert!(recorder.push(&frame).is_err());
        assert_eq!(recorder.frame_count(), 0);
        assert_eq!(recorder.byte_count(), 0);
    }
}
