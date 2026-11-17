//! AppKit needs the process main thread, so this native boundary test has its
//! own harness. It uses an invisible window and never starts the application.
#[cfg(target_os = "macos")]
mod appkit {
    use native_focus as focus;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained};
    use objc2_app_kit::{NSApplication, NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

    define_class!(
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        struct InputView;
        unsafe impl NSObjectProtocol for InputView {}
        impl InputView {
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }
        }
    );

    impl InputView {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            // SAFETY: NSView's initializer accepts this main-thread subclass.
            unsafe { msg_send![super(mtm.alloc::<Self>().set_ivars(())), init] }
        }
    }

    define_class!(
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        struct RefusingView;
        unsafe impl NSObjectProtocol for RefusingView {}
        impl RefusingView {
            #[unsafe(method(becomeFirstResponder))]
            fn become_first_responder(&self) -> bool {
                false
            }
        }
    );

    impl RefusingView {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            // SAFETY: NSView's initializer accepts this main-thread subclass.
            unsafe { msg_send![super(mtm.alloc::<Self>().set_ivars(())), init] }
        }
    }

    pub(super) fn run() {
        let mtm = MainThreadMarker::new().expect("AppKit test runs on the main thread");
        let _app = NSApplication::sharedApplication(mtm);
        // SAFETY: all arguments are valid AppKit values; the window stays hidden.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(200., 200.)),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        let parent = InputView::new(mtm);
        let page = InputView::new(mtm);
        let field = InputView::new(mtm);
        let other = InputView::new(mtm);
        assert!(!focus::has_focus(&page));
        focus::release(&page);
        window.setContentView(Some(&parent));
        parent.addSubview(&page);
        parent.addSubview(&other);
        page.addSubview(&field);

        // Both a WebView itself and a focused content descendant return input.
        for responder in [&*page, &*field] {
            assert!(window.makeFirstResponder(Some(responder)));
            assert!(focus::has_focus(&page));
            focus::release(&page);
            assert!(focus::has_focus(&parent));
            assert!(!focus::has_focus(&page));
        }
        assert!(window.makeFirstResponder(Some(&field)));
        focus::hide(&page);
        assert!(page.isHidden());
        assert!(!focus::has_focus(&page));
        assert!(window.firstResponder().unwrap().isEqual(Some(&parent)));

        // Repeated hides and clicks must preserve an unrelated field's focus.
        assert!(window.makeFirstResponder(Some(&other)));
        focus::hide(&page);
        focus::release(&page);
        assert!(window.firstResponder().unwrap().isEqual(Some(&other)));

        // A native layout container need not accept keyboard focus. Hiding
        // its child must still stop that hidden child receiving keystrokes.
        let container = RefusingView::new(mtm);
        parent.addSubview(&container);
        container.addSubview(&page);
        page.setHidden(false);
        assert!(window.makeFirstResponder(Some(&field)));
        focus::hide(&page);
        assert!(!focus::has_focus(&page));
        parent.addSubview(&page);
        assert!(window.makeFirstResponder(Some(&other)));

        // Remote keys reach a hidden page's input and leave the field focused.
        let mut dispatched = false;
        assert!(focus::with_keyboard(&field, || {
            dispatched = window.firstResponder().unwrap().isEqual(Some(&field));
        }));
        assert!(dispatched);
        assert!(window.firstResponder().unwrap().isEqual(Some(&other)));
        let refusing = RefusingView::new(mtm);
        page.addSubview(&refusing);
        for unavailable in [&**refusing, &InputView::new(mtm)] {
            assert!(!focus::with_keyboard(unavailable, || {
                panic!("keys must not reach a refusing or detached view")
            }));
        }
        assert!(window.firstResponder().unwrap().isEqual(Some(&other)));
        page.setHidden(false);
        assert!(window.makeFirstResponder(Some(&field)));
        assert!(focus::has_focus(&page));
        focus::release(&page);
        assert!(window.firstResponder().unwrap().isEqual(Some(&parent)));
        println!("Native browser focus returns to the parent view and preserves other fields");
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    appkit::run();
}
