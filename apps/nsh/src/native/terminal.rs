//! SwiftTerm owns rendering and native input; the native component only moves bytes.
use crate::l10n::tr;
use nsh_core::JsonValue;
use objc2::rc::Retained;
use serde_json::json;
use std::{ffi::c_void, ptr::NonNull, rc::Rc, sync::OnceLock};

#[cfg(target_os = "ios")]
use nsh_platform_ios::{ComponentContext, Components, NativeComponent};
#[cfg(target_os = "macos")]
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView as PlatformView;
#[cfg(target_os = "ios")]
use objc2_ui_kit::UIView as PlatformView;

type Callback = unsafe extern "C" fn(*mut c_void, i32, *const u8, isize, i32, i32);

struct Bridge {
    create: unsafe extern "C" fn(*mut c_void, Callback) -> *mut c_void,
    view: unsafe extern "C" fn(*mut c_void) -> *mut PlatformView,
    input: unsafe extern "C" fn(*mut c_void, bool),
    #[cfg(target_os = "macos")]
    menu: unsafe extern "C" fn(*mut c_void, *const u8, isize),
    feed: unsafe extern "C" fn(*mut c_void, *const u8, isize, i32, i32),
    release: unsafe extern "C" fn(*mut c_void),
}

static BRIDGE: OnceLock<Result<Bridge, String>> = OnceLock::new();

impl Bridge {
    fn load() -> Result<Self, String> {
        // The declarations match the framework's C exports.
        unsafe {
            Ok(Self {
                create: super::apple::symbol(b"bex_nsh_terminal_create\0")?,
                view: super::apple::symbol(b"bex_nsh_terminal_view\0")?,
                input: super::apple::symbol(b"bex_nsh_terminal_input\0")?,
                #[cfg(target_os = "macos")]
                menu: super::apple::symbol(b"bex_nsh_terminal_menu\0")?,
                feed: super::apple::symbol(b"bex_nsh_terminal_feed\0")?,
                release: super::apple::symbol(b"bex_nsh_terminal_release\0")?,
            })
        }
    }
}

struct ForeignTerminal {
    bridge: &'static Bridge,
    handle: NonNull<c_void>,
    context: Box<ComponentContext>,
    sequence: u64,
}

impl ForeignTerminal {
    fn new(context: ComponentContext) -> Result<Self, String> {
        let bridge = BRIDGE
            .get_or_init(Bridge::load)
            .as_ref()
            .map_err(Clone::clone)?;
        let mut context = Box::new(context);
        // The box's address remains stable until release has disconnected the delegate.
        let handle =
            unsafe { (bridge.create)((&mut *context as *mut ComponentContext).cast(), event) };
        let handle = NonNull::new(handle)
            .ok_or_else(|| tr("ターミナルを作成できませんでした。").to_owned())?;
        Ok(Self {
            bridge,
            handle,
            context,
            sequence: 0,
        })
    }

    fn output(&mut self, chunks: &[agent_core::state::TerminalOutput]) {
        let before = self.sequence;
        for chunk in chunks.iter().filter(|chunk| chunk.sequence > before) {
            let size = chunk.reset_size;
            // Vec/slice lengths fit isize. Swift copies the bytes before this call returns.
            unsafe {
                (self.bridge.feed)(
                    self.handle.as_ptr(),
                    chunk.data.as_ptr(),
                    chunk.data.len() as isize,
                    size.map_or(0, |size| i32::from(size.cols)),
                    size.map_or(0, |size| i32::from(size.rows)),
                );
            }
            self.sequence = chunk.sequence;
        }
        if self.sequence > before {
            self.context.emit("ack", json!(self.sequence));
        }
    }
}

impl Drop for ForeignTerminal {
    fn drop(&mut self) {
        // Swift clears its delegate/callback before the context box can be dropped.
        unsafe {
            (self.bridge.release)(self.handle.as_ptr());
        }
    }
}

unsafe extern "C" fn event(
    context: *mut c_void,
    kind: i32,
    bytes: *const u8,
    count: isize,
    cols: i32,
    rows: i32,
) {
    // Swift calls synchronously on the main actor while ForeignTerminal retains this box.
    let context = unsafe { &*context.cast::<ComponentContext>() };
    match kind {
        1 if count > 0 && !bytes.is_null() => {
            let bytes = unsafe { std::slice::from_raw_parts(bytes, count as usize) };
            context.emit("input", json!(bytes));
        }
        2 => context.emit("resize", json!({"cols": cols, "rows": rows})),
        3 => context.emit("focus", json!(cols != 0)),
        #[cfg(target_os = "macos")]
        4 if count > 0 && !bytes.is_null() => {
            let bytes = unsafe { std::slice::from_raw_parts(bytes, count as usize) };
            if let Ok(text) = std::str::from_utf8(bytes) {
                context.emit("quote", json!(text));
            }
        }
        _ => {}
    }
}

struct Terminal {
    view: Retained<PlatformView>,
    native: Option<ForeignTerminal>,
}

impl Terminal {
    fn new(context: ComponentContext) -> Self {
        context.set_size(0.0, 0.0);
        match ForeignTerminal::new(context) {
            Ok(native) => {
                // Swift retains its native view until native.release; NSH also needs its own retain.
                let view =
                    unsafe { Retained::retain((native.bridge.view)(native.handle.as_ptr())) }
                        .expect("the Swift terminal owner always has a native view");
                // Its value is the screen's text (Terminal.swift).
                #[cfg(target_os = "macos")]
                objc2_app_kit::NSAccessibility::setAccessibilityLabel(
                    &*view,
                    Some(&objc2_foundation::NSString::from_str(tr("ターミナル"))),
                );
                Self {
                    view,
                    native: Some(native),
                }
            }
            Err(reason) => Self {
                view: super::apple::failure(&reason),
                native: None,
            },
        }
    }
}

impl NativeComponent for Terminal {
    #[cfg(target_os = "macos")]
    fn view(&self) -> Retained<PlatformView> {
        self.view.clone()
    }

    #[cfg(target_os = "ios")]
    fn view(&self) -> &PlatformView {
        &self.view
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let Some(native) = &mut self.native else {
            return;
        };
        match name {
            "input" => unsafe {
                (native.bridge.input)(
                    native.handle.as_ptr(),
                    value.and_then(JsonValue::as_bool).unwrap_or(false),
                );
            },
            "output" => {
                if let Some(chunks) = value.and_then(|v| {
                    serde_json::from_value::<Vec<agent_core::state::TerminalOutput>>(v.clone()).ok()
                }) {
                    native.output(&chunks);
                }
            }
            #[cfg(target_os = "macos")]
            "menu" => {
                if let Some(bytes) = value.and_then(|v| serde_json::to_vec(v).ok()) {
                    unsafe {
                        (native.bridge.menu)(
                            native.handle.as_ptr(),
                            bytes.as_ptr(),
                            bytes.len() as isize,
                        )
                    };
                }
            }
            _ => {}
        }
    }
}

pub(super) fn register(components: &Components) {
    components.register(
        "bex.terminal",
        Rc::new(|context| Box::new(Terminal::new(context))),
    );
}
