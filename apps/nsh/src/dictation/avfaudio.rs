//! Capture with AVFAudio, in the format transcription takes.
use crate::l10n::tr;
use block2::RcBlock;
use objc2::{AnyThread, rc::Retained, runtime::Bool};
use objc2_avf_audio::*;
use objc2_foundation::{NSString, NSURL};

pub(super) struct Capture {
    recorder: Retained<AVAudioRecorder>,
    file: tempfile::TempPath,
    #[cfg(target_os = "ios")]
    _session: AudioSession,
}

impl Capture {
    pub(super) async fn request_permission() -> Result<(), String> {
        let (send, receive) = async_channel::bounded(1);
        let permission = RcBlock::new(move |allowed: Bool| {
            let _ = send.try_send(allowed.as_bool());
        });
        // AVAudioApplication copies the callback; the result resumes on NSH's UI executor.
        unsafe {
            AVAudioApplication::requestRecordPermissionWithCompletionHandler(&permission);
        }
        if !receive.recv().await.map_err(|reason| reason.to_string())? {
            return Err(tr("設定アプリでBexのマイクへのアクセスを許可してください。").into());
        }
        Ok(())
    }

    pub(super) fn start() -> Result<Self, String> {
        #[cfg(target_os = "ios")]
        let session = AudioSession::record()?;
        let file = tempfile::Builder::new()
            .prefix("bex-dictation-")
            .suffix(".wav")
            .tempfile()
            .map_err(|reason| reason.to_string())?
            .into_temp_path();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        // A mono 24 kHz signed 16-bit PCM format matches core's transcription contract.
        let recorder = unsafe {
            let format = AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
                AVAudioFormat::alloc(),
                AVAudioCommonFormat::PCMFormatInt16,
                24_000.0,
                1,
                true,
            )
            .ok_or(tr("録音形式を作成できませんでした。"))?;
            AVAudioRecorder::initWithURL_format_error(AVAudioRecorder::alloc(), &url, &format)
                .map_err(|reason| reason.localizedDescription().to_string())?
        };
        let capture = Self {
            recorder,
            file,
            #[cfg(target_os = "ios")]
            _session: session,
        };
        unsafe { capture.recorder.setMeteringEnabled(true) };
        if !unsafe { capture.recorder.record() } {
            return Err(tr("録音を開始できませんでした。").into());
        }
        Ok(capture)
    }

    pub(super) fn level(&self) -> f32 {
        unsafe {
            self.recorder.updateMeters();
            10_f32.powf(self.recorder.averagePowerForChannel(0) / 20.0)
        }
    }

    pub(super) fn finish(self) -> Result<Vec<u8>, String> {
        // The recorder and buffer stay owned here until the sample copy completes.
        unsafe {
            if !self.recorder.isRecording() {
                return Err(tr("録音が中断されました。もう一度録音してください。").into());
            }
            self.recorder.stop();
            let url = NSURL::fileURLWithPath(&NSString::from_str(&self.file.to_string_lossy()));
            let file = AVAudioFile::initForReading_commonFormat_interleaved_error(
                AVAudioFile::alloc(),
                &url,
                AVAudioCommonFormat::PCMFormatInt16,
                true,
            )
            .map_err(|reason| reason.localizedDescription().to_string())?;
            let length = u32::try_from(file.length())
                .ok()
                .filter(|length| *length > 0)
                .ok_or(tr("音声を録音できませんでした。"))?;
            let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &file.processingFormat(),
                length,
            )
            .ok_or(tr("録音データを読み込めませんでした。"))?;
            file.readIntoBuffer_error(&buffer)
                .map_err(|reason| reason.localizedDescription().to_string())?;
            let channels = buffer.int16ChannelData();
            if channels.is_null() {
                return Err(tr("録音データを読み込めませんでした。").into());
            }
            let samples =
                std::slice::from_raw_parts((*channels).as_ptr(), buffer.frameLength() as usize);
            Ok(samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect())
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            self.recorder.stop();
        }
    }
}

#[cfg(target_os = "ios")]
struct AudioSession {
    session: Retained<AVAudioSession>,
    category: Retained<NSString>,
    mode: Retained<NSString>,
    options: AVAudioSessionCategoryOptions,
}
#[cfg(target_os = "ios")]
impl AudioSession {
    fn record() -> Result<Self, String> {
        // Retain the previous configuration before making any fallible change.
        unsafe {
            let session = AVAudioSession::sharedInstance();
            let previous = Self {
                category: session.category(),
                mode: session.mode(),
                options: session.categoryOptions(),
                session,
            };
            previous
                .session
                .setCategory_mode_options_error(
                    AVAudioSessionCategoryRecord.ok_or(tr("録音を利用できません。"))?,
                    AVAudioSessionModeMeasurement.ok_or(tr("録音を利用できません。"))?,
                    AVAudioSessionCategoryOptions::empty(),
                )
                .map_err(|reason| reason.localizedDescription().to_string())?;
            previous
                .session
                .setActive_error(true)
                .map_err(|reason| reason.localizedDescription().to_string())?;
            Ok(previous)
        }
    }
}
#[cfg(target_os = "ios")]
impl Drop for AudioSession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.session.setActive_withOptions_error(
                false,
                AVAudioSessionSetActiveOptions::NotifyOthersOnDeactivation,
            );
            let _ = self.session.setCategory_mode_options_error(
                &self.category,
                &self.mode,
                self.options,
            );
        }
    }
}
