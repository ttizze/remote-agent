use super::{Effect, Event, build};
use agent_protocol::browser::BrowserDialog;
use async_channel::Sender;
use block2::{DynBlock, RcBlock};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, NSObject, ProtocolObject},
};
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString, NSURL};
use objc2_web_kit::{
    WKFrameInfo, WKNavigationAction, WKOpenPanelParameters, WKUIDelegate, WKWebView,
    WKWebViewConfiguration, WKWindowFeatures,
};
use std::{cell::RefCell, rc::Rc};
use wry::{
    WebViewExtMacOS,
    raw_window_handle::{AppKitWindowHandle, WindowHandle},
};

enum Completion {
    Alert(RcBlock<dyn Fn()>),
    Confirm(RcBlock<dyn Fn(Bool)>),
    Prompt(RcBlock<dyn Fn(*mut NSString)>),
}
struct Pending {
    message: String,
    completion: Option<Completion>,
}
impl Pending {
    fn finish(&mut self, accept: bool, text: &str) {
        match self.completion.take() {
            Some(Completion::Alert(block)) => block.call(()),
            Some(Completion::Confirm(block)) => block.call((Bool::new(accept),)),
            Some(Completion::Prompt(block)) => {
                let text = accept.then(|| NSString::from_str(text));
                block.call((text.as_ref().map_or(std::ptr::null_mut(), |text| {
                    Retained::as_ptr(text).cast_mut()
                }),));
            }
            None => {}
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.finish(false, "");
    }
}

pub struct Ivars {
    scope: String,
    id: String,
    effects: Rc<RefCell<Vec<Effect>>>,
    changed: Sender<Event>,
    dialog: RefCell<Option<Pending>>,
    dialog_opened: tokio::sync::Notify,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "BexBrowserUIDelegate"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        unsafe fn popup(
            &self,
            web: &WKWebView,
            configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            self.create_popup(web, configuration, action)
        }
        #[unsafe(method(webViewDidClose:))]
        unsafe fn closed(&self, _web: &WKWebView) {
            let i = self.ivars();
            i.effects.borrow_mut().push(Effect::Close {
                scope: i.scope.clone(),
                id: i.id.clone(),
            });
            let _ = i.changed.try_send(Event::Changed);
        }
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn alert(
            &self,
            _web: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            completion: &DynBlock<dyn Fn()>,
        ) {
            self.pending(message, frame, Completion::Alert(completion.copy()));
        }
        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn confirm(
            &self,
            _web: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            completion: &DynBlock<dyn Fn(Bool)>,
        ) {
            self.pending(message, frame, Completion::Confirm(completion.copy()));
        }
        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        unsafe fn prompt(
            &self,
            _web: &WKWebView,
            message: &NSString,
            _default: Option<&NSString>,
            frame: &WKFrameInfo,
            completion: &DynBlock<dyn Fn(*mut NSString)>,
        ) {
            self.pending(message, frame, Completion::Prompt(completion.copy()));
        }
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        unsafe fn files(
            &self,
            web: &WKWebView,
            parameters: &WKOpenPanelParameters,
            _frame: &WKFrameInfo,
            completion: &DynBlock<dyn Fn(*const NSArray<NSURL>)>,
        ) {
            // The OS file picker remains a user action, scoped to the native page.
            if web.isHidden() {
                completion.call((std::ptr::null(),));
                return;
            }
            let panel = NSOpenPanel::openPanel(MainThreadMarker::new().unwrap());
            panel.setCanChooseFiles(true);
            panel.setAllowsMultipleSelection(unsafe { parameters.allowsMultipleSelection() });
            panel.setCanChooseDirectories(unsafe { parameters.allowsDirectories() });
            if panel.runModal() == NSModalResponseOK {
                completion.call((Retained::as_ptr(&panel.URLs()),));
            } else {
                completion.call((std::ptr::null(),));
            }
        }
    }
);
impl Delegate {
    fn create_popup(
        &self,
        web: &WKWebView,
        configuration: &WKWebViewConfiguration,
        action: &WKNavigationAction,
    ) -> Option<Retained<WKWebView>> {
        let url = unsafe { action.request().URL().and_then(|u| u.absoluteString()) }
            .map(|u| u.to_string())
            .unwrap_or_else(|| "about:blank".into());
        if agent_protocol::browser::browser_url(&url).is_err() {
            return None;
        }
        // SAFETY: the native owner retains the parent until shutdown drops its Wry
        // children, which detach themselves. This callback runs on the main
        // thread, so the parent cannot be destroyed while we retain it.
        let parent = unsafe { web.superview() }?;
        let handle = AppKitWindowHandle::new(std::ptr::NonNull::from(&*parent).cast());
        // SAFETY: the retained NSView stays alive through build; the borrowed
        // handle never escapes this callback.
        let handle = unsafe { WindowHandle::borrow_raw(handle.into()) };
        let i = self.ivars();
        let id = uuid::Uuid::new_v4().to_string();
        let (raw, delegate, input) = build(
            &handle,
            &i.scope,
            &id,
            configuration.into(),
            i.effects.clone(),
            i.changed.clone(),
        )
        .ok()?;
        let web = raw.webview().into_super();
        i.effects.borrow_mut().push(Effect::Popup {
            scope: i.scope.clone(),
            id,
            opener: i.id.clone(),
            raw,
            delegate,
            input,
        });
        let _ = i.changed.try_send(Event::Changed);
        Some(web)
    }
    pub fn install(
        raw: &wry::WebView,
        scope: String,
        id: String,
        effects: Rc<RefCell<Vec<Effect>>>,
        changed: Sender<Event>,
    ) -> Retained<Self> {
        let new = MainThreadMarker::new()
            .unwrap()
            .alloc::<Self>()
            .set_ivars(Ivars {
                scope,
                id,
                effects,
                changed,
                dialog: RefCell::new(None),
                dialog_opened: tokio::sync::Notify::new(),
            });
        let delegate: Retained<Self> = unsafe { msg_send![super(new), init] };
        unsafe {
            raw.webview()
                .setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        delegate
    }
    fn pending(&self, message: &NSString, frame: &WKFrameInfo, completion: Completion) {
        let origin = unsafe { frame.request().URL().and_then(|u| u.host()) }
            .map(|v| v.to_string())
            .unwrap_or_default();
        *self.ivars().dialog.borrow_mut() = Some(Pending {
            message: format!("{origin}\n{message}"),
            completion: Some(completion),
        });
        self.ivars().dialog_opened.notify_waiters();
        let _ = self.ivars().changed.try_send(Event::Changed);
    }
    /// A modal pauses JavaScript, including acknowledgements of native input.
    /// Return control to the client so it can answer before awaiting more JS.
    pub async fn until_dialog<T>(&self, work: impl std::future::Future<Output = T>) -> Option<T> {
        let opened = async {
            loop {
                let notified = self.ivars().dialog_opened.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.ivars().dialog.borrow().is_some() {
                    return;
                }
                notified.await;
            }
        };
        tokio::select! {
            biased;
            () = opened => None,
            result = work => Some(result),
        }
    }
    pub fn dialog(&self) -> Option<BrowserDialog> {
        self.ivars()
            .dialog
            .borrow()
            .as_ref()
            .map(|d| BrowserDialog {
                message: d.message.clone(),
                prompt: matches!(d.completion, Some(Completion::Prompt(_))),
            })
    }
    pub fn respond(&self, accept: bool, text: &str) -> Result<(), String> {
        let mut pending = self
            .ivars()
            .dialog
            .borrow_mut()
            .take()
            .ok_or("ダイアログは既に閉じています。")?;
        pending.finish(accept, text);
        Ok(())
    }
}
