use super::RecordingEvent;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, mpsc};

pub(crate) struct Recording(mpsc::Sender<()>);
impl Recording {
    pub(crate) fn finish(&mut self) -> Result<(), String> {
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
