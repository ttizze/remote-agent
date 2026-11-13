//! Own native Swift views and their callback context across the C ABI.
use crate::l10n::tr;
use nsh_core::JsonValue;
use objc2::rc::Retained;
use std::{ffi::c_void, ptr::NonNull, rc::Rc};

#[cfg(target_os = "ios")]
use nsh_platform_ios::{ComponentContext, Components, NativeComponent};
#[cfg(target_os = "macos")]
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView as PlatformView;
#[cfg(target_os = "ios")]
use objc2_ui_kit::UIView as PlatformView;

type Event = unsafe extern "C" fn(*mut c_void, *const u8, isize);
struct Bridge {
    create: unsafe extern "C" fn(*mut c_void, Event) -> *mut c_void,
    view: unsafe extern "C" fn(*mut c_void) -> *mut PlatformView,
    update: unsafe extern "C" fn(*mut c_void, *const u8, isize),
    release: unsafe extern "C" fn(*mut c_void),
    command: Option<unsafe extern "C" fn(*mut c_void, *const u8, isize)>,
}
impl Bridge {
    fn load(prefix: &str) -> Result<Self, String> {
        unsafe {
            Ok(Self {
                create: super::apple::symbol(format!("{prefix}_create\0").as_bytes())?,
                view: super::apple::symbol(format!("{prefix}_view\0").as_bytes())?,
                update: super::apple::symbol(format!("{prefix}_update\0").as_bytes())?,
                release: super::apple::symbol(format!("{prefix}_release\0").as_bytes())?,
                command: super::apple::symbol(format!("{prefix}_command\0").as_bytes()).ok(),
            })
        }
    }
}
struct Resource {
    bridge: Bridge,
    handle: NonNull<c_void>,
    _context: Box<ComponentContext>,
}
impl Drop for Resource {
    fn drop(&mut self) {
        // Swift disconnects callbacks before the retained context can be dropped.
        unsafe { (self.bridge.release)(self.handle.as_ptr()) };
    }
}
unsafe extern "C" fn event(context: *mut c_void, bytes: *const u8, count: isize) {
    if count <= 0 || bytes.is_null() {
        return;
    }
    let context = unsafe { &*context.cast::<ComponentContext>() };
    let bytes = unsafe { std::slice::from_raw_parts(bytes, count as usize) };
    if let Ok(value) = serde_json::from_slice::<JsonValue>(bytes) {
        context.emit("action", value);
    }
}
pub(super) struct Hosted {
    view: Retained<PlatformView>,
    resource: Option<Resource>,
}
impl Hosted {
    pub(super) fn new(context: ComponentContext, prefix: &str) -> Self {
        context.set_size(0., 0.);
        let create = || {
            let bridge = Bridge::load(prefix)?;
            let mut context = Box::new(context);
            let handle = NonNull::new(unsafe {
                (bridge.create)((&mut *context as *mut ComponentContext).cast(), event)
            })
            .ok_or_else(|| tr("表示を作成できませんでした。").to_owned())?;
            let resource = Resource {
                bridge,
                handle,
                _context: context,
            };
            // Swift owns the view for the entire retained resource lifetime.
            let view = unsafe { Retained::retain((resource.bridge.view)(handle.as_ptr())) }
                .expect("a Swift owner always has a native view");
            Ok::<_, String>((resource, view))
        };
        match create() {
            Ok((resource, view)) => Self {
                view,
                resource: Some(resource),
            },
            Err(reason) => Self {
                view: super::apple::failure(&reason),
                resource: None,
            },
        }
    }
}
impl NativeComponent for Hosted {
    #[cfg(target_os = "macos")]
    fn view(&self) -> Retained<PlatformView> {
        self.view.clone()
    }
    #[cfg(target_os = "ios")]
    fn view(&self) -> &PlatformView {
        &self.view
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        if name == "data"
            && let Some(resource) = &self.resource
            && let Some(value) = value
            && let Ok(data) = serde_json::to_vec(value)
        {
            // Swift decodes synchronously and retains no borrowed bytes.
            unsafe {
                (resource.bridge.update)(
                    resource.handle.as_ptr(),
                    data.as_ptr(),
                    data.len() as isize,
                );
            }
        }
    }
    fn command(&mut self, name: &str, args: &JsonValue) {
        if let Some(resource) = &self.resource
            && let Some(command) = resource.bridge.command
            && let Ok(data) = serde_json::to_vec(&serde_json::json!({"name": name, "args": args}))
        {
            unsafe { command(resource.handle.as_ptr(), data.as_ptr(), data.len() as isize) };
        }
    }
}
pub(super) fn register(components: &Components, name: &'static str, prefix: &'static str) {
    components.register(
        name,
        Rc::new(move |context| Box::new(Hosted::new(context, prefix))),
    );
}
