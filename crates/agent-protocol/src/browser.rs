//! Shared browser contract. The Host owns pages; clients render frames and send input.
use serde::{Deserialize, Serialize};

pub const WIDTH: u32 = 1024;
pub const HEIGHT: u32 = 768;

#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserRequest {
    pub thread_id: crate::session::SessionRef,
    pub tab_id: String,
    pub image_id: String,
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
    Navigate {
        url: String,
    },
    Click {
        x: f64,
        y: f64,
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
                && (0.0..f64::from(WIDTH)).contains(&x)
                && (0.0..f64::from(HEIGHT)).contains(&y)
        }
        let valid = match self {
            Self::Navigate { url } => {
                browser_url(url)?;
                url.len() <= 8192
            }
            Self::Click { x, y } => point(*x, *y),
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
            Self::SelectTab { id } => !id.is_empty() && id.len() <= 256,
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

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowserFrame {
    pub tabs: Vec<BrowserTab>,
    pub tab_id: String,
    pub width: u32,
    pub height: u32,
    #[serde(with = "crate::protocol::bytes")]
    pub image: Vec<u8>,
    pub image_id: String,
    pub dialog: Option<BrowserDialog>,
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
        self.action.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_contract_rejects_invalid_input_and_preserves_binary_images() {
        for action in [
            BrowserAction::Click {
                x: f64::NAN,
                y: 0.0,
            },
            BrowserAction::Click { x: 1024.0, y: 0.0 },
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
        let frame = BrowserFrame {
            image: vec![0, 255, 128],
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
        format!("https://{input}")
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
