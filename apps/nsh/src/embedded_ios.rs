//! Embed only the conversation in the existing UIKit/SwiftUI application.
use crate::{Area, application::Application, client::BorrowedSession};
use agent_core::client::{native::AgentPane, session::ClientSession};
use nsh_platform_ios::{AppView, SurfaceOptions};
use objc2::MainThreadMarker;
use objc2_core_foundation::CGRect;
use objc2_ui_kit::UIView;
use serde_json::json;
use std::{
    ffi::{CStr, c_char, c_void},
    ptr,
    sync::Arc,
};
use uniffi::{FfiConverterArc, ffi::Handle};

type Callback = unsafe extern "C" fn(*mut c_void, *const u8, isize);

#[derive(Clone, Copy)]
struct Events {
    context: usize,
    callback: Callback,
}
impl Events {
    fn emit(self, value: serde_json::Value) {
        if let Ok(bytes) = serde_json::to_vec(&value) {
            // The Swift view owns this context until the engine has stopped.
            unsafe {
                (self.callback)(
                    self.context as *mut c_void,
                    bytes.as_ptr(),
                    bytes.len() as isize,
                )
            };
        }
    }
}

struct Conversation {
    view: Option<AppView>,
    application: Arc<Application>,
    runtime: tokio::runtime::Runtime,
}
impl Drop for Conversation {
    fn drop(&mut self) {
        // Stop callbacks before releasing the Swift context or its shared session.
        drop(self.view.take());
        let _ = self.runtime.block_on(self.application.shutdown());
    }
}

/// Consumes cloned UniFFI handles, never the Swift screen's original references.
///
/// # Safety
/// Call on the main thread. `session` and a nonzero `pane` are handles cloned
/// for this call, `host` is a NUL-terminated string borrowed for it, and
/// `callback` may be called with `context` until the returned conversation is
/// released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bex_nsh_conversation_create(
    session: u64,
    pane: u64,
    host: *const c_char,
    context: *mut c_void,
    callback: Callback,
) -> *mut c_void {
    let events = Events {
        context: context as usize,
        callback,
    };
    let create = || -> Result<Conversation, String> {
        let session = <ClientSession as FfiConverterArc<agent_core::UniFfiTag>>::try_lift(
            Handle::from_raw_unchecked(session),
        )
        .map_err(|e| e.to_string())?;
        let pane = if pane == 0 {
            None
        } else {
            Some(
                <AgentPane as FfiConverterArc<agent_core::UniFfiTag>>::try_lift(
                    Handle::from_raw_unchecked(pane),
                )
                .map_err(|e| e.to_string())?,
            )
        };
        let area = Area::Conversation {
            pane: pane.as_ref().map(|_| 1),
        };
        let host = unsafe { CStr::from_ptr(host) }
            .to_str()
            .map_err(|e| e.to_string())?
            .to_owned();
        let root = crate::storage::directory().map_err(|e| e.to_string())?;
        let (runtime, application) = Application::embedded(
            root,
            BorrowedSession {
                host,
                session,
                pane,
            },
        )
        .map_err(|e| e.to_string())?;
        let options = SurfaceOptions::default();
        let view = AppView::new(
            MainThreadMarker::new().expect("UIKit main thread"),
            CGRect::ZERO,
            options,
        );
        view.set_error_handler(move |error| events.emit(json!({"action":"error", "text":error})));
        view.run(crate::root::root(
            application.clone(),
            area,
            Some(Arc::new(move |event| events.emit(event))),
        ));
        Ok(Conversation {
            view: Some(view),
            runtime,
            application,
        })
    };
    match create() {
        Ok(view) => Box::into_raw(Box::new(view)).cast(),
        Err(reason) => {
            events.emit(json!({"action":"error", "text":reason}));
            ptr::null_mut()
        }
    }
}

/// # Safety
/// Call on the main thread with a conversation `bex_nsh_conversation_create`
/// returned and has not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bex_nsh_conversation_view(handle: *mut c_void) -> *mut UIView {
    let owner = unsafe { &*handle.cast::<Conversation>() };
    owner
        .view
        .as_ref()
        .expect("a live conversation owns its view")
        .view() as *const UIView as *mut UIView
}

/// # Safety
/// Call once, on the main thread, with a conversation
/// `bex_nsh_conversation_create` returned; it is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bex_nsh_conversation_release(handle: *mut c_void) {
    drop(unsafe { Box::from_raw(handle.cast::<Conversation>()) });
}
