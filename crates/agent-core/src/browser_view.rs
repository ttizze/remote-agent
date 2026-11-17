//! Shared browser presentation and input decisions.
use crate::localization::{UiLanguage, localized_ui_format};
use agent_protocol::browser::BrowserNotice;

/// Native browser surfaces retain facts so language changes do not re-read the Host.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BrowserStatus {
    Importing { remote: bool },
    Notice { notice: BrowserNotice },
    Value { value: String },
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn browser_status_label(language: UiLanguage, status: BrowserStatus) -> String {
    let (key, arguments) = match status {
        BrowserStatus::Importing { remote } => (
            if remote {
                "ブラウザデータを取り込み中… 接続先のMacでOSのアクセス確認が出たら許可してください。"
            } else {
                "ブラウザデータを取り込み中… OSのアクセス確認が出たら許可してください。"
            },
            vec![],
        ),
        BrowserStatus::Value { value } => return value,
        BrowserStatus::Notice { notice } => match notice {
            BrowserNotice::CookiesImported { count: 0, skipped } => (
                "取り込めるCookieがありません（期限切れなど{0}件を除外）。",
                vec![skipped.to_string()],
            ),
            BrowserNotice::CookiesImported { count, skipped } => (
                "Cookieを{0}件取り込みました（期限切れなど{1}件を除外）。ログイン状態をサイトで確認してください。",
                vec![count.to_string(), skipped.to_string()],
            ),
            BrowserNotice::PasswordsImported { count, skipped } => (
                "パスワードを{0}件Keychainに保存しました（HTTPS以外など{1}件を除外）。入力はサイトごとに選択してください。",
                vec![count.to_string(), skipped.to_string()],
            ),
            BrowserNotice::PasswordsDeleted => {
                ("このサイトの保存パスワードを削除しました。", vec![])
            }
            BrowserNotice::PasswordFilled => (
                "パスワードを入力しました。内容を確認してからログインしてください。",
                vec![],
            ),
        },
    };
    localized_ui_format(language, key.into(), arguments)
}

pub fn address_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 8192 {
        return Err("URL または検索語を入力してください。".into());
    }
    if input == "about:blank" || input.contains("://") {
        return agent_protocol::browser::browser_url(input);
    }
    let authority = input.split(['/', '?', '#']).next().unwrap_or_default();
    let bare_port = authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
    if authority.contains(':') && !bare_port && !authority.starts_with('[') {
        return Err("http または https の URL を入力してください。".into());
    }
    if !input.chars().any(char::is_whitespace)
        && (authority.contains('.')
            || authority == "localhost"
            || authority.starts_with('[')
            || bare_port)
    {
        return agent_protocol::browser::browser_url(input);
    }
    let mut url = url::Url::parse("https://duckduckgo.com/").unwrap();
    url.query_pairs_mut().append_pair("q", input);
    Ok(url.into())
}

/// Follow a native view's logical size without repeatedly resizing an unchanged page.
pub fn fit_action(
    width: f64,
    height: f64,
    current_width: u32,
    current_height: u32,
) -> Option<agent_protocol::browser::BrowserAction> {
    use agent_protocol::browser::{BrowserAction, MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH};
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return None;
    }
    let width = width
        .round()
        .clamp(f64::from(MIN_WIDTH), f64::from(MAX_WIDTH)) as u32;
    let height = height
        .round()
        .clamp(f64::from(MIN_HEIGHT), f64::from(MAX_HEIGHT)) as u32;
    (width != current_width || height != current_height)
        .then_some(BrowserAction::Resize { width, height })
}

/// A pointer click or Tab explicitly chooses a page editor. Polling must not
/// reopen a keyboard the user dismissed, or steal focus from the address bar.
pub fn input_focus(
    action: &agent_protocol::browser::BrowserAction,
    input_kind: Option<agent_protocol::browser::BrowserInputKind>,
) -> Option<bool> {
    use agent_protocol::browser::{BrowserAction, BrowserKey};
    match action {
        BrowserAction::Click { .. }
        | BrowserAction::Key {
            key: BrowserKey::Tab,
        } => Some(input_kind.is_some()),
        _ => None,
    }
}

/// Where a pointer at (`x`, `y`) on a page image shown at `scale` is in the
/// page's own coordinates; `None` off the `width` × `height` page.
fn page_point(x: f64, y: f64, scale: f64, width: u32, height: u32) -> Option<(f64, f64)> {
    if !(scale.is_finite() && scale > 0.) {
        return None;
    }
    let (x, y) = (x / scale, y / scale);
    ((0.0..f64::from(width)).contains(&x) && (0.0..f64::from(height)).contains(&y))
        .then_some((x, y))
}

/// A click at a pointer on the page image shown at `scale`.
pub fn click_action(
    x: f64,
    y: f64,
    scale: f64,
    width: u32,
    height: u32,
) -> Option<agent_protocol::browser::BrowserAction> {
    let (x, y) = page_point(x, y, scale, width, height)?;
    Some(agent_protocol::browser::BrowserAction::Click { x, y })
}

/// A wheel or touchpad scroll over the page image shown at `scale`: the
/// pointer's page point and the distance in page points, down and right
/// positive, within what the Host accepts for one step.
pub fn scroll_action(
    (x, y): (f64, f64),
    (delta_x, delta_y): (f64, f64),
    scale: f64,
    width: u32,
    height: u32,
) -> Option<agent_protocol::browser::BrowserAction> {
    let (x, y) = page_point(x, y, scale, width, height)?;
    let step = |delta: f64| (delta / scale).clamp(-4096., 4096.);
    (delta_x.is_finite() && delta_y.is_finite() && (delta_x != 0. || delta_y != 0.)).then(|| {
        agent_protocol::browser::BrowserAction::Scroll {
            x,
            y,
            delta_x: step(delta_x),
            delta_y: step(delta_y),
        }
    })
}

/// The address bar's text: the shown tab's URL, empty for a blank page or no tab.
pub fn displayed_address(frame: &agent_protocol::browser::BrowserFrame) -> &str {
    frame
        .tabs
        .iter()
        .find(|tab| tab.id == frame.tab_id)
        .map_or("", |tab| {
            if tab.url == "about:blank" {
                ""
            } else {
                &tab.url
            }
        })
}

/// A tab's name: its title, else its URL. A blank page has none; the client
/// names it as a new tab.
pub fn tab_label(tab: &agent_protocol::browser::BrowserTab) -> Option<&str> {
    if tab.url == "about:blank" {
        None
    } else if tab.title.is_empty() {
        Some(&tab.url)
    } else {
        Some(&tab.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::browser::{BrowserFrame, BrowserTab};

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ResizeCase {
        width: u32,
        height: u32,
        current_width: u32,
        current_height: u32,
        next_width: Option<u32>,
        next_height: Option<u32>,
    }

    #[test]
    #[ignore = "requires Lean-generated presentation.jsonl"]
    fn lean_core_browser_resize_conformance() {
        for row in crate::presentation::lean_cases::cases::<ResizeCase>("resize", 324) {
            let actual = super::fit_action(
                f64::from(row.width) / 2.,
                f64::from(row.height) / 2.,
                row.current_width,
                row.current_height,
            );
            let actual = actual.map(|action| match action {
                agent_protocol::browser::BrowserAction::Resize { width, height } => (width, height),
                _ => panic!("unexpected action: {row:?}"),
            });
            assert_eq!(actual, row.next_width.zip(row.next_height), "{row:?}");
        }
    }

    fn tab(id: &str, title: &str, url: &str) -> BrowserTab {
        BrowserTab {
            id: id.into(),
            title: title.into(),
            url: url.into(),
        }
    }

    #[test]
    fn the_address_bar_shows_the_selected_page_and_leaves_blank_pages_empty() {
        let mut frame = BrowserFrame {
            tabs: vec![
                tab("blank", "", "about:blank"),
                tab("page", "例", "https://example.com/日本語"),
            ],
            tab_id: "page".into(),
            ..BrowserFrame::default()
        };
        assert_eq!(displayed_address(&frame), "https://example.com/日本語");
        frame.tab_id = "blank".into();
        assert_eq!(displayed_address(&frame), "");
        frame.tab_id = "closed".into();
        assert_eq!(displayed_address(&frame), "");
        frame.tab_id = String::new();
        assert_eq!(displayed_address(&frame), "");
    }

    #[test]
    fn tabs_are_named_by_title_then_url_and_blank_pages_by_the_client() {
        assert_eq!(
            tab_label(&tab("a", "Example 👋", "https://example.com/")),
            Some("Example 👋")
        );
        assert_eq!(
            tab_label(&tab("a", "", "https://example.com/")),
            Some("https://example.com/")
        );
        assert_eq!(tab_label(&tab("a", "Blank", "about:blank")), None);
    }

    #[test]
    fn pointers_reach_the_page_at_its_own_scale_and_only_inside_it() {
        use agent_protocol::browser::BrowserAction;
        assert_eq!(page_point(50., 25., 0.5, 1280, 800), Some((100., 50.)));
        assert_eq!(page_point(640., 0., 0.5, 1280, 800), None);
        assert_eq!(page_point(-1., 10., 1., 1280, 800), None);
        assert_eq!(page_point(10., 10., 0., 1280, 800), None);
        assert_eq!(page_point(10., 10., f64::NAN, 1280, 800), None);
        assert_eq!(page_point(10., 10., 1., 0, 0), None);
        assert!(matches!(
            click_action(10., 20., 0.5, 1280, 800),
            Some(BrowserAction::Click { x, y }) if (x, y) == (20., 40.)
        ));
        let Some(BrowserAction::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        }) = scroll_action((10., 20.), (0., 60.), 0.5, 1280, 800)
        else {
            panic!("a scroll over the page scrolls it");
        };
        assert_eq!((x, y, delta_x, delta_y), (20., 40., 0., 120.));
        assert!(matches!(
            scroll_action((0., 0.), (-1e9, 1e9), 1., 1280, 800),
            Some(BrowserAction::Scroll { delta_x, delta_y, .. }) if (delta_x, delta_y) == (-4096., 4096.)
        ));
        assert!(scroll_action((0., 0.), (0., 0.), 1., 1280, 800).is_none());
        assert!(scroll_action((0., 0.), (f64::NAN, 1.), 1., 1280, 800).is_none());
        assert!(scroll_action((2000., 0.), (0., 1.), 1., 1280, 800).is_none());
    }

    proptest::proptest! {
        #[test]
        fn page_actions_are_ones_the_host_accepts(
            x in -10_000.0f64..10_000.0,
            y in -10_000.0f64..10_000.0,
            dx in -1e7f64..1e7,
            dy in -1e7f64..1e7,
            scale in 0.01f64..4.0,
            width in 1u32..=agent_protocol::browser::MAX_WIDTH,
            height in 1u32..=agent_protocol::browser::MAX_HEIGHT,
        ) {
            for action in [
                click_action(x, y, scale, width, height),
                scroll_action((x, y), (dx, dy), scale, width, height),
            ]
            .into_iter()
            .flatten()
            {
                proptest::prop_assert!(action.validate().is_ok());
            }
        }
    }

    #[test]
    fn credential_outcomes_use_current_language_and_page_values_stay_verbatim() {
        use BrowserNotice::*;
        for (notice, japanese, english) in [
            (
                CookiesImported {
                    count: 0,
                    skipped: 3,
                },
                "取り込めるCookieがありません（期限切れなど3件を除外）。",
                "No cookies to import (3 expired or otherwise excluded).",
            ),
            (
                CookiesImported {
                    count: 12,
                    skipped: 3,
                },
                "Cookieを12件取り込みました（期限切れなど3件を除外）。ログイン状態をサイトで確認してください。",
                "Imported 12 cookies (3 expired or otherwise excluded). Check your login status on the site.",
            ),
            (
                PasswordsImported {
                    count: 4,
                    skipped: 2,
                },
                "パスワードを4件Keychainに保存しました（HTTPS以外など2件を除外）。入力はサイトごとに選択してください。",
                "Saved 4 passwords in Keychain (2 non-HTTPS or otherwise excluded). Select passwords to fill for each site.",
            ),
            (
                PasswordsDeleted,
                "このサイトの保存パスワードを削除しました。",
                "Deleted saved passwords for this site.",
            ),
            (
                PasswordFilled,
                "パスワードを入力しました。内容を確認してからログインしてください。",
                "Filled the password. Check the contents before signing in.",
            ),
        ] {
            let status = BrowserStatus::Notice { notice };
            assert_eq!(
                browser_status_label(UiLanguage::Japanese, status.clone()),
                japanese
            );
            assert_eq!(browser_status_label(UiLanguage::English, status), english);
        }
        for language in [UiLanguage::Japanese, UiLanguage::English] {
            for value in ["設定", "残り 42%", "設定\n{0}"] {
                assert_eq!(
                    browser_status_label(
                        language,
                        BrowserStatus::Value {
                            value: value.into()
                        }
                    ),
                    value
                );
            }
        }
        assert_eq!(
            browser_status_label(
                UiLanguage::English,
                BrowserStatus::Importing { remote: false }
            ),
            "Importing browser data… Allow access if prompted by the OS."
        );
        assert_eq!(
            browser_status_label(
                UiLanguage::English,
                BrowserStatus::Importing { remote: true }
            ),
            "Importing browser data… Allow access if prompted on the remote Mac."
        );
    }

    #[test]
    fn only_explicit_editor_selection_changes_keyboard_focus() {
        use agent_protocol::browser::{BrowserAction, BrowserInputKind, BrowserKey};
        for (kind, opens_keyboard) in [
            (None, false),
            (Some(BrowserInputKind::Text), true),
            (Some(BrowserInputKind::Password), true),
        ] {
            for action in [
                BrowserAction::Click { x: 10., y: 20. },
                BrowserAction::Key {
                    key: BrowserKey::Tab,
                },
            ] {
                assert_eq!(input_focus(&action, kind), Some(opens_keyboard));
            }
            for action in [
                BrowserAction::Read,
                BrowserAction::Type {
                    text: "日本語".into(),
                },
                BrowserAction::Key {
                    key: BrowserKey::Enter,
                },
                BrowserAction::Key {
                    key: BrowserKey::Backspace,
                },
            ] {
                assert_eq!(input_focus(&action, kind), None);
            }
        }
    }

    #[test]
    fn follows_the_view_and_preserves_protocol_limits() {
        use agent_protocol::browser::{BrowserAction::Resize, MAX_HEIGHT, MAX_WIDTH};
        for (width, height, expected) in [
            (540.3, 852.6, (540, 853)),
            (20., 30., (320, 240)),
            (10000., 10000., (MAX_WIDTH, MAX_HEIGHT)),
        ] {
            let Some(Resize {
                width: actual_width,
                height: actual_height,
            }) = fit_action(width, height, 1280, 800)
            else {
                panic!("expected resize");
            };
            assert_eq!((actual_width, actual_height), expected);
            assert!(fit_action(width, height, actual_width, actual_height).is_none());
        }
        for invalid in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(fit_action(invalid, 800., 1280, 800).is_none());
            assert!(fit_action(1280., invalid, 1280, 800).is_none());
        }
        assert!(fit_action(1280., 800., 1280, 800).is_none());
        assert!(matches!(
            fit_action(1280., 801., 1280, 800),
            Some(Resize {
                width: 1280,
                height: 801
            })
        ));
        assert!(matches!(
            fit_action(1281., 800., 1280, 800),
            Some(Resize {
                width: 1281,
                height: 800
            })
        ));
    }

    proptest::proptest! {
        #[test]
        fn resized_viewports_are_valid_and_stable(width in 0.01f64..20000., height in 0.01f64..20000.) {
            let action = fit_action(width, height, 0, 0).unwrap();
            proptest::prop_assert!(action.validate().is_ok());
            if let agent_protocol::browser::BrowserAction::Resize { width: actual_width, height: actual_height } = action {
                proptest::prop_assert!(fit_action(width, height, actual_width, actual_height).is_none());
            }
        }
    }

    #[test]
    fn addresses_search_and_host_local_development_are_distinct() {
        for (input, expected) in [
            ("example.com", "https://example.com/"),
            ("localhost:3000/path", "http://localhost:3000/path"),
            ("[::1]:4000", "http://[::1]:4000/"),
            (
                "日本語 search",
                "https://duckduckgo.com/?q=%E6%97%A5%E6%9C%AC%E8%AA%9E+search",
            ),
            (
                "query&secret=other",
                "https://duckduckgo.com/?q=query%26secret%3Dother",
            ),
        ] {
            assert_eq!(address_url(input).unwrap(), expected);
        }
        for input in [
            "",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:pass@example.com",
        ] {
            assert!(address_url(input).is_err());
        }
    }
}
