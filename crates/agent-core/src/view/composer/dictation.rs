//! Dictation in the mobile composer: what the toolbar shows in each phase of
//! a recording, the waveform's levels, and the recording limit.

pub const VOICE_RECORDING_LIMIT_SECONDS: u32 = 5 * 60;
pub const VOICE_WAVEFORM_SAMPLE_COUNT: u32 = 64;
const VOICE_NOISE_FLOOR_DECIBELS: f64 = -60.0;

/// A recording stops by itself after this many seconds.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn voice_recording_limit_seconds() -> u32 {
    VOICE_RECORDING_LIMIT_SECONDS
}

/// Samples the waveform keeps, newest last.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn voice_waveform_sample_count() -> u32 {
    VOICE_WAVEFORM_SAMPLE_COUNT
}

/// Why a dictation stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DictationFailure {
    /// `open_settings`: the permission can now only be granted in Settings.
    MicrophoneDenied { open_settings: bool },
    CouldNotStart,
    Interrupted,
    Backgrounded,
    TranscriptionFailed,
}
impl DictationFailure {
    pub fn message(&self) -> &'static str {
        match self {
            Self::MicrophoneDenied { .. } => "Microphone access is required for voice input.",
            Self::CouldNotStart => "Could not start voice recording.",
            Self::Interrupted => "Voice recording was interrupted.",
            Self::Backgrounded => "Voice input stopped when the app moved to the background.",
            Self::TranscriptionFailed => "Could not transcribe this recording.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DictationPhase {
    Idle,
    /// Waiting for the microphone permission and the Host.
    Preparing,
    Recording,
    Transcribing,
    Error { failure: DictationFailure },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DictationPresentation {
    /// The leading "Cancel dictation" button.
    pub cancel: bool,
    /// The trailing button finishes the recording; otherwise it starts one.
    pub confirm: bool,
    /// Confirming is possible; otherwise the button shows a spinner.
    pub confirmation_enabled: bool,
    pub shows_send: bool,
    /// The status in place of the toolbar's controls.
    pub status: Option<String>,
    /// The status is an error with a dismiss button.
    pub error: bool,
    /// Sending and editing wait for the dictation.
    pub blocks_submission: bool,
    /// The mic opens the app's settings to grant the microphone.
    pub opens_settings: bool,
}

/// "1:04".
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn dictation_elapsed_label(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The composer toolbar for a dictation phase `elapsed_seconds` into it.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn dictation_presentation(phase: DictationPhase, elapsed_seconds: u32) -> DictationPresentation {
    let active = |status: String, confirmation_enabled: bool| DictationPresentation {
        cancel: true,
        confirm: true,
        confirmation_enabled,
        shows_send: false,
        status: Some(status),
        error: false,
        blocks_submission: true,
        opens_settings: false,
    };
    match phase {
        DictationPhase::Idle => DictationPresentation {
            cancel: false,
            confirm: false,
            confirmation_enabled: false,
            shows_send: true,
            status: None,
            error: false,
            blocks_submission: false,
            opens_settings: false,
        },
        DictationPhase::Error { failure } => DictationPresentation {
            cancel: false,
            confirm: false,
            confirmation_enabled: false,
            shows_send: true,
            status: Some(failure.message().into()),
            error: true,
            blocks_submission: false,
            opens_settings: matches!(
                failure,
                DictationFailure::MicrophoneDenied {
                    open_settings: true
                }
            ),
        },
        DictationPhase::Preparing => active("Preparing".into(), false),
        DictationPhase::Recording => active(
            format!("Recording {}", dictation_elapsed_label(elapsed_seconds)),
            true,
        ),
        DictationPhase::Transcribing => active("Transcribing".into(), false),
    }
}

/// A microphone reading in decibels as a waveform level from 0 to 1,
/// compressed so speech stays distinct and full height means 0 dB.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn normalize_voice_input_decibels(decibels: Option<f64>) -> f64 {
    let Some(decibels) = decibels.filter(|value| value.is_finite()) else {
        return 0.0;
    };
    if decibels <= VOICE_NOISE_FLOOR_DECIBELS {
        return 0.0;
    }
    if decibels >= 0.0 {
        return 1.0;
    }
    let floor = 10f64.powf(VOICE_NOISE_FLOOR_DECIBELS / 20.0);
    let amplitude = 10f64.powf(decibels / 20.0);
    ((amplitude - floor) / (1.0 - floor)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_voice_states_to_stable_composer_actions_and_editor_read_only_state() {
        let idle = dictation_presentation(DictationPhase::Idle, 0);
        assert_eq!(
            idle,
            DictationPresentation {
                cancel: false,
                confirm: false,
                confirmation_enabled: false,
                shows_send: true,
                status: None,
                error: false,
                blocks_submission: false,
                opens_settings: false,
            }
        );
        let preparing = dictation_presentation(DictationPhase::Preparing, 0);
        assert!(preparing.cancel && preparing.confirm && !preparing.shows_send);
        assert_eq!(preparing.status.as_deref(), Some("Preparing"));
        assert!(!preparing.confirmation_enabled);
        let recording = dictation_presentation(DictationPhase::Recording, 64);
        assert!(recording.cancel && recording.confirm && !recording.shows_send);
        assert_eq!(recording.status.as_deref(), Some("Recording 1:04"));
        assert!(recording.confirmation_enabled);
        let transcribing = dictation_presentation(DictationPhase::Transcribing, 0);
        assert_eq!(transcribing.status.as_deref(), Some("Transcribing"));
        assert!(!transcribing.confirmation_enabled);
        let error = dictation_presentation(
            DictationPhase::Error {
                failure: DictationFailure::MicrophoneDenied {
                    open_settings: false,
                },
            },
            0,
        );
        assert!(!error.cancel && !error.confirm && error.shows_send && error.error);
        assert_eq!(
            error.status.as_deref(),
            Some("Microphone access is required for voice input.")
        );
        assert!(!error.opens_settings);
        assert!(preparing.blocks_submission);
        assert!(recording.blocks_submission);
        assert!(transcribing.blocks_submission);
        assert!(!idle.blocks_submission);
    }

    #[test]
    fn treats_a_missing_or_invalid_reading_as_silence() {
        for decibels in [None, Some(f64::NAN), Some(f64::INFINITY), Some(f64::NEG_INFINITY)] {
            assert_eq!(normalize_voice_input_decibels(decibels), 0.0);
        }
    }

    #[test]
    fn keeps_a_reading_at_or_below_the_noise_floor_silent() {
        for decibels in [-160.0, -90.0, -60.0] {
            assert_eq!(normalize_voice_input_decibels(Some(decibels)), 0.0);
        }
    }

    #[test]
    fn keeps_quiet_background_readings_close_to_the_baseline() {
        let quiet = normalize_voice_input_decibels(Some(-50.0));
        assert!(quiet > 0.0 && quiet < 0.05);
    }

    #[test]
    fn keeps_loud_negative_speech_readings_distinct_below_full_height() {
        let levels: Vec<f64> = [-20.0, -18.0, -12.0, -6.0, -3.0]
            .map(|db| normalize_voice_input_decibels(Some(db)))
            .to_vec();
        assert!(levels.iter().all(|level| *level > 0.0 && *level < 1.0));
        assert!(levels.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn makes_near_speech_changes_visible_without_an_early_ceiling() {
        let level = |db: f64| normalize_voice_input_decibels(Some(db));
        assert!(level(-6.0) - level(-12.0) > 0.18);
        assert!(level(-3.0) - level(-12.0) > 0.3);
    }

    #[test]
    fn increases_throughout_the_usable_microphone_range() {
        let levels: Vec<f64> = [
            -60.0, -55.0, -50.0, -40.0, -30.0, -20.0, -12.0, -6.0, -3.0, -0.001, 0.0,
        ]
        .map(|db| normalize_voice_input_decibels(Some(db)))
        .to_vec();
        assert!(levels.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn approaches_the_noise_floor_and_full_scale_without_a_jump() {
        assert!(normalize_voice_input_decibels(Some(-59.999)) < 0.001);
        assert!(normalize_voice_input_decibels(Some(-0.001)) > 0.999);
        assert!(normalize_voice_input_decibels(Some(-0.001)) < 1.0);
    }

    #[test]
    fn caps_only_full_scale_or_higher_readings_at_one() {
        for decibels in [0.0, 6.0, 160.0] {
            assert_eq!(normalize_voice_input_decibels(Some(decibels)), 1.0);
        }
    }
}
