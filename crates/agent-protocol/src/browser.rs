//! Shared browser contract. The Host owns pages; clients render frames and send input.
use serde::{Deserialize, Serialize};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 800;
pub const MIN_WIDTH: u32 = 320;
pub const MIN_HEIGHT: u32 = 240;
pub const MAX_WIDTH: u32 = 7680;
pub const MAX_HEIGHT: u32 = 4320;

/// Observe only the focused editor's kind, never its value or selection.
pub const INPUT_KIND_SCRIPT: &str = include_str!("browser_input.generated.js");

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserRequest {
    pub thread_id: crate::session::SessionRef,
    pub tab_id: String,
    pub image_id: String,
    pub generation: u64,
    /// A desktop that owns this native renderer already displays its surface.
    /// Other clients leave this empty and receive browser images.
    pub renderer_id: String,
    pub action: BrowserAction,
}
impl std::fmt::Debug for BrowserRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserRequest").finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum BrowserAction {
    Read,
    Credentials,
    ImportCookies {
        profile: String,
    },
    ImportPasswords {
        profile: String,
    },
    FillPassword {
        origin: String,
        username: String,
    },
    DeletePasswords {
        origin: String,
    },
    OpenTab {
        url: String,
    },
    CloseTab {
        id: String,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Navigate {
        url: String,
    },
    Click {
        x: f64,
        y: f64,
    },
    Hover {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
    },
    Snapshot,
    Evaluate {
        expression: String,
    },
    ClickElement {
        selector: String,
    },
    Fill {
        selector: String,
        text: String,
    },
    SelectOption {
        selector: String,
        value: String,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: BrowserKey,
    },
    Back,
    Forward,
    Reload,
    SelectTab {
        id: String,
    },
    Dialog {
        accept: bool,
        text: String,
    },
}
impl BrowserAction {
    pub fn validate(&self) -> Result<(), String> {
        fn point(x: f64, y: f64) -> bool {
            x.is_finite()
                && y.is_finite()
                && (0.0..f64::from(MAX_WIDTH)).contains(&x)
                && (0.0..f64::from(MAX_HEIGHT)).contains(&y)
        }
        let valid = match self {
            Self::ImportCookies { profile } | Self::ImportPasswords { profile } => {
                !profile.is_empty() && profile.len() <= 256
            }
            Self::FillPassword { origin, username } => {
                crate::browser_import::password_origin(origin).as_deref() == Ok(origin.as_str())
                    && username.len() <= 4096
            }
            Self::DeletePasswords { origin } => {
                crate::browser_import::password_origin(origin).as_deref() == Ok(origin.as_str())
            }
            Self::Navigate { url } | Self::OpenTab { url } => {
                browser_url(url)?;
                url.len() <= 8192
            }
            Self::Click { x, y } | Self::Hover { x, y } => point(*x, *y),
            Self::Drag { x, y, to_x, to_y } => point(*x, *y) && point(*to_x, *to_y),
            Self::Resize { width, height } => {
                (MIN_WIDTH..=MAX_WIDTH).contains(width)
                    && (MIN_HEIGHT..=MAX_HEIGHT).contains(height)
            }
            Self::Evaluate { expression } => {
                !expression.trim().is_empty() && expression.len() <= 16 * 1024
            }
            Self::ClickElement { selector } => !selector.is_empty() && selector.len() <= 4096,
            Self::Fill { selector, text }
            | Self::SelectOption {
                selector,
                value: text,
            } => !selector.is_empty() && selector.len() <= 4096 && text.len() <= 16 * 1024,
            Self::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                point(*x, *y)
                    && delta_x.is_finite()
                    && delta_y.is_finite()
                    && delta_x.abs() <= 4096.0
                    && delta_y.abs() <= 4096.0
            }
            Self::Type { text } | Self::Dialog { text, .. } => text.len() <= 16 * 1024,
            Self::SelectTab { id } | Self::CloseTab { id } => !id.is_empty() && id.len() <= 256,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err("ブラウザ操作の値が範囲外です。".into())
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BrowserKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    SelectAll,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserTab {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserDialog {
    pub message: String,
    pub prompt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserInputKind {
    Text,
    Password,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportProfile {
    pub id: String,
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserImportError {
    pub browser: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserCredentials {
    pub profiles: Vec<BrowserImportProfile>,
    pub import_errors: Vec<BrowserImportError>,
    pub origin: Option<String>,
    pub accounts: Vec<String>,
    pub password_error: Option<String>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowserFrame {
    pub tabs: Vec<BrowserTab>,
    pub tab_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(with = "crate::protocol::bytes")]
    pub image: Vec<u8>,
    /// Empty clears the client's cached image. Otherwise omitted bytes reuse
    /// the image with this identity (for example while a dialog blocks capture).
    pub image_id: String,
    pub dialog: Option<BrowserDialog>,
    pub input_kind: Option<BrowserInputKind>,
    pub generation: u64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
    pub page_text: String,
    pub value: String,
    pub credentials: Option<BrowserCredentials>,
    pub renderer_id: String,
}
impl std::fmt::Debug for BrowserFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserFrame")
            .field("image_bytes", &self.image.len())
            .finish_non_exhaustive()
    }
}

impl BrowserRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.thread_id.validate()?;
        if self.tab_id.len() > 256 || self.image_id.len() > 256 || self.renderer_id.len() > 256 {
            return Err("ブラウザ接続情報が不正です。".into());
        }
        self.action.validate()
    }
}

/// Input refers to the page the client saw. Observations and tab selection can
/// refresh stale state; uncertain input is never replayed against another page.
pub fn validate_displayed_page(
    active: &str,
    generation: u64,
    displayed_tab: &str,
    displayed_generation: u64,
    action: &BrowserAction,
) -> Result<(), String> {
    if matches!(
        action,
        BrowserAction::Read | BrowserAction::Snapshot | BrowserAction::SelectTab { .. }
    ) {
        return Ok(());
    }
    if active != displayed_tab {
        return Err("表示中のタブが変わりました。最新の画面で再試行してください。".into());
    }
    if generation != displayed_generation {
        return Err("表示中のページが変わりました。最新の画面で再試行してください。".into());
    }
    Ok(())
}

pub fn validate_viewport_point(
    width: u32,
    height: u32,
    action: &BrowserAction,
) -> Result<(), String> {
    let point = |x: f64, y: f64| {
        (0.0..f64::from(width)).contains(&x) && (0.0..f64::from(height)).contains(&y)
    };
    let valid = match action {
        BrowserAction::Click { x, y }
        | BrowserAction::Hover { x, y }
        | BrowserAction::Scroll { x, y, .. } => point(*x, *y),
        BrowserAction::Drag { x, y, to_x, to_y } => point(*x, *y) && point(*to_x, *to_y),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err("表示範囲外の座標です。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_input_is_rejected_but_observation_can_recover() {
        for action in [
            BrowserAction::Type { text: "one".into() },
            BrowserAction::Navigate {
                url: "https://example.com/".into(),
            },
            BrowserAction::Key {
                key: BrowserKey::Enter,
            },
        ] {
            assert!(validate_displayed_page("a", 7, "a", 7, &action).is_ok());
            assert!(validate_displayed_page("a", 7, "b", 7, &action).is_err());
            assert!(validate_displayed_page("a", 7, "a", 6, &action).is_err());
            assert!(validate_displayed_page("a", 7, "a", 8, &action).is_err());
        }
        for action in [
            BrowserAction::Read,
            BrowserAction::Snapshot,
            BrowserAction::SelectTab { id: "b".into() },
        ] {
            assert!(validate_displayed_page("a", 7, "b", 0, &action).is_ok());
        }
    }
    #[test]
    fn input_coordinates_follow_the_current_viewport() {
        for (x, y, valid) in [
            (0., 0., true),
            (389., 843., true),
            (390., 0., false),
            (0., 844., false),
            (-1., 0., false),
            (f64::NAN, 0., false),
            (0., f64::INFINITY, false),
        ] {
            for action in [
                BrowserAction::Click { x, y },
                BrowserAction::Hover { x, y },
                BrowserAction::Scroll {
                    x,
                    y,
                    delta_x: 1.,
                    delta_y: 1.,
                },
                BrowserAction::Drag {
                    x: 0.,
                    y: 0.,
                    to_x: x,
                    to_y: y,
                },
                BrowserAction::Drag {
                    x,
                    y,
                    to_x: 0.,
                    to_y: 0.,
                },
            ] {
                assert_eq!(validate_viewport_point(390, 844, &action).is_ok(), valid);
            }
        }
    }
    #[test]
    fn browser_contract_rejects_invalid_input_and_preserves_binary_images() {
        for action in [
            BrowserAction::Click {
                x: f64::NAN,
                y: 0.0,
            },
            BrowserAction::Click {
                x: f64::from(MAX_WIDTH),
                y: 0.0,
            },
            BrowserAction::Scroll {
                x: 0.0,
                y: 0.0,
                delta_x: 0.0,
                delta_y: f64::INFINITY,
            },
            BrowserAction::Navigate {
                url: "file:///private/data".into(),
            },
            BrowserAction::Type {
                text: "x".repeat(16385),
            },
        ] {
            assert!(action.validate().is_err());
        }
        for action in [
            BrowserAction::ImportCookies {
                profile: String::new(),
            },
            BrowserAction::ImportPasswords {
                profile: "x".repeat(257),
            },
            BrowserAction::FillPassword {
                origin: "https://example.test/login".into(),
                username: "fixture-user".into(),
            },
            BrowserAction::FillPassword {
                origin: "https://example.test".into(),
                username: "x".repeat(4097),
            },
            BrowserAction::DeletePasswords {
                origin: "http://example.test".into(),
            },
        ] {
            assert!(action.validate().is_err());
        }
        for action in [
            BrowserAction::Credentials,
            BrowserAction::ImportCookies {
                profile: "Chrome/Default".into(),
            },
            BrowserAction::ImportPasswords {
                profile: "x".repeat(256),
            },
            BrowserAction::FillPassword {
                origin: "https://example.test".into(),
                username: "x".repeat(4096),
            },
            BrowserAction::DeletePasswords {
                origin: "https://example.test:8443".into(),
            },
        ] {
            assert!(action.validate().is_ok());
        }
        let frame = BrowserFrame {
            image: vec![0, 255, 128],
            input_kind: Some(BrowserInputKind::Password),
            ..Default::default()
        };
        assert_eq!(
            crate::protocol::decode::<BrowserFrame>(&crate::protocol::encode(&frame).unwrap())
                .unwrap(),
            frame
        );
        assert!(!format!("{frame:?}").contains("255"));
    }
}

pub fn browser_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input == "about:blank" {
        return Ok(input.into());
    }
    if input.is_empty() {
        return Err("URL を入力してください".into());
    }
    let source = if input.contains("://") {
        input.to_owned()
    } else {
        let host = input.split(['/', '?', '#']).next().unwrap_or_default();
        let local = host == "localhost"
            || host.starts_with("localhost:")
            || host == "127.0.0.1"
            || host.starts_with("127.0.0.1:")
            || host == "0.0.0.0"
            || host.starts_with("0.0.0.0:")
            || host == "[::1]"
            || host.starts_with("[::1]:");
        format!("{}://{input}", if local { "http" } else { "https" })
    };
    let url = url::Url::parse(&source).map_err(|_| "URL が不正です")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("http または https の URL を入力してください".into());
    }
    Ok(url.into())
}

#[cfg(test)]
mod url_tests {
    use super::browser_url;

    #[test]
    fn accepts_web_addresses_and_blank_but_rejects_privileged_schemes_and_credentials() {
        for (input, expected) in [
            (" example.com/path ", "https://example.com/path"),
            ("http://localhost:8080", "http://localhost:8080/"),
            ("about:blank", "about:blank"),
        ] {
            assert_eq!(browser_url(input).unwrap(), expected);
        }
        for input in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:secret@example.com",
            "https://",
        ] {
            assert!(browser_url(input).is_err(), "{input}");
        }
    }
}
