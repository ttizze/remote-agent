use super::Event;
use crate::platform;
use gpui_kit::{App, Entity, EventEmitter};
use serde_json::{Value, json};
use std::sync::mpsc;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Scope {
    Main,
    SideChat,
}
impl Scope {
    fn file(self) -> &'static str {
        match self {
            Self::Main => "desktop-drafts.json",
            Self::SideChat => "desktop-side-drafts.json",
        }
    }
}
pub(super) struct SaveError {
    pub(super) scope: Scope,
    pub(super) message: String,
}
pub(super) struct Save {
    pub(super) scope: Scope,
    pub(super) cache: Value,
}

/// Plain draft values shared by the file editor and conversation projections.
#[derive(Default)]
pub(super) struct State {
    caches: [Value; 2],
    errors: [Option<String>; 2],
    pub(super) revisions: [u64; 2],
}
impl EventEmitter<SaveError> for State {}
impl State {
    pub(super) fn cache(&self, scope: Scope) -> &Value {
        &self.caches[scope as usize]
    }
    pub(super) fn error(&self, scope: Scope) -> Option<&str> {
        self.errors[scope as usize].as_deref()
    }
}

pub(super) fn read() -> State {
    let [(main, main_error), (side, side_error)] = [load(Scope::Main), load(Scope::SideChat)];
    State {
        caches: [main, side],
        errors: [main_error, side_error],
        revisions: [0; 2],
    }
}

pub(super) fn change(
    mut state: State,
    scope: Scope,
    transform: impl FnOnce(Value) -> Value,
) -> State {
    let index = scope as usize;
    state.caches[index] = transform(state.caches[index].take());
    state.revisions[index] += 1;
    state
}

/// GPUI adapter: replace the observed value with the pure transition result.
pub(super) fn update(
    entity: &Entity<State>,
    scope: Scope,
    cx: &mut App,
    transform: impl FnOnce(Value) -> Value,
) {
    entity.update(cx, |state, cx| {
        *state = change(std::mem::take(state), scope, transform);
        cx.notify();
    });
}

pub(super) fn pending_saves(state: State, saved: [u64; 2]) -> [Option<Save>; 2] {
    let revisions = state.revisions;
    let [main, side] = state.caches;
    [
        (revisions[0] != saved[0]).then_some(Save {
            scope: Scope::Main,
            cache: main,
        }),
        (revisions[1] != saved[1]).then_some(Save {
            scope: Scope::SideChat,
            cache: side,
        }),
    ]
}

pub(super) fn writer(events: async_channel::Sender<Event>) -> mpsc::Sender<Save> {
    let (writer, saves) = mpsc::channel::<Save>();
    std::thread::spawn(move || {
        while let Ok(first) = saves.recv() {
            let mut pending = [None, None];
            let index = first.scope as usize;
            pending[index] = Some(first);
            while let Ok(newer) = saves.try_recv() {
                let index = newer.scope as usize;
                pending[index] = Some(newer);
            }
            for save in pending.into_iter().flatten() {
                if let Err(message) = platform::save_cache(save.scope.file(), &save.cache) {
                    let _ = events.send_blocking(Event::DraftError(SaveError {
                        scope: save.scope,
                        message,
                    }));
                }
            }
        }
    });
    writer
}

fn load(scope: Scope) -> (Value, Option<String>) {
    let error = match std::fs::read(platform::state_dir().join(scope.file())) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) if value.is_object() => {
                return (
                    if value.get("messages").is_some() {
                        value
                    } else {
                        json!({"messages":value,"files":{},"attachments":{}})
                    },
                    None,
                );
            }
            _ => Some("下書きファイルを読み込めません".into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(error.to_string()),
    };
    (json!({"messages":{},"files":{},"attachments":{}}), error)
}
