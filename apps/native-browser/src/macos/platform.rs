//! AppKit operations target the selected WKWebView, never the system's active
//! window. Hidden conversations must not type into the PC's page or keep its keyboard.
use agent_protocol::browser::BrowserKey;
use async_channel::{Receiver, bounded};
use objc2::{AnyThread, MainThreadMarker, msg_send, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags, NSEventType, NSImage,
    NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSObjectProtocol, NSPoint, NSProcessInfo, NSRange, NSString,
};
use objc2_web_kit::{WKContentWorld, WKSnapshotConfiguration};
use serde_json::Value;
use wry::WebViewExtMacOS;

pub(super) fn evaluate(
    view: &wry::WebView,
    expression: &str,
    isolated: bool,
) -> Receiver<Result<Value, String>> {
    let (tx, rx) = bounded(1);
    let body = format!(
        "const value = await (0,eval)({}); return JSON.stringify(value === undefined ? null : value);",
        serde_json::to_string(expression).unwrap()
    );
    let block = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
        // SAFETY: WebKit supplies a nullable object valid for this callback;
        // the borrowed reference is used only while parsing the result here.
        let value = unsafe { value.as_ref() };
        let result = error
            .is_null()
            .then_some(value)
            .flatten()
            .and_then(|v| v.downcast_ref::<NSString>())
            .and_then(|v| serde_json::from_str::<Value>(&v.to_string()).ok())
            .ok_or_else(|| {
                "ページの処理に失敗しました。ページ移動中は画面を確認してください。".into()
            });
        let _ = tx.try_send(result);
    });
    let mtm = MainThreadMarker::new().unwrap();
    let world = if isolated {
        unsafe { WKContentWorld::worldWithName(&NSString::from_str("BexBrowser"), mtm) }
    } else {
        unsafe { WKContentWorld::pageWorld(mtm) }
    };
    unsafe {
        view.webview()
            .callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(&body),
                None,
                None,
                &world,
                Some(&block),
            );
    }
    rx
}

pub(super) fn snapshot(view: &wry::WebView, width: u32) -> Receiver<Result<Vec<u8>, String>> {
    let (tx, rx) = bounded(1);
    let block = block2::RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
        // SAFETY: WebKit keeps the nullable image alive for this callback;
        // we finish encoding it before the callback returns.
        let image = unsafe { image.as_ref() };
        let result = image
            .and_then(|image| image.TIFFRepresentation())
            .and_then(|data| NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data))
            .and_then(|rep| {
                // SAFETY: the empty properties dictionary requests the default
                // JPEG encoding and contains no incorrectly typed values.
                unsafe {
                    rep.representationUsingType_properties(
                        NSBitmapImageFileType::JPEG,
                        &NSDictionary::new(),
                    )
                }
            })
            .map(|data| data.to_vec())
            .ok_or_else(|| "ブラウザ画像を取得できません。".into());
        let _ = tx.try_send(result);
    });
    unsafe {
        let config = WKSnapshotConfiguration::new(MainThreadMarker::new().unwrap());
        config.setSnapshotWidth(Some(&NSNumber::new_u32(width)));
        view.webview()
            .takeSnapshotWithConfiguration_completionHandler(Some(&config), &block);
    }
    rx
}

fn input_view(view: Retained<NSView>) -> Result<Retained<NSView>, String> {
    for child in view.subviews().iter() {
        if let Ok(found) = input_view(child) {
            return Ok(found);
        }
    }
    view.respondsToSelector(objc2::sel!(insertText:replacementRange:))
        .then_some(view)
        .ok_or_else(|| "ページの入力先を取得できません。".into())
}

pub(super) fn insert(view: &wry::WebView, text: &str) -> Result<(), String> {
    let input = input_view(view.webview().into_super().into_super())?;
    unsafe {
        let _: () = msg_send![&input, insertText: &*NSString::from_str(text), replacementRange: NSRange::new(usize::MAX, 0)];
    }
    Ok(())
}

fn pointer_event(view: &wry::WebView, x: f64, y: f64, kind: NSEventType) -> Result<(), String> {
    let web = view.webview();
    let window = web.window().ok_or("ブラウザのウィンドウが閉じています。")?;
    let zoom = unsafe { web.pageZoom() };
    let bounds = web.bounds();
    let (x, y) = (x * zoom, y * zoom);
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.
        || y < 0.
        || x >= bounds.size.width
        || y >= bounds.size.height
    {
        return Err("ページの範囲外です。".into());
    }
    let point = web.convertPoint_toView(
        NSPoint::new(
            x,
            if web.isFlipped() {
                y
            } else {
                bounds.size.height - y
            },
        ),
        None,
    );
    let input = input_view(web.clone().into_super().into_super())?;
    if !web.isHidden() {
        window.makeFirstResponder(Some(&input));
    }
    let time = NSProcessInfo::processInfo().systemUptime();
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), time, window.windowNumber(), None, (time * 1e6) as isize, 1, if kind == NSEventType::LeftMouseUp {0.} else {1.}).ok_or("入力イベントを作成できません。")?;
    match kind {
        NSEventType::LeftMouseDown => input.mouseDown(&event),
        NSEventType::LeftMouseUp => input.mouseUp(&event),
        NSEventType::LeftMouseDragged => input.mouseDragged(&event),
        _ => input.mouseMoved(&event),
    }
    Ok(())
}

struct ReleasePointer<'a> {
    input: &'a super::input::InputAck,
    view: &'a wry::WebView,
    point: Option<(f64, f64)>,
}
impl Drop for ReleasePointer<'_> {
    fn drop(&mut self) {
        self.input.release();
        if let Some((x, y)) = self.point {
            let _ = pointer_event(self.view, x, y, NSEventType::LeftMouseUp);
        }
    }
}

pub(super) async fn pointer(
    view: &wry::WebView,
    input: &super::input::InputAck,
    events: &[(f64, f64, NSEventType)],
) -> Result<(), String> {
    input.claim()?;
    let mut release = ReleasePointer {
        view,
        input,
        point: None,
    };
    for &(x, y, kind) in events {
        let down = (kind == NSEventType::LeftMouseDown).then(|| input.arm());
        pointer_event(view, x, y, kind)?;
        if kind == NSEventType::LeftMouseDown {
            release.point = Some((x, y));
        }
        if kind == NSEventType::LeftMouseUp {
            release.point = None;
        }
        if let Some(down) = down {
            down.recv()
                .await
                .map_err(|_| "ページの入力が中断されました。")?;
        }
    }
    Ok(())
}

pub(super) fn key(view: &wry::WebView, key: BrowserKey) -> Result<(), String> {
    let input = input_view(view.webview().into_super().into_super())?;
    if matches!(key, BrowserKey::SelectAll) {
        unsafe {
            let _: () = msg_send![&input, selectAll: std::ptr::null::<AnyObject>()];
        }
        return Ok(());
    }
    let (code, chars) = match key {
        BrowserKey::Enter => (36, "\r"),
        BrowserKey::Tab => (48, "\t"),
        BrowserKey::Backspace => (51, "\u{7f}"),
        BrowserKey::Escape => (53, "\u{1b}"),
        BrowserKey::ArrowUp => (126, "\u{f700}"),
        BrowserKey::ArrowDown => (125, "\u{f701}"),
        BrowserKey::ArrowLeft => (123, "\u{f702}"),
        BrowserKey::ArrowRight => (124, "\u{f703}"),
        BrowserKey::Delete => (117, "\u{f728}"),
        BrowserKey::Home => (115, "\u{f729}"),
        BrowserKey::End => (119, "\u{f72b}"),
        BrowserKey::PageUp => (116, "\u{f72c}"),
        BrowserKey::PageDown => (121, "\u{f72d}"),
        BrowserKey::SelectAll => unreachable!(),
    };
    let window = view
        .webview()
        .window()
        .ok_or("ブラウザのウィンドウが閉じています。")?;
    let text = NSString::from_str(chars);
    let event = |kind| {
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(kind, NSPoint::new(0.,0.), NSEventModifierFlags::empty(), NSProcessInfo::processInfo().systemUptime(), window.windowNumber(), None, &text, &text, false, code).ok_or("キー入力を作成できません。")
    };
    let (down, up) = (event(NSEventType::KeyDown)?, event(NSEventType::KeyUp)?);
    if !super::focus::with_keyboard(&input, || {
        input.keyDown(&down);
        input.keyUp(&up);
    }) {
        return Err("ページにキー入力を送れません。".into());
    }
    Ok(())
}

pub(super) async fn receive<T>(rx: Receiver<Result<T, String>>) -> Result<T, String> {
    rx.recv()
        .await
        .map_err(|_| "ブラウザの処理が中断されました。".to_string())?
}
