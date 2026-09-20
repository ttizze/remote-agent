//! Shared browser contract. The Host owns pages and control; clients render frames.
use serde::{Deserialize, Serialize};

pub const WIDTH: u32 = 1024;
pub const HEIGHT: u32 = 768;

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BrowserRequest {
    pub thread_id: String,
    pub control_token: String,
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BrowserAction {
    Read,
    TakeControl,
    ReleaseControl,
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
                crate::presentation::browser::browser_url(url)?;
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BrowserControl {
    #[default]
    Agent,
    AwaitingHuman,
    Yours,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BrowserTab {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BrowserDialog {
    pub message: String,
    pub prompt: bool,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BrowserFrame {
    pub tabs: Vec<BrowserTab>,
    pub tab_id: String,
    pub control: BrowserControl,
    pub control_token: String,
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
            .field("control", &self.control)
            .field("image_bytes", &self.image.len())
            .finish_non_exhaustive()
    }
}

impl BrowserRequest {
    pub fn validate(&self) -> Result<(), String> {
        crate::session::SessionRef::from_thread_id(&self.thread_id)?;
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
