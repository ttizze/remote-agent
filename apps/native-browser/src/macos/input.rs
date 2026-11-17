//! WebKit may defer native mouse-down hit testing after switching a hidden tab.
//! A private content world acknowledges the trusted event before its release.
//! Website JavaScript has neither this handler nor a native execution API.
use async_channel::{Receiver, Sender};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, ProtocolObject},
};
use objc2_foundation::{NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKContentWorld, WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime, WKWebViewConfiguration,
};
use std::cell::{Cell, RefCell};

pub(super) struct Ivars {
    busy: Cell<bool>,
    down: RefCell<Option<Sender<()>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "BexBrowserInputAck"]
    #[thread_kind=MainThreadOnly]
    #[ivars=Ivars]
    pub(super) struct InputAck;
    unsafe impl NSObjectProtocol for InputAck {}
    unsafe impl WKScriptMessageHandler for InputAck {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn received(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            if unsafe { message.body() }
                .downcast_ref::<NSString>()
                .is_some_and(|s| s.to_string() == "down")
                && let Some(sender) = self.ivars().down.borrow_mut().take()
            {
                let _ = sender.try_send(());
            }
        }
    }
);
impl InputAck {
    pub(super) fn install(configuration: &WKWebViewConfiguration) -> Retained<Self> {
        let mtm = MainThreadMarker::new().unwrap();
        let new = mtm.alloc::<Self>().set_ivars(Ivars {
            busy: Cell::new(false),
            down: RefCell::new(None),
        });
        let handler: Retained<Self> = unsafe { msg_send![super(new), init] };
        unsafe {
            let controller = WKUserContentController::new(mtm);
            let world = WKContentWorld::worldWithName(&NSString::from_str("BexBrowser"), mtm);
            controller.addScriptMessageHandler_contentWorld_name(
                ProtocolObject::from_ref(&*handler),
                &world,
                &NSString::from_str("bexInput"),
            );
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                mtm.alloc(),
                &NSString::from_str(
                    "window.addEventListener('mousedown', event => { if (event.isTrusted) window.webkit.messageHandlers.bexInput.postMessage('down'); }, true);",
                ),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
                &world,
            );
            controller.addUserScript(&script);
            configuration.setUserContentController(&controller);
        }
        handler
    }
    pub(super) fn claim(&self) -> Result<(), String> {
        if self.ivars().busy.replace(true) {
            return Err("このタブの入力処理中です。画面を確認してください。".into());
        }
        Ok(())
    }
    pub(super) fn arm(&self) -> Receiver<()> {
        let (tx, rx) = async_channel::bounded(1);
        *self.ivars().down.borrow_mut() = Some(tx);
        rx
    }
    pub(super) fn release(&self) {
        self.ivars().down.borrow_mut().take();
        self.ivars().busy.set(false);
    }
}
