use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(target_os = "macos")]
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use std::sync::{Arc, Mutex, mpsc};

#[cfg(target_os = "macos")]
pub(crate) fn prepare_microphone() {
    // Initialize the frameworks and device metadata without requesting access or
    // creating an input stream. Capture always queries the current device again.
    if let Some(audio) = unsafe { AVMediaTypeAudio } {
        let _ = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    }
    if let Some(device) = cpal::default_host().default_input_device() {
        let _ = device.default_input_config();
    }
}

pub(crate) enum RecordingEvent {
    #[cfg(target_os = "macos")]
    RequestingPermission,
    #[cfg(target_os = "macos")]
    Preparing,
    Started,
    Level(f32),
    Finished(Result<Vec<u8>, String>),
}

pub(crate) struct Recording(mpsc::Sender<()>);
impl Recording {
    pub(crate) fn finish(&self) -> Result<(), String> {
        self.0.send(()).map_err(|_| "recording has stopped".into())
    }
}

pub(crate) fn start_recording(
    events: async_channel::Sender<RecordingEvent>,
) -> Result<Recording, String> {
    let (control, commands) = mpsc::channel();
    std::thread::Builder::new()
        .name("microphone".into())
        .spawn(move || {
            let result = capture(commands, &events);
            let _ = events.send_blocking(RecordingEvent::Finished(result));
        })
        .map_err(|error| error.to_string())?;
    Ok(Recording(control))
}

fn capture(
    commands: mpsc::Receiver<()>,
    events: &async_channel::Sender<RecordingEvent>,
) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "macos")]
    authorize_microphone(&commands, events)?;
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no microphone is available")?;
    let supported = device
        .default_input_config()
        .map_err(|error| error.to_string())?;
    let sample_rate = supported.sample_rate();
    let config = supported.config();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let (failures, failed) = mpsc::channel();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => input::<f32>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::F64 => input::<f64>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::I8 => input::<i8>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::I16 => input::<i16>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::I24 => input::<cpal::I24>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::I32 => input::<i32>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::I64 => input::<i64>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::U8 => input::<u8>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::U16 => input::<u16>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::U24 => input::<cpal::U24>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::U32 => input::<u32>(&device, config, samples.clone(), failures),
        cpal::SampleFormat::U64 => input::<u64>(&device, config, samples.clone(), failures),
        format => return Err(format!("unsupported microphone sample format: {format}")),
    }?;
    // A cancelled permission request or device setup must not start recording.
    match commands.try_recv() {
        Err(mpsc::TryRecvError::Empty) => {}
        _ => return Err("recording cancelled".into()),
    }
    stream.play().map_err(|error| error.to_string())?;
    events
        .send_blocking(RecordingEvent::Started)
        .map_err(|_| "recording cancelled")?;
    loop {
        if let Ok(error) = failed.try_recv() {
            return Err(error);
        }
        match commands.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(()) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("recording cancelled".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let level = {
            let samples = samples.lock().unwrap();
            let recent = &samples[samples.len().saturating_sub(sample_rate as usize / 20)..];
            (recent.iter().map(|sample| sample * sample).sum::<f32>() / recent.len().max(1) as f32)
                .sqrt()
        };
        events
            .send_blocking(RecordingEvent::Level(level))
            .map_err(|_| "recording cancelled")?;
    }
    drop(stream);
    if let Ok(error) = failed.try_recv() {
        return Err(error);
    }
    let samples = std::mem::take(&mut *samples.lock().unwrap());
    pcm(samples, sample_rate)
}

#[cfg(target_os = "macos")]
fn authorize_microphone(
    commands: &mpsc::Receiver<()>,
    events: &async_channel::Sender<RecordingEvent>,
) -> Result<(), String> {
    use objc2::runtime::Bool;

    let denied = "システム設定の「プライバシーとセキュリティ → マイク」で Bex のアクセスを許可してください。";
    // AVMediaTypeAudio is a framework constant and both APIs accept this media type
    // on any thread. The completion block owns its sender until Apple releases it.
    let audio = unsafe { AVMediaTypeAudio }.ok_or("audio media type unavailable")?;
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
        AVAuthorizationStatus::Authorized => return Ok(()),
        AVAuthorizationStatus::NotDetermined => {}
        _ => return Err(denied.into()),
    }
    events
        .send_blocking(RecordingEvent::RequestingPermission)
        .map_err(|_| "recording cancelled")?;
    let (permission, response) = mpsc::channel();
    let completion = block2::RcBlock::new(move |granted: Bool| {
        let _ = permission.send(granted.as_bool());
    });
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &completion) };
    if !wait_for_permission(commands, &response)? {
        return Err(denied.into());
    }
    events
        .send_blocking(RecordingEvent::Preparing)
        .map_err(|_| "recording cancelled".into())
}

#[cfg(target_os = "macos")]
fn wait_for_permission(
    commands: &mpsc::Receiver<()>,
    response: &mpsc::Receiver<bool>,
) -> Result<bool, String> {
    loop {
        let permission = response.recv_timeout(std::time::Duration::from_millis(50));
        if !matches!(commands.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err("recording cancelled".into());
        }
        match permission {
            Ok(granted) => return Ok(granted),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("microphone permission request interrupted".into());
            }
        }
    }
}

fn input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    failures: mpsc::Sender<String>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut samples = samples.lock().unwrap();
                samples.extend(data.chunks_exact(channels).map(|frame| {
                    frame
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32
                }));
            },
            move |error| {
                let _ = failures.send(error.to_string());
            },
            Some(std::time::Duration::from_secs(5)),
        )
        .map_err(|error| error.to_string())
}

fn pcm(samples: Vec<f32>, rate: u32) -> Result<Vec<u8>, String> {
    use rubato::{Resampler, audioadapter_buffers::owned::InterleavedOwned};
    if samples.is_empty() {
        return Err("recording contains no audio".into());
    }
    let samples = if rate == 24_000 {
        samples
    } else {
        let frames = samples.len();
        let input =
            InterleavedOwned::new_from(samples, 1, frames).map_err(|error| error.to_string())?;
        let parameters =
            rubato::SincInterpolationParameters::new(256, rubato::WindowFunction::BlackmanHarris2);
        let mut resampler = rubato::Async::<f32>::new_sinc(
            24_000.0 / f64::from(rate),
            1.0,
            &parameters,
            1024,
            1,
            rubato::FixedAsync::Input,
        )
        .map_err(|error| error.to_string())?;
        resampler
            .process_all(&input, frames, None)
            .map_err(|error| error.to_string())?
            .take_data()
    };
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        pcm.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32768.0).round() as i16).to_le_bytes());
    }
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn native_rate_preserves_signed_pcm(samples in proptest::collection::vec(proptest::num::i16::ANY, 1..2048)) {
            let input = samples.iter().map(|sample| f32::from(*sample) / 32768.).collect();
            let expected: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
            proptest::prop_assert_eq!(pcm(input, 24_000).unwrap(), expected);
        }
    }

    #[test]
    fn pcm_clips_and_rounds_samples_and_rejects_empty_audio() {
        let samples = vec![-2., -1., -0.5, -0.5 / 32768., 0., 0.5 / 32768., 0.5, 1., 2.];
        let expected = [-32768_i16, -32768, -16384, -1, 0, 1, 16384, 32767, 32767];
        let expected: Vec<u8> = expected.into_iter().flat_map(i16::to_le_bytes).collect();
        assert_eq!(pcm(samples, 24_000).unwrap(), expected);
        assert!(pcm(Vec::new(), 24_000).is_err());
    }

    #[test]
    fn resampling_preserves_silence_duration_and_tone_with_partial_chunks() {
        for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
            let output = pcm(vec![0.; 1013], rate).unwrap();
            assert_eq!(
                output.len() / 2,
                (1013_usize * 24_000).div_ceil(rate as usize)
            );
            assert!(output.iter().all(|byte| *byte == 0));
            let tone = |frame: usize, sample_rate: u32| {
                0.3 * (std::f32::consts::TAU * 1000. * frame as f32 / sample_rate as f32).sin()
            };
            let output = pcm((0..4093).map(|frame| tone(frame, rate)).collect(), rate).unwrap();
            assert_eq!(
                output.len() / 2,
                (4093_usize * 24_000).div_ceil(rate as usize)
            );
            let frames = output.len() / 2;
            let body: Vec<f32> = output
                .as_chunks::<2>()
                .0
                .iter()
                .skip(128)
                .take(frames - 256)
                .map(|bytes| f32::from(i16::from_le_bytes(*bytes)) / 32768.)
                .collect();
            let energy = body.iter().map(|sample| sample * sample).sum::<f32>() / body.len() as f32;
            assert!(
                (energy - 0.045).abs() < 0.002,
                "rate={rate} energy={energy}"
            );
            let cycles = body
                .windows(2)
                .filter(|pair| pair[0] <= 0. && pair[1] > 0.)
                .count();
            assert!(
                cycles.abs_diff(body.len() / 24) <= 1,
                "rate={rate} cycles={cycles}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn permission_wait_handles_decisions_cancellation_and_lost_callbacks() {
        for granted in [false, true] {
            let (_control, commands) = mpsc::channel();
            let (permission, response) = mpsc::channel();
            permission.send(granted).unwrap();
            assert_eq!(wait_for_permission(&commands, &response), Ok(granted));
        }
        for decision in [None, Some(true), Some(false)] {
            let (control, commands) = mpsc::channel();
            let (permission, response) = mpsc::channel();
            if let Some(granted) = decision {
                permission.send(granted).unwrap();
            }
            drop(control);
            assert_eq!(
                wait_for_permission(&commands, &response),
                Err("recording cancelled".into())
            );
        }
        let (control, commands) = mpsc::channel();
        let (permission, response) = mpsc::channel();
        control.send(()).unwrap();
        permission.send(true).unwrap();
        assert_eq!(
            wait_for_permission(&commands, &response),
            Err("recording cancelled".into())
        );

        let (_control, commands) = mpsc::channel();
        let (permission, response) = mpsc::channel();
        let callback = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            permission.send(true).unwrap();
        });
        assert_eq!(wait_for_permission(&commands, &response), Ok(true));
        callback.join().unwrap();

        let (_control, commands) = mpsc::channel();
        let (permission, response) = mpsc::channel();
        drop(permission);
        assert_eq!(
            wait_for_permission(&commands, &response),
            Err("microphone permission request interrupted".into())
        );
    }

    #[test]
    #[ignore = "Requires a microphone and OS permission; run in a signed Mac bundle"]
    fn native_recording_starts_stops_and_restarts() {
        #[cfg(target_os = "macos")]
        prepare_microphone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let next = |incoming: &async_channel::Receiver<RecordingEvent>| {
            runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), incoming.recv())
                    .await
                    .expect("recording timed out")
            })
        };
        for attempt in 0..5 {
            let (events, incoming) = async_channel::unbounded();
            let started = std::time::Instant::now();
            let recording = start_recording(events).unwrap();
            loop {
                match next(&incoming).unwrap() {
                    RecordingEvent::Started => break,
                    RecordingEvent::Finished(Err(error)) => panic!("recording failed: {error}"),
                    RecordingEvent::Finished(Ok(_)) => {
                        panic!("recording finished before it started")
                    }
                    _ => {}
                }
            }
            eprintln!(
                "recording attempt {attempt}: started in {:?}",
                started.elapsed()
            );
            let level = next(&incoming).unwrap();
            assert!(
                matches!(level, RecordingEvent::Level(value) if value.is_finite() && (0.0..=1.0).contains(&value))
            );
            recording.finish().unwrap();
            loop {
                match next(&incoming).unwrap() {
                    RecordingEvent::Finished(result) => {
                        let audio = result.unwrap();
                        assert!(!audio.is_empty());
                        assert_eq!(audio.len() % 2, 0);
                        break;
                    }
                    RecordingEvent::Level(_) => {}
                    _ => panic!("unexpected recording state"),
                }
            }
            assert!(next(&incoming).is_err());
        }
        let (events, incoming) = async_channel::unbounded();
        let recording = start_recording(events).unwrap();
        drop(recording);
        loop {
            if let RecordingEvent::Finished(result) = next(&incoming).unwrap() {
                assert_eq!(result, Err("recording cancelled".into()));
                break;
            }
        }
    }
}
