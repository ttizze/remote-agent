//! NSH's native-component boundary. Standard OS controls own their drawing,
//! intrinsic sizes, accessibility, menus, search and input behavior.
use crate::l10n::{localized, tr};
use nsh_core::{element::NativeView, prelude::*};
use serde_json::{Value, json};

/// The embedding screen owns the presentation of selected text and links.
#[derive(Clone, Copy)]
pub(crate) struct ExternalActions(pub Stored<std::sync::Arc<dyn Fn(Value) + Send + Sync>>);

impl ExternalActions {
    pub fn emit(self, event: Value) {
        self.0.with(|send| send(event));
    }
}

#[derive(Clone, PartialEq, serde::Serialize)]
pub(crate) struct RowIndicators {
    pub activity: Option<agent_core::presentation::thread_status::ThreadActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<agent_core::presentation::thread_status::ThreadAttention>,
    /// Its worktree has changes its source branch lacks.
    pub changes: bool,
}

impl RowIndicators {
    pub(crate) fn label(&self) -> String {
        use agent_core::presentation::thread_status::{
            thread_activity_label, thread_attention_label,
        };
        // A badge says what a waiting chat waits for.
        self.attention
            .map(|attention| localized(thread_attention_label(attention)))
            .or_else(|| {
                self.activity
                    .map(|activity| localized(thread_activity_label(activity)))
            })
            .into_iter()
            .chain(self.changes.then(|| tr("作成元からの変更あり").to_owned()))
            .collect::<Vec<_>>()
            .join(tr("、"))
    }
}

/// Whether the app keeps its work going now (its connection, a recording). An iPhone app
/// does only in the foreground, where the system lets it run; a desktop app
/// does all the time, since the user works in other apps meanwhile.
pub(crate) fn in_use() -> bool {
    cfg!(desktop) || lifecycle() == nsh_core::platform::Lifecycle::Active
}

/// Whether the user can see the app's content: on the desktop while any of its
/// window shows, on the iPhone only in the foreground.
pub(crate) fn shown() -> bool {
    use nsh_core::platform::Lifecycle;
    let lifecycle = lifecycle();
    lifecycle == Lifecycle::Active || (cfg!(desktop) && lifecycle == Lifecycle::Inactive)
}

pub fn items(choices: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> Value {
    json!(
        choices
            .into_iter()
            .map(|(id, title)| json!({"id": id.into(), "title": title.into()}))
            .collect::<Vec<_>>()
    )
}

pub fn choices(titles: &[&str]) -> Value {
    items(titles.iter().map(|title| (*title, *title)))
}

#[cfg(target_os = "macos")]
pub(crate) mod browser_macos;
#[cfg(target_os = "ios")]
mod document_picker;
#[cfg(target_os = "ios")]
pub mod ios;
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod hosted;
#[cfg(target_os = "ios")]
mod preview_ios;
#[cfg(target_os = "macos")]
mod preview_macos;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod terminal;
#[cfg(windows)]
pub(crate) mod windows;
#[cfg(target_os = "macos")]
mod zoom_macos;

/// A scroll over a [`wheel_area`], in its points: `dy` toward the content's
/// end (down), as a scroll view would move.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Scroll {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// A transparent view over drawn content (a Host's page, the terminal's
/// grid) that reports each wheel or touchpad scroll over it; its own presses
/// and moves stay its view's. Windows' component reports them.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) fn wheel_area(scrolled: impl Fn(Scroll) + 'static) -> NativeView {
    native("bex.wheel").on_native_event(move |event: &NativeEvent| {
        if &*event.name == "wheel"
            && let Ok(scroll) = serde_json::from_value::<Scroll>(event.data.clone())
        {
            scrolled(scroll);
        }
    })
}

/// A transparent view over the image viewer's image that reports the zooms
/// by hand over it: a pinch, and a wheel with Ctrl (⌘ on the Mac) held. Its
/// other wheels scroll the image as they would without it. Windows' and the
/// Mac's components report them (`windows.rs`, `zoom_macos.rs`).
#[cfg(desktop)]
pub(crate) fn zoom_area(
    zoomed: impl Fn(agent_core::presentation::media::ZoomGesture) + 'static,
) -> NativeView {
    native("bex.zoom").on_native_event(move |event: &NativeEvent| {
        if &*event.name == "zoom"
            && let Ok(gesture) = serde_json::from_value(event.data.clone())
        {
            zoomed(gesture);
        }
    })
}

pub fn image_extension(mime: &str) -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use objc2_foundation::NSString;
        use objc2_uniform_type_identifiers::UTType;
        UTType::typeWithMIMEType(&NSString::from_str(mime))?
            .preferredFilenameExtension()
            .map(|value| value.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    agent_core::presentation::files::image_extension(mime).map(str::to_owned)
}

pub fn image_file(path: &std::path::Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use objc2_foundation::NSString;
        use objc2_uniform_type_identifiers::UTType;
        let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) else {
            return false;
        };
        UTType::typeWithFilenameExtension(&NSString::from_str(extension))
            .zip(UTType::typeWithIdentifier(&NSString::from_str(
                "public.image",
            )))
            .is_some_and(|(file, image)| file.conformsToType(&image))
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    path.to_str()
        .is_some_and(agent_core::presentation::files::is_image_file)
}

pub fn button(symbol: &'static str, label: &'static str, press: impl Fn() + 'static) -> NativeView {
    native("bex.button")
        .prop("symbol", json!(symbol))
        .prop("label", json!(label))
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "press" {
                press();
            }
        })
}

pub fn menu(
    symbol: &'static str,
    label: &'static str,
    items: impl IntoReactive<Value>,
    select: impl Fn(&str) + 'static,
) -> NativeView {
    native("bex.menu")
        .prop("symbol", json!(symbol))
        .prop("label", json!(label))
        .prop("items", items)
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "select"
                && let Some(title) = event.data.as_str()
            {
                select(title);
            }
        })
}

pub fn text_button(title: impl IntoReactive<Value>, press: impl Fn() + 'static) -> NativeView {
    native("bex.button")
        .prop("title", title)
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "press" {
                press();
            }
        })
}

pub fn toolbar(title: Memo<String>, action: impl Fn(&str) + 'static) -> NativeView {
    native("bex.toolbar")
        .prop("title", move || json!(title.get()))
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "select"
                && let Some(id) = event.data.as_str()
            {
                action(id);
            }
        })
}

pub fn message(title: impl Into<String>, description: impl Into<String>) {
    let title = title.into();
    // Callers pass their own copy or a reason Core composed.
    let description = localized(description.into());
    spawn(async move {
        let _ = nsh_base::alert_dialog()
            .title(title)
            .description(description)
            .action("close", tr("閉じる"))
            .show()
            .await;
    });
}

pub fn open_link(url: impl Into<String>) {
    let url = url.into();
    spawn(async move {
        if !open_url(url).await {
            message(
                tr("リンクを開けませんでした"),
                tr("開くアプリを確認してください。"),
            );
        }
    });
}

pub fn date_time(seconds: i64) -> String {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle};
        NSDateFormatter::localizedStringFromDate_dateStyle_timeStyle(
            &NSDate::dateWithTimeIntervalSince1970(seconds as f64),
            NSDateFormatterStyle::MediumStyle,
            NSDateFormatterStyle::ShortStyle,
        )
        .to_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| seconds.to_string())
}
