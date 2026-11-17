//! WebKit's shared owner outlives individual NSH tool surfaces.
use crate::application::Application;
use native_browser::{Browser, Startup};
use nsh_core::JsonValue;
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{NSAccessibility, NSAccessibilityGroupRole, NSView};
use objc2_foundation::{NSObjectProtocol, NSString};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};
use tokio_util::task::TaskTracker;

pub(crate) struct Service {
    application: Arc<Application>,
    runtime: tokio::runtime::Handle,
    directory: Option<PathBuf>,
    closing: TaskTracker,
    startup: RefCell<Option<Startup>>,
    browser: RefCell<Option<Browser>>,
}

impl Service {
    pub(crate) fn new(
        application: Arc<Application>,
        runtime: tokio::runtime::Handle,
        directory: Option<PathBuf>,
    ) -> Rc<Self> {
        Rc::new(Self {
            application,
            runtime,
            directory,
            closing: TaskTracker::new(),
            startup: RefCell::new(None),
            browser: RefCell::new(None),
        })
    }

    fn start(self: &Rc<Self>, parent: &NSView) {
        if self.startup.borrow().is_some() || self.closing.is_closed() {
            return;
        }
        let Some(directory) = self.directory.clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        self.startup.replace(Some(Browser::start(
            parent,
            directory,
            self.runtime.clone(),
            self.closing.clone(),
            move |result| {
                let Some(service) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(browser) => {
                        service
                            .application
                            .set_browser_renderer(browser.renderer_id());
                        service.browser.replace(Some(browser));
                    }
                    // Another desktop may already own these pages. Host RPCs keep
                    // routing to that owner; never replace its registration or state.
                    Err(reason) => eprintln!("{reason}"),
                }
            },
        )));
    }

    /// Background pages live in this container once it is in a window.
    fn container(self: &Rc<Self>) -> Retained<NSView> {
        let mtm = MainThreadMarker::new().unwrap();
        let container = NSView::new(mtm);
        let view = MountObserver::alloc(mtm).set_ivars(self.clone());
        let view: Retained<MountObserver> = unsafe { msg_send![super(view), init] };
        container.addSubview(&view);
        container
    }

    pub(crate) fn shutdown(&self) -> TaskTracker {
        self.startup.take();
        if let Some(browser) = self.browser.take() {
            browser.shutdown();
        }
        self.application.set_browser_renderer(String::new());
        self.closing.close();
        self.closing.clone()
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<Service>]
    struct MountObserver;
    unsafe impl NSObjectProtocol for MountObserver {}
    impl MountObserver {
        #[unsafe(method(viewDidMoveToWindow))]
        fn moved(&self) {
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
            if self.window().is_some() && let Some(parent) = unsafe { self.superview() } {
                self.ivars().start(&parent);
            }
        }
    }
);

struct Selection {
    scope: String,
    renderer: String,
    tab: String,
}
struct SurfaceState {
    service: Rc<Service>,
    selection: RefCell<Selection>,
    context: ComponentContext,
    size: Cell<(f64, f64)>,
}
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SurfaceState]
    struct Surface;
    unsafe impl NSObjectProtocol for Surface {}
    impl Surface {
        #[unsafe(method(layout))]
        fn layout_native(&self) {
            unsafe { let _: () = msg_send![super(self), layout]; }
            self.mount();
        }
        #[unsafe(method(viewDidMoveToWindow))]
        fn moved(&self) {
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
            self.mount();
        }
    }
);
impl Surface {
    fn mount(&self) {
        let bounds = self.bounds();
        let size = (bounds.size.width, bounds.size.height);
        if self.ivars().size.replace(size) != size {
            self.ivars().context.emit(
                "size",
                serde_json::json!({"width": size.0, "height": size.1}),
            );
        }
        let selection = self.ivars().selection.borrow();
        if let Some(browser) = self.ivars().service.browser.borrow().as_ref() {
            if self.window().is_some()
                && browser.has_surface(&selection.scope, &selection.renderer, &selection.tab)
            {
                browser.layout(&selection.scope, &selection.tab, self, self.bounds());
            } else {
                browser.unmount(self);
            }
        }
    }
}
struct NativeSurface(Retained<Surface>);
impl NativeSurface {
    fn new(context: ComponentContext, service: Rc<Service>) -> Self {
        context.set_size(0., 0.);
        let view = Surface::alloc(MainThreadMarker::new().unwrap()).set_ivars(SurfaceState {
            service,
            context,
            size: Cell::new((0., 0.)),
            selection: RefCell::new(Selection {
                scope: String::new(),
                renderer: String::new(),
                tab: String::new(),
            }),
        });
        let view: Retained<Surface> = unsafe { msg_send![super(view), init] };
        // The page WebKit shows inside it is the browser, as Windows labels
        // the page it draws.
        view.setAccessibilityElement(true);
        view.setAccessibilityRole(Some(unsafe { NSAccessibilityGroupRole }));
        view.setAccessibilityLabel(Some(&NSString::from_str(crate::l10n::tr("ブラウザ"))));
        Self(view)
    }
}
impl Drop for NativeSurface {
    fn drop(&mut self) {
        if let Some(browser) = self.0.ivars().service.browser.borrow().as_ref() {
            browser.unmount(&self.0);
        }
    }
}
impl NativeComponent for NativeSurface {
    fn view(&self) -> Retained<NSView> {
        self.0.clone().into_super()
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        {
            let mut selection = self.0.ivars().selection.borrow_mut();
            let value = value
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_owned();
            match name {
                "scope" => selection.scope = value,
                "renderer" => selection.renderer = value,
                "tab" => selection.tab = value,
                _ => return,
            }
        }
        self.0.mount();
    }
}

struct Host(Retained<NSView>);
impl NativeComponent for Host {
    fn view(&self) -> Retained<NSView> {
        self.0.clone()
    }
    fn set_prop(&mut self, _: &str, _: Option<&JsonValue>) {}
}

pub(crate) fn register(components: &Components, service: Rc<Service>) {
    let host = service.clone();
    components.register(
        "bex.browser.host",
        Rc::new(move |_| Box::new(Host(host.container()))),
    );
    components.register(
        "bex.browser.native",
        Rc::new(move |context| Box::new(NativeSurface::new(context, service.clone()))),
    );
}
