//! Quick Look owns the panel, preview controls and window lifecycle.
use nsh_core::JsonValue;
use nsh_platform_macos::{ComponentContext, NativeComponent};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
    rc::{Retained, Weak},
    runtime::ProtocolObject,
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification, NSResponder, NSView, NSWindowDelegate,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{
    NSInteger, NSNotification, NSNotificationCenter, NSObject, NSObjectNSThreadPerformAdditions,
    NSObjectProtocol, NSString, NSURL,
};
use objc2_quick_look_ui::{
    QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate,
};
use std::{
    cell::{Cell, RefCell},
    ptr::NonNull,
};

struct State {
    context: ComponentContext,
    path: RefCell<String>,
    live: Cell<bool>,
    /// Waits for the app to become active, whose key window Quick Look takes
    /// its controller from: a file that arrives while the user is in another
    /// app shows when they come back.
    activation: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>>,
}
define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = State]
    struct PreviewView;
    unsafe impl NSObjectProtocol for PreviewView {}
    impl PreviewView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts(&self, _: &QLPreviewPanel) -> bool { self.ivars().live.get() && !self.ivars().path.borrow().is_empty() }
        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin(&self, panel: &QLPreviewPanel) {
            unsafe {
                panel.setDataSource(Some(ProtocolObject::from_ref(self)));
                panel.setDelegate(Some(self));
            }
        }
        #[unsafe(method(endPreviewPanelControl:))]
        fn end(&self, panel: &QLPreviewPanel) {
            unsafe { panel.setDataSource(None); panel.setDelegate(None); }
            // The shared panel hides instead of closing its NSWindow.
            self.ivars().context.emit("closed", self.ivars().path.borrow().clone().into());
        }
        #[unsafe(method(applyPreviewPath:))]
        fn apply(&self, path: &NSString) {
            if path.to_string() != *self.ivars().path.borrow() { return; }
            if path.is_empty() {
                self.close_panel();
                return;
            }
            if !self.ivars().live.get() { return; }
            if !NSApplication::sharedApplication(self.mtm()).isActive() {
                self.present_on_activation();
                return;
            }
            let Some(window) = self.window() else { return; };
            if !window.makeFirstResponder(Some(self)) {
                self.ivars().context.emit("error", JsonValue::Null);
                return;
            }
            let Some(panel) = (unsafe { QLPreviewPanel::sharedPreviewPanel(self.mtm()) }) else {
                self.ivars().context.emit("error", JsonValue::Null);
                return;
            };
            panel.makeKeyAndOrderFront(None);
            unsafe { panel.updateController(); }
            if !self.controls(&panel) {
                self.ivars().context.emit("error", JsonValue::Null);
                return;
            }
            unsafe { panel.reloadData(); }
        }
    }
    unsafe impl NSWindowDelegate for PreviewView {}
    unsafe impl QLPreviewPanelDelegate for PreviewView {}
    unsafe impl QLPreviewPanelDataSource for PreviewView {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn count(&self, _: Option<&QLPreviewPanel>) -> NSInteger {
            if self.ivars().path.borrow().is_empty() { 0 } else { 1 }
        }
        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn item(&self, _: Option<&QLPreviewPanel>, _: NSInteger) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            Some(ProtocolObject::from_retained(NSURL::fileURLWithPath(&NSString::from_str(&self.ivars().path.borrow()))))
        }
    }
);
impl PreviewView {
    fn controls(&self, panel: &QLPreviewPanel) -> bool {
        unsafe {
            panel.currentController().is_some_and(|controller| {
                std::ptr::eq(Retained::as_ptr(&controller).cast::<Self>(), self)
            })
        }
    }
    fn close_panel(&self) {
        if unsafe { QLPreviewPanel::sharedPreviewPanelExists(self.mtm()) }
            && let Some(panel) = unsafe { QLPreviewPanel::sharedPreviewPanel(self.mtm()) }
            && self.controls(&panel)
        {
            unsafe {
                panel.setDelegate(None);
            }
            panel.close();
        }
    }
    fn present_on_activation(&self) {
        if self.ivars().activation.borrow().is_some() {
            return;
        }
        let this = Weak::from_retained(&self.retain());
        let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
            if let Some(this) = this.load() {
                this.stop_waiting_for_activation();
                let path = this.ivars().path.borrow().clone();
                this.request(&path);
            }
        });
        let observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                &block,
            )
        };
        self.ivars().activation.replace(Some(observer));
    }
    fn stop_waiting_for_activation(&self) {
        if let Some(observer) = self.ivars().activation.take() {
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer.as_ref()) };
        }
    }
    fn request(&self, path: &str) {
        *self.ivars().path.borrow_mut() = path.into();
        // Quick Look can run a nested AppKit loop (notably its text viewer).
        // Present after NSH's mount batch releases the native view tree.
        unsafe {
            self.performSelectorOnMainThread_withObject_waitUntilDone(
                sel!(applyPreviewPath:),
                Some(&NSString::from_str(path)),
                false,
            );
        }
    }
}

pub(super) struct Preview(Retained<PreviewView>);
impl Preview {
    pub(super) fn new(context: ComponentContext) -> Self {
        let this = PreviewView::alloc(MainThreadMarker::new().unwrap()).set_ivars(State {
            context,
            path: RefCell::default(),
            live: Cell::new(true),
            activation: RefCell::default(),
        });
        Self(unsafe { msg_send![super(this), initWithFrame: CGRect::ZERO] })
    }
}
impl NativeComponent for Preview {
    fn view(&self) -> Retained<NSView> {
        self.0.clone().into_super()
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        if name == "path" {
            self.0
                .request(value.and_then(JsonValue::as_str).unwrap_or(""));
        }
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.0.ivars().live.set(false);
        self.0.stop_waiting_for_activation();
        self.0.request("");
    }
}
