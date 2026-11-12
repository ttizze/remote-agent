//! Web links follow this device's preference; a primary modifier always opens
//! the system browser. File links keep their separate workspace actions.
use crate::localization::UiLanguage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WebLinkTarget {
    #[default]
    SystemBrowser,
    InAppBrowser,
}

/// Only HTTP(S) links have an in-app browser destination.
pub fn web_link_target(
    link: &str,
    preference: WebLinkTarget,
    primary_modifier: bool,
) -> Option<WebLinkTarget> {
    let url = url::Url::parse(link).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    Some(if primary_modifier {
        WebLinkTarget::SystemBrowser
    } else {
        preference
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebLinkAction {
    SystemBrowser,
    InAppBrowser,
    CopyLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebLinkMenuItem {
    pub id: WebLinkAction,
    pub title: String,
}

/// Explicit destinations are available even when the default is the other one.
pub fn web_link_menu(language: UiLanguage) -> Vec<WebLinkMenuItem> {
    [
        (WebLinkAction::SystemBrowser, "既定のブラウザで開く"),
        (WebLinkAction::InAppBrowser, "Bexで開く"),
        (WebLinkAction::CopyLink, "リンクをコピー"),
    ]
    .into_iter()
    .map(|(id, title)| WebLinkMenuItem {
        id,
        title: language.text(title).into(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_click_follows_preference_and_modifier_always_uses_system_browser() {
        assert_eq!(WebLinkTarget::default(), WebLinkTarget::SystemBrowser);
        for link in [
            "https://example.com/path?q=1#part",
            "http://localhost:3000",
            "HTTPS://EXAMPLE.COM",
        ] {
            for preference in [WebLinkTarget::SystemBrowser, WebLinkTarget::InAppBrowser] {
                assert_eq!(web_link_target(link, preference, false), Some(preference));
                assert_eq!(
                    web_link_target(link, preference, true),
                    Some(WebLinkTarget::SystemBrowser)
                );
            }
        }
    }

    #[test]
    fn files_fragments_and_other_schemes_keep_their_own_actions() {
        for link in [
            "",
            "src/main.rs",
            "/tmp/file.html",
            "C:\\src\\main.rs",
            "file:///tmp/index.html",
            "#section",
            "mailto:user@example.com",
            "javascript:alert(1)",
            "https://",
            "//example.com",
        ] {
            for modifier in [false, true] {
                assert_eq!(
                    web_link_target(link, WebLinkTarget::InAppBrowser, modifier),
                    None,
                    "{link}"
                );
            }
        }
    }

    #[test]
    fn menu_lists_explicit_destinations_and_copy_in_the_selected_language() {
        let menu = web_link_menu(UiLanguage::English);
        assert_eq!(
            menu.iter().map(|item| item.id).collect::<Vec<_>>(),
            [
                WebLinkAction::SystemBrowser,
                WebLinkAction::InAppBrowser,
                WebLinkAction::CopyLink
            ]
        );
        assert_eq!(menu[0].title, "Open in default browser");
        assert_eq!(menu[1].title, "Open in Bex");
        assert_eq!(menu[2].title, "Copy link");
        assert_eq!(web_link_menu(UiLanguage::Japanese)[1].title, "Bexで開く");
    }
}
