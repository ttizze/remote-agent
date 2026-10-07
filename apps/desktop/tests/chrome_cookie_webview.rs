// Run on macOS with `cargo test -p bex-desktop --test chrome_cookie_webview`.
// Each WebKit persistence phase runs in a fresh process with a unique data store.
#[cfg(target_os = "macos")]
include!("../src/browser.rs");

#[cfg(not(target_os = "macos"))]
fn main() {
    if std::env::args().any(|arg| arg == "--list") {
        return;
    }
    eprintln!("Chrome Cookie import uses macOS WebKit; run this test on macOS.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<_> = std::env::args().collect();
    // Listing must never initialize WebKit or create the test's native windows.
    if args.iter().any(|arg| arg == "--list") {
        // Listing in both modes marks this manual native-window probe ignored
        // in nextest, while the explicit cargo --test command still runs it.
        println!("chrome_cookie_webview: test");
        return;
    }
    if args.get(1).is_some_and(|s| s == "--phase") {
        native_phase(&args[2], &args[3], &args[4], &args[5]);
        return;
    }
    use std::io::{Read, Write};
    let (directory, db) = chrome_tests::fixture();
    db.execute_batch("UPDATE cookies SET host_key='localhost', path='/', is_secure=0,
        encrypted_value=X'76313061f8d83827d2fabc8b7c4cbc230263a34b04859bc17fad7d973df6aabdd1f843fde0c9f8d84450325e95ca4078ad1562';").unwrap();
    // Expiration remains a persistent cookie, independent of the test's calendar date.
    db.execute(
        "UPDATE cookies SET expires_utc=?",
        [(chrono::Utc::now().timestamp() + 3600 + 11_644_473_600) * 1_000_000],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO cookies SELECT creation_utc, host_key, top_frame_site_key,
        'session-marker', 'fixture-session', X'', path, 0, is_secure, is_httponly,
        last_access_utc, 0, 0, priority, samesite, source_scheme, source_port,
        last_update_utc, source_type, has_cross_site_ancestor FROM cookies;",
    )
    .unwrap();
    drop(db);
    let profile = directory.path().join("Default");
    std::fs::create_dir(&profile).unwrap();
    std::fs::rename(directory.path().join("Cookies"), profile.join("Cookies")).unwrap();
    std::fs::write(
        directory.path().join("Local State"),
        r#"{"profile":{"info_cache":{"Default":{"name":"Test profile"}}}}"#,
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://localhost:{}/",
        listener.local_addr().unwrap().port()
    );
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let recorded = recorded.clone();
            // WebKit can close or leave speculative connections idle. An
            // unused socket must not stop the server or block actual requests.
            std::thread::spawn(move || {
                if stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .is_err()
                {
                    return;
                }
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(size) = stream.read(&mut buffer) else {
                        return;
                    };
                    if size == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..size]);
                    assert!(request.len() < 16384);
                }
                let request = String::from_utf8(request).unwrap();
                let authenticated = request.lines().any(|line| {
                    line.to_ascii_lowercase().starts_with("cookie:")
                        && line
                            .split_once(':')
                            .unwrap()
                            .1
                            .split(';')
                            .any(|cookie| cookie.trim() == "login=fixture-login")
                });
                recorded.lock().unwrap().push(authenticated);
                let body = if authenticated {
                    "<!doctype html><h1>Logged in</h1>"
                } else {
                    "<!doctype html><h1>Logged out</h1>"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                // A cancelled navigation may close its connection before the
                // response; the native page assertions still require delivery.
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    let store = uuid::Uuid::new_v4().to_string();
    let bundle = directory.path().join("Bex Cookie Import Test.app");
    let executables = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&executables).unwrap();
    let executable = executables.join("CookieImportTest");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
        <plist version="1.0"><dict>
        <key>CFBundleExecutable</key><string>CookieImportTest</string>
        <key>CFBundleIdentifier</key><string>app.bex.cookie-test.{store}</string>
        <key>CFBundleName</key><string>Bex Cookie Import Test</string>
        <key>CFBundlePackageType</key><string>APPL</string>
        <key>LSMinimumSystemVersion</key><string>26.0</string>
        <key>NSHighResolutionCapable</key><true/>
        </dict></plist>"#
        ),
    )
    .unwrap();
    if std::env::var_os("BEX_COOKIE_SMOKE_INSPECT").is_some() {
        println!("Native test bundle: {}", bundle.display());
    }
    let mut failure = None;
    for phase in ["import", "reopen", "cleanup"] {
        let mut child = std::process::Command::new(&executable)
            .args(["--phase", phase, &store, &url, profile.to_str().unwrap()])
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                if !status.success() {
                    failure = Some(format!("native {phase} failed: {status}"));
                }
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                failure = Some(format!("native {phase} timed out"));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    assert!(failure.is_none(), "{}", failure.unwrap_or_default());
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.first(),
        Some(&false),
        "fresh WebKit store starts logged out"
    );
    assert!(
        requests.iter().filter(|&&logged_in| logged_in).count() >= 2,
        "both import and reopened process must reach the server with authentication"
    );
    println!(
        "PASS: encrypted Chrome DB -> Browser import/retry -> authenticated HTTP; HttpOnly; fresh-process persistence"
    );
}

#[cfg(target_os = "macos")]
fn native_phase(phase: &str, identifier: &str, url: &str, profile: &str) {
    use gpui_kit::component::Root;
    use wry::{WebViewBuilderExtDarwin, WebViewExtDarwin};
    let identifier = *uuid::Uuid::parse_str(identifier).unwrap().as_bytes();
    let phase = phase.to_owned();
    let url = url.to_owned();
    let profile = std::path::PathBuf::from(profile);
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            cx.activate(true);
            cx.spawn(async move |cx| {
                let mut browser = None;
                let window = cx
                    .open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(80.), px(80.)),
                                size(px(560.), px(760.)),
                            ))),
                            titlebar: Some(TitlebarOptions {
                                title: Some("Bex Cookie Import Test".into()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        |window, cx| {
                            let view = Browser::new(
                                wry::WebViewBuilder::new()
                                    .with_data_store_identifier(identifier)
                                    .with_incognito(phase == "cleanup"),
                                ChromeProfileSource { root: profile.parent().map(std::path::Path::to_path_buf), password: chrome_tests::password },
                                window,
                                cx,
                            )
                            .unwrap();
                            browser = Some(view.clone());
                            cx.new(|cx| Root::new(view, window, cx))
                        },
                    )
                    .unwrap();
                let browser = browser.unwrap();
                if phase == "cleanup" {
                    let (tx, rx) = async_channel::bounded(1);
                    wry::WebView::remove_data_store(&identifier, move |result| {
                        tx.try_send(result).unwrap();
                    });
                    rx.recv().await.unwrap().unwrap();
                    cx.update(|cx| cx.quit());
                    return;
                }

                browser.update(cx, |s, cx| {
                    s.set_visible(true, cx);
                    s.webview.read(cx).raw().load_url(&url).unwrap();
                });
                wait_for_page(
                    &browser,
                    if phase == "import" {
                        "Logged out"
                    } else {
                        "Logged in"
                    },
                    cx,
                )
                .await;
                if phase == "import" {
                    seed_existing_cookies(&browser, cx).await;
                    browser.update(cx, |s, cx| {
                        s.import_chrome(profile.clone(), || Err("fixture denied".into()), cx)
                    });
                    wait_for_import(&browser, cx).await;
                    browser.update(cx, |s, cx| {
                        assert_eq!(s.error, "fixture denied");
                        assert!(s.import_notice.is_empty());
                        assert!(s.webview.read(cx).raw().cookies().unwrap().iter().any(|cookie| cookie.name() == "login" && cookie.value() == "stale"));
                    });
                    wait_for_page(&browser, "Logged out", cx).await;
                    browser.update(cx, |s, cx| {
                        s.import_chrome(profile, chrome_tests::password, cx);
                        // An overlapping import must not replace the in-flight operation.
                        s.import_chrome(
                            std::path::PathBuf::new(),
                            || panic!("overlapping import"),
                            cx,
                        );
                    });
                    wait_for_import(&browser, cx).await;
                    browser.update(cx, |s, cx| {
                        assert!(s.error.is_empty(), "{}", s.error);
                        assert!(s.import_notice.starts_with("Imported 2 cookies"));
                        assert!(s.webview.read(cx).raw().cookies().unwrap().iter().any(|cookie| cookie.name() == "scope-regression" && cookie.value() == "fixture"));
                    });
                    wait_for_page(&browser, "Logged in", cx).await;
                }
                browser.update(cx, |s, cx| {
                    let saved = s.webview.read(cx).raw().cookies().unwrap();
                    assert_eq!(saved.iter().any(|cookie| cookie.name() == "session-marker"), phase == "import", "session-only cookies must disappear in a fresh process");
                });
                let (tx, rx) = async_channel::bounded(1);
                browser
                    .update(cx, |s, cx| {
                        s.webview.read(cx).raw().evaluate_script_with_callback(
                            "document.cookie",
                            move |value| {
                                tx.try_send(value).unwrap();
                            },
                        )
                    })
                    .unwrap();
                let value: String = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
                assert!(
                    value.is_empty(),
                    "HttpOnly login must not be exposed to JavaScript"
                );
                // Allow WebKit's normal persistent-store flush before clean process exit.
                let pause = if std::env::var_os("BEX_COOKIE_SMOKE_INSPECT").is_some() {
                    45
                } else {
                    3
                };
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(pause))
                    .await;
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .unwrap();
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
}

#[cfg(target_os = "macos")]
async fn wait_for_import(browser: &Entity<Browser>, cx: &mut AsyncApp) {
    for _ in 0..300 {
        if browser.update(cx, |s, _| !s.importing) {
            return;
        }
        cx.background_executor()
            .timer(std::time::Duration::from_millis(100))
            .await;
    }
    panic!("import did not complete");
}

#[cfg(target_os = "macos")]
async fn wait_for_page(browser: &Entity<Browser>, expected: &str, cx: &mut AsyncApp) {
    for _ in 0..150 {
        let (tx, rx) = async_channel::bounded(1);
        browser.update(cx, |s, cx| {
            s.webview
                .read(cx)
                .raw()
                .evaluate_script_with_callback(
                    "document.body ? document.body.innerText : ''",
                    move |value| {
                        let _ = tx.try_send(value);
                    },
                )
                .unwrap()
        });
        // lb-wry queues scripts before the first navigation finishes and drops their callbacks.
        if let Ok(value) = rx.recv().await
            && let Ok(text) = serde_json::from_str::<String>(&value)
            && text.trim() == expected
        {
            return;
        }
        cx.background_executor()
            .timer(std::time::Duration::from_millis(100))
            .await;
    }
    panic!("page did not reach {expected}");
}

#[cfg(target_os = "macos")]
async fn seed_existing_cookies(browser: &Entity<Browser>, cx: &mut AsyncApp) {
    let cookie = wry::cookie::Cookie::build(("scope-regression", "fixture"))
        .domain(".example.test")
        .path("/")
        .build();
    browser.update(cx, |s, cx| {
        let raw = s.webview.read(cx).raw();
        raw.set_cookie(&cookie).unwrap();
        raw.set_cookie(
            &wry::cookie::Cookie::build(("login", "stale"))
                .domain("localhost")
                .path("/")
                .http_only(true)
                .build(),
        )
        .unwrap();
    });
}
