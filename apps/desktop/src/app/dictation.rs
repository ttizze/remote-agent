use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase { Permission, Recording, Transcribing }

pub(super) struct Dictation {
    id: uuid::Uuid,
    key: String,
    generation: u64,
    pub(super) phase: Phase,
    send: bool,
    draft: String,
    files: Vec<Value>,
    // Dropping stdin makes the native helper stop and exit. Its worker reaps
    // the child and removes both temporary audio files on every exit path.
    control: Option<ChildStdin>,
    rpc: Rpc,
}

pub(super) enum DictationEvent {
    Recording,
    Audio(Result<String, String>),
    Transcript(Result<Value, String>),
}

impl Desktop {
    pub(super) fn start_dictation(&mut self) {
        if self.dictation.is_some() || !self.connected || self.busy > 0 { return; }
        let result = (|| {
            let directory = tempfile::Builder::new().prefix("bex-dictation-").tempdir()?;
            let helper = std::env::current_exe()?.parent().unwrap()
                .join("../Resources/Bex Dictation.app/Contents/MacOS/Dictation");
            let mut child = Command::new(helper).arg(directory.path())
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
            let control = child.stdin.take();
            let stdout = child.stdout.take().unwrap();
            let id = uuid::Uuid::new_v4();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let result = (|| -> Result<String, String> {
                    for line in BufReader::new(stdout).lines() {
                        let value: Value = serde_json::from_str(&line.map_err(|e| e.to_string())?)
                            .map_err(|_| "録音状態を読み込めませんでした。")?;
                        if let Some(error) = value["error"].as_str() { return Err(error.into()); }
                        if value["recording"] == true {
                            tx.send_blocking(Event::Dictation(id, DictationEvent::Recording))
                                .map_err(|_| "録音を中止しました。")?;
                        } else if value["complete"] == true {
                            let audio = std::fs::read(directory.path().join("recording.pcm")).map_err(|e| e.to_string())?;
                            if audio.is_empty() || audio.len() % 2 != 0 { return Err("録音データが無効です。".into()); }
                            return Ok(base64::engine::general_purpose::STANDARD.encode(audio));
                        }
                    }
                    Err("録音が中断されました。もう一度録音してください。".into())
                })();
                let _ = child.kill();
                let _ = child.wait();
                drop(directory);
                let _ = tx.send_blocking(Event::Dictation(id, DictationEvent::Audio(result)));
            });
            Ok::<_, std::io::Error>(Dictation { id, key: self.draft_key(), generation: self.load_generation,
                phase: Phase::Permission, send: false, draft: String::new(), files: Vec::new(), control, rpc: self.rpc.clone() })
        })();
        match result {
            Ok(state) => { self.error.clear(); self.dictation = Some(state); }
            Err(error) => self.error = format!("録音を開始できませんでした: {error}"),
        }
    }

    pub(super) fn finish_dictation(&mut self, send: bool, cx: &Context<Self>) {
        let Some(state) = self.dictation.as_mut().filter(|d| d.phase == Phase::Recording) else { return; };
        if let Err(error) = state.control.as_mut().unwrap().write_all(b"stop\n") {
            self.error = format!("録音を終了できませんでした: {error}");
            self.dictation = None;
            return;
        }
        state.phase = Phase::Transcribing;
        state.send = send;
        if send {
            state.draft = self.composer.read(cx).value().to_string();
            state.files = array(&self.cache["attachments"][&state.key]).to_vec();
        }
    }

    pub(super) fn cancel_recording(&mut self) {
        if let Some(state) = self.dictation.as_mut() {
            // Navigating away and back must not restore a previous send intent.
            state.send = false;
            if state.phase != Phase::Transcribing { self.dictation = None; }
        }
    }

    pub(super) fn dictation_event(&mut self, id: uuid::Uuid, event: DictationEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.dictation.as_mut().filter(|d| d.id == id) else { return; };
        match event {
            DictationEvent::Recording => state.phase = Phase::Recording,
            DictationEvent::Audio(Ok(audio)) => {
                state.control = None;
                let tx = self.tx.clone();
                state.rpc.request_async("host/dictation/transcribe", json!({"audio": audio}), move |result| {
                    let _ = tx.send_blocking(Event::Dictation(id, DictationEvent::Transcript(result)));
                });
            }
            DictationEvent::Audio(Err(error)) | DictationEvent::Transcript(Err(error)) => {
                self.dictation = None;
                self.error = error;
            }
            DictationEvent::Transcript(Ok(mut value)) => {
                let state = self.dictation.take().unwrap();
                let Value::String(transcript) = value["text"].take() else {
                    self.error = "音声を認識できませんでした。もう一度録音してください。".into();
                    return;
                };
                if transcript.trim().is_empty() {
                    self.error = "音声を認識できませんでした。もう一度録音してください。".into();
                    return;
                }
                let current = state.key == self.draft_key();
                if current && state.send && state.generation == self.load_generation && self.connected && self.busy == 0 {
                    self.submit(state.draft, state.files, Some(transcript), cx);
                    return;
                }
                let draft = if current { self.composer.read(cx).value().to_string() }
                    else { text(&self.cache["messages"], &state.key).to_owned() };
                let combined = append_text(&draft, &transcript);
                self.cache["messages"][&state.key] = json!(combined);
                self.persist();
                if current {
                    self.restore_draft(window, cx);
                }
            }
        }
    }
}

pub(super) fn append_text(draft: &str, transcript: &str) -> String {
    let separator = if draft.is_empty() || draft.ends_with(char::is_whitespace) { "" } else { "\n" };
    format!("{draft}{separator}{transcript}")
}
