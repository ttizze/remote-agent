//! Capture from the default microphone in its own shared format, averaged to
//! mono; core converts it to the format transcription takes.
use crate::l10n::tr;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

pub(super) struct Capture {
    stream: cpal::Stream,
    rate: u32,
    samples: Arc<Mutex<Vec<f32>>>,
    /// The first error that ended the stream.
    failure: Arc<Mutex<Option<String>>>,
}

impl Capture {
    /// Windows asks no app for its microphone: its privacy settings either
    /// let the stream open or fail it.
    pub(super) async fn request_permission() -> Result<(), String> {
        Ok(())
    }

    pub(super) fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or(tr("マイクが見つかりません。"))?;
        let supported = device
            .default_input_config()
            .map_err(|reason| reason.to_string())?;
        let rate = supported.sample_rate();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let failure = Arc::new(Mutex::new(None));
        let (config, recorded, failed) = (supported.config(), samples.clone(), failure.clone());
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => input::<f32>(&device, config, recorded, failed),
            cpal::SampleFormat::I16 => input::<i16>(&device, config, recorded, failed),
            cpal::SampleFormat::I32 => input::<i32>(&device, config, recorded, failed),
            cpal::SampleFormat::U8 => input::<u8>(&device, config, recorded, failed),
            format => Err(format!("unsupported microphone sample format: {format}")),
        }?;
        stream.play().map_err(|reason| reason.to_string())?;
        Ok(Self {
            stream,
            rate,
            samples,
            failure,
        })
    }

    pub(super) fn finish(self) -> Result<Vec<u8>, String> {
        drop(self.stream);
        if let Some(reason) = self.failure.lock().unwrap().take() {
            return Err(reason);
        }
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        agent_core::recording::transcription_pcm(samples, self.rate)
    }

    pub(super) fn level(&self) -> f32 {
        let samples = self.samples.lock().unwrap();
        let recent = &samples[samples.len().saturating_sub((self.rate / 20) as usize)..];
        if recent.is_empty() {
            0.0
        } else {
            (recent.iter().map(|sample| sample * sample).sum::<f32>() / recent.len() as f32).sqrt()
        }
    }
}

/// A stream that averages each frame of `T` samples into `samples`.
fn input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    failure: Arc<Mutex<Option<String>>>,
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
                samples
                    .lock()
                    .unwrap()
                    .extend(data.chunks_exact(channels).map(|frame| {
                        frame
                            .iter()
                            .map(|sample| sample.to_sample::<f32>())
                            .sum::<f32>()
                            / channels as f32
                    }));
            },
            move |error| {
                if ends_recording(error.kind()) {
                    failure.lock().unwrap().get_or_insert(error.to_string());
                }
            },
            Some(std::time::Duration::from_secs(5)),
        )
        .map_err(|reason| reason.to_string())
}

/// WASAPI reports a discontinuity or a device change shortly after capture
/// starts; the stream stays usable, so the recording continues.
fn ends_recording(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}
