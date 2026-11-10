//! One NSH surface draws the whole window: AppKit's on the Mac, Win32's on Windows.
//! On the Mac, WebKit's pages are owned by a hosted container.
use crate::{Area, application::Application};
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(windows)]
use nsh_platform_windows as platform;
use platform::{AppOptions, AppSource, run_app};

/// Ends the app's event loop once an update's worker waits to replace it.
pub(crate) fn quit() {
    #[cfg(target_os = "macos")]
    nsh_platform_macos::quit_app();
    // Windows' app ends with its only window.
    #[cfg(windows)]
    nsh_core::element::desktop::close_window();
}

pub fn run() -> std::process::ExitCode {
    // The app's data follows its Host's scope, so a source build keeps apart
    // from the installed app as their Hosts do.
    let scope = match agent_local_host::launch::HostScope::from_environment(cfg!(debug_assertions))
    {
        Ok(scope) => scope,
        Err(reason) => return fail(&reason.to_string()),
    };
    // The app's diagnostic log sits beside its Host's, and starts before the
    // app's threads do.
    let logging = agent_transport::diagnostics::initialize(
        &scope.directory,
        agent_transport::diagnostics::Component::Desktop,
        env!("CARGO_PKG_VERSION"),
        option_env!("AGENT_BUILD_REVISION").unwrap_or("development"),
    );
    let (runtime, application, local_directory) = match scope
        .resolve()
        .map(|host| host.directory)
        .map_err(crate::storage::error)
        .and_then(|directory| {
            Application::new(scope.directory.join("nsh-client"), Some(scope))
                .map(|(runtime, app)| (runtime, app, Some(directory)))
        }) {
        Ok(service) => service,
        Err(reason) => return fail(&reason.to_string()),
    };
    if let Err(reason) = logging {
        application.fail(format!("エラーログを保存できません: {reason}"));
    }
    #[cfg(target_os = "macos")]
    nsh_platform_macos::set_components(crate::native::macos::register);
    let options = AppOptions {
        app_id: "app.bex.desktop".into(),
        title: "Bex".into(),
        size: (1360, 900),
        // Closing hides the window; the Dock reopens it, and Quit stops the app.
        #[cfg(target_os = "macos")]
        keep_alive_on_close: true,
        menu_labels: menu_labels(),
        ..Default::default()
    };
    #[cfg(windows)]
    crate::native::windows::register(&options.components);
    #[cfg(target_os = "macos")]
    let browser = {
        let browser = crate::native::browser_macos::Service::new(
            application.clone(),
            runtime.handle().clone(),
            local_directory,
        );
        crate::native::browser_macos::register(&options.components, browser.clone());
        browser
    };
    // Windows' pages are the Host's rendered images; no local renderer starts.
    #[cfg(windows)]
    let _ = local_directory;
    #[cfg(all(target_os = "macos", debug_assertions))]
    let acceptance = match crate::acceptance::from_environment(
        objc2::MainThreadMarker::new().expect("the app starts on the main thread"),
    ) {
        Ok(acceptance) => acceptance,
        Err(reason) => return fail(&reason),
    };
    let result = run_app(
        AppSource::Rust(crate::root::root(application.clone(), Area::Shell, None)),
        options,
    );
    tracing::info!(target: "agent", operation = "shutdown", "Bex shutting down");
    #[cfg(target_os = "macos")]
    runtime.block_on(browser.shutdown().wait());
    if let Err(reason) = runtime.block_on(application.shutdown()) {
        return fail(&reason.to_string());
    }
    #[cfg(all(target_os = "macos", debug_assertions))]
    if acceptance.is_some_and(|passed| !passed.get()) {
        return std::process::ExitCode::FAILURE;
    }
    result
}

/// The menus NSH makes itself, in the UI language: the Mac's menu bar (the app
/// menu, Edit and Window) and Windows' menus of text fields and selectable
/// text. The app opens with them and relabels them as its language changes.
pub(crate) fn menu_labels() -> nsh_core::desktop::MenuLabels {
    use crate::l10n::tr;
    use nsh_core::desktop::{MenuLabels, MenuRole};
    let mut labels = MenuLabels::default();
    labels.roles = [
        (MenuRole::About, tr("Bexについて")),
        (MenuRole::Hide, tr("Bexを非表示")),
        (MenuRole::HideOthers, tr("ほかを非表示")),
        (MenuRole::ShowAll, tr("すべてを表示")),
        (MenuRole::Quit, tr("Bexを終了")),
        (MenuRole::Undo, tr("取り消す")),
        (MenuRole::Redo, tr("やり直す")),
        (MenuRole::Cut, tr("カット")),
        (MenuRole::Copy, tr("コピー")),
        (MenuRole::Paste, tr("ペースト")),
        (MenuRole::SelectAll, tr("すべてを選択")),
        (MenuRole::Minimize, tr("しまう")),
        (MenuRole::Zoom, tr("拡大/縮小")),
        (MenuRole::ToggleFullScreen, tr("フルスクリーンにする")),
        (MenuRole::Close, tr("閉じる")),
    ]
    .into_iter()
    .map(|(role, label)| (role, label.to_owned()))
    .collect();
    labels.edit_menu = Some(tr("編集").to_owned());
    labels.window_menu = Some(tr("ウインドウ").to_owned());
    labels.delete = Some(tr("削除").to_owned());
    labels
}

/// A failure before or after the window: on the console on the Mac, in a
/// message box on Windows, whose app has no console.
fn fail(reason: &str) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    eprintln!("{reason}");
    #[cfg(windows)]
    nsh_platform_windows::fatal_error(reason);
    std::process::ExitCode::FAILURE
}
