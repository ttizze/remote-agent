//! Device settings are edited when an action is accepted, never inferred from
//! a Host's later save. Each Host only keeps an immutable copy for presentation.
use crate::{
    client::{AgentError, error},
    localization::UiLanguage,
    persistence::EditorPreferences,
    state::{ComposerPreferences, ModelPreferences, ModelSelection},
};
use agent_protocol::editors::EditorId;
use serde::de::DeserializeOwned;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Copy)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PreferenceKey {
    Models,
    Editor,
    Language,
    Composer,
    Contrast,
}

/// Device-wide locations, independent of a Host or window's state file.
#[cfg_attr(feature = "bindings", uniffi::export(rust, foreign))]
pub trait PreferenceStorage: Send + Sync {
    fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, AgentError>;
    fn save(&self, key: PreferenceKey, bytes: Vec<u8>) -> Result<(), AgentError>;
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct Preferences {
    pub models: ModelPreferences,
    pub editor: EditorPreferences,
    pub language: UiLanguage,
    pub composer: ComposerPreferences,
    /// Unset, or unreadable, is the default contrast.
    pub contrast: Option<u32>,
}

#[derive(Clone, Copy)]
pub(crate) enum ModelChoice {
    Model,
    Effort,
    ServiceTier,
}

/// An option edit preserves another window's other option for the same model.
/// Selecting a different model brings that model's own resolved options.
fn choose_model(
    current: &ModelPreferences,
    choice: ModelChoice,
    mut selection: ModelSelection,
) -> ModelPreferences {
    let mut next = current.clone();
    let provider = selection.model.provider;
    if let Some(previous) = current.selections.get(&provider)
        && previous.model == selection.model
    {
        match choice {
            ModelChoice::Model => (),
            ModelChoice::Effort => selection.service_tier = previous.service_tier.clone(),
            ModelChoice::ServiceTier => selection.effort = previous.effort.clone(),
        }
    }
    next.provider = Some(provider);
    next.selections.insert(provider, selection);
    next
}

struct DeviceState {
    current: Arc<Preferences>,
    restored: Result<(), AgentError>,
    saved_models: Option<ModelPreferences>,
    saved_editor: Option<EditorPreferences>,
    saved_composer: Option<ComposerPreferences>,
}

struct Write {
    complete: Option<oneshot::Sender<Result<(), AgentError>>>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct DevicePreferences {
    storage: Option<Arc<dyn PreferenceStorage>>,
    state: Arc<Mutex<DeviceState>>,
    language_write: Mutex<()>,
    updates: watch::Sender<Arc<Preferences>>,
    failure: watch::Sender<Option<String>>,
    writes: mpsc::UnboundedSender<Write>,
    pending_writer: Mutex<Option<mpsc::UnboundedReceiver<Write>>>,
}

#[cfg_attr(feature = "bindings", uniffi::export(async_runtime = "tokio"))]
impl DevicePreferences {
    /// Finish the first read before a native window or composition is created.
    /// Invalid model/editor files reject Host restoration. Language failure only
    /// chooses the display default, and never rewrites the damaged payload.
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn new(storage: Arc<dyn PreferenceStorage>) -> Arc<Self> {
        let language = read(storage.as_ref(), PreferenceKey::Language)
            .ok()
            .flatten()
            .unwrap_or_default();
        let contrast = read(storage.as_ref(), PreferenceKey::Contrast)
            .ok()
            .flatten();
        let mut state = DeviceState {
            current: Arc::new(Preferences {
                language,
                contrast,
                ..Default::default()
            }),
            restored: Ok(()),
            saved_models: None,
            saved_editor: None,
            saved_composer: None,
        };
        state.restored = state.load(storage.as_ref());
        Self::from_state(Some(storage), state)
    }

    pub fn language(&self) -> UiLanguage {
        self.state.lock().unwrap().current.language
    }

    /// Language becomes visible only after this write succeeds. Native mobile
    /// adapters keep their existing non-throwing preferences save contract.
    pub fn select_language(&self, language: UiLanguage) -> Result<(), AgentError> {
        self.write_display(PreferenceKey::Language, &language, |preferences| {
            preferences.language = language;
        })
    }

    pub fn contrast(&self) -> u32 {
        use crate::presentation::appearance::{DEFAULT_CONTRAST, MAX_CONTRAST, MIN_CONTRAST};
        let contrast = self.state.lock().unwrap().current.contrast;
        contrast.map_or(DEFAULT_CONTRAST, |contrast| {
            contrast.clamp(MIN_CONTRAST, MAX_CONTRAST)
        })
    }

    /// As the language, the contrast is visible only after its write succeeds.
    pub fn select_contrast(&self, contrast: u32) -> Result<(), AgentError> {
        self.write_display(PreferenceKey::Contrast, &contrast, |preferences| {
            preferences.contrast = Some(contrast);
        })
    }

    pub async fn next_language(&self, previous: UiLanguage) -> Result<UiLanguage, AgentError> {
        let mut updates = self.updates.subscribe();
        loop {
            let language = updates.borrow_and_update().language;
            if language != previous {
                return Ok(language);
            }
            updates.changed().await.map_err(error)?;
        }
    }
}

impl DevicePreferences {
    fn write_display<T: serde::Serialize>(
        &self,
        key: PreferenceKey,
        value: &T,
        apply: impl FnOnce(&mut Preferences),
    ) -> Result<(), AgentError> {
        let writing = self.language_write.lock().unwrap();
        if let Some(storage) = &self.storage {
            storage.save(key, serde_json::to_vec(value).map_err(error)?)?;
        }
        {
            let mut state = self.state.lock().unwrap();
            let mut next = (*state.current).clone();
            apply(&mut next);
            if *state.current != next {
                state.current = Arc::new(next);
            }
        }
        drop(writing);
        self.notify();
        Ok(())
    }

    pub(crate) async fn flush(&self) -> Result<(), AgentError> {
        if self.storage.is_none() {
            return Ok(());
        }
        self.start_writer();
        let (complete, result) = oneshot::channel();
        self.writes
            .send(Write {
                complete: Some(complete),
            })
            .map_err(error)?;
        result.await.map_err(error)?
    }

    pub(crate) fn memory(preferences: Preferences) -> Arc<Self> {
        Self::from_state(
            None,
            DeviceState {
                saved_models: Some(preferences.models.clone()),
                saved_editor: Some(preferences.editor.clone()),
                saved_composer: Some(preferences.composer),
                current: Arc::new(preferences),
                restored: Ok(()),
            },
        )
    }

    fn from_state(storage: Option<Arc<dyn PreferenceStorage>>, state: DeviceState) -> Arc<Self> {
        let (writes, pending_writer) = mpsc::unbounded_channel();
        Arc::new(Self {
            updates: watch::channel(state.current.clone()).0,
            state: Arc::new(Mutex::new(state)),
            storage,
            language_write: Mutex::default(),
            failure: watch::channel(None).0,
            writes,
            pending_writer: Mutex::new(Some(pending_writer)),
        })
    }

    pub(crate) fn current(&self) -> Arc<Preferences> {
        self.state.lock().unwrap().current.clone()
    }

    pub(crate) fn restore(&self) -> Result<Arc<Preferences>, AgentError> {
        let preferences = {
            let mut state = self.state.lock().unwrap();
            if state.restored.is_err() {
                state.restored =
                    state.load(self.storage.as_ref().expect("disk restoration").as_ref());
            }
            state.restored.clone()?;
            state.current.clone()
        };
        self.notify();
        Ok(preferences)
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<Preferences>> {
        self.updates.subscribe()
    }

    pub fn failures(&self) -> watch::Receiver<Option<String>> {
        self.failure.subscribe()
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.borrow().clone()
    }

    /// Store calls this after admission, while it owns its publication lock.
    /// It publishes the resulting native snapshot before notifying our readers.
    pub(crate) fn apply_model(
        &self,
        choice: ModelChoice,
        selection: ModelSelection,
    ) -> Result<Arc<Preferences>, AgentError> {
        let mut state = self.state.lock().unwrap();
        state.restored.clone()?;
        let models = choose_model(&state.current.models, choice, selection);
        if models != state.current.models {
            Arc::make_mut(&mut state.current).models = models;
        }
        Ok(state.current.clone())
    }

    pub(crate) fn apply_editor(&self, editor: EditorId) -> Result<Arc<Preferences>, AgentError> {
        let mut state = self.state.lock().unwrap();
        state.restored.clone()?;
        if state.current.editor.preferred != Some(editor) {
            Arc::make_mut(&mut state.current).editor.preferred = Some(editor);
        }
        Ok(state.current.clone())
    }

    pub(crate) fn apply_composer(
        &self,
        composer: ComposerPreferences,
    ) -> Result<Arc<Preferences>, AgentError> {
        let mut state = self.state.lock().unwrap();
        state.restored.clone()?;
        if state.current.composer != composer {
            Arc::make_mut(&mut state.current).composer = composer;
        }
        Ok(state.current.clone())
    }

    pub(crate) fn publish(&self) {
        self.notify();
        if self.storage.is_some()
            && let Err(reason) = self.writes.send(Write { complete: None })
        {
            super::publish_failure(&self.failure, Some(error(reason).to_string()));
        }
    }

    fn notify(&self) {
        // Read the latest value under the watch lock: two windows can finish
        // publication in the opposite order to their accepted edits.
        self.updates.send_if_modified(|published| {
            let current = self.current();
            if Arc::ptr_eq(published, &current) {
                false
            } else {
                *published = current;
                true
            }
        });
    }

    pub(crate) fn start_writer(&self) {
        let Some(storage) = self.storage.clone() else {
            return;
        };
        let Some(mut requests) = self.pending_writer.lock().unwrap().take() else {
            return;
        };
        let state = self.state.clone();
        let failure = self.failure.clone();
        tokio::spawn(async move {
            while let Some(Write { complete }) = requests.recv().await {
                let (state, storage) = (state.clone(), storage.clone());
                let result = tokio::task::spawn_blocking(move || persist(&state, storage.as_ref()))
                    .await
                    .map_err(error)
                    .and_then(|result| result);
                super::publish_failure(&failure, result.as_ref().err().map(ToString::to_string));
                if let Some(complete) = complete {
                    let _ = complete.send(result);
                }
            }
        });
    }
}

impl DeviceState {
    fn load(&mut self, storage: &dyn PreferenceStorage) -> Result<(), AgentError> {
        let models: Option<ModelPreferences> = read(storage, PreferenceKey::Models)?;
        let editor: Option<EditorPreferences> = read(storage, PreferenceKey::Editor)?;
        let composer: Option<ComposerPreferences> = read(storage, PreferenceKey::Composer)?;
        self.current = Arc::new(Preferences {
            models: models.clone().unwrap_or_default(),
            editor: editor.clone().unwrap_or_default(),
            language: self.current.language,
            composer: composer.unwrap_or_default(),
            contrast: self.current.contrast,
        });
        self.saved_models = models;
        self.saved_editor = editor;
        self.saved_composer = composer;
        Ok(())
    }
}

fn read<T: DeserializeOwned>(
    storage: &dyn PreferenceStorage,
    key: PreferenceKey,
) -> Result<Option<T>, AgentError> {
    let bytes = storage.load(key)?;
    if bytes.is_empty() {
        Ok(None)
    } else {
        serde_json::from_slice(&bytes).map(Some).map_err(error)
    }
}

fn persist(state: &Mutex<DeviceState>, storage: &dyn PreferenceStorage) -> Result<(), AgentError> {
    let (current, models, editor, composer) = {
        let state = state.lock().unwrap();
        state.restored.clone()?;
        (
            state.current.clone(),
            state.saved_models.as_ref() != Some(&state.current.models),
            state.saved_editor.as_ref() != Some(&state.current.editor),
            state.saved_composer != Some(state.current.composer),
        )
    };
    if models {
        storage.save(
            PreferenceKey::Models,
            serde_json::to_vec(&current.models).map_err(error)?,
        )?;
        state.lock().unwrap().saved_models = Some(current.models.clone());
    }
    if editor {
        storage.save(
            PreferenceKey::Editor,
            serde_json::to_vec(&current.editor).map_err(error)?,
        )?;
        state.lock().unwrap().saved_editor = Some(current.editor.clone());
    }
    if composer {
        storage.save(
            PreferenceKey::Composer,
            serde_json::to_vec(&current.composer).map_err(error)?,
        )?;
        state.lock().unwrap().saved_composer = Some(current.composer);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
