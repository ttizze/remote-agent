//! User-captured screen metadata travels with the image as data, never as permission.
use crate::context::bounded;
use serde::{Deserialize, Serialize};

pub const ACCESSIBILITY_TEXT_LIMIT: usize = 32_000;
pub const ACCESSIBILITY_NODE_LIMIT: usize = 10_000;
// Keep nested provider-text and saved-draft JSON below the decoder's depth limit.
const ACCESSIBILITY_DEPTH_LIMIT: usize = 32;
const ICON_PREFIX: &str = "data:image/png;base64,";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotSource {
    pub captured_at_ms: u64,
    pub app_name: String,
    pub window_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_icon_data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<SnapShotAccessibility>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SnapShotAccessibility {
    FlatText {
        text: String,
        truncated: bool,
    },
    /// Bounds are relative to the captured image, including clipped elements.
    ElementTree {
        image_width: u32,
        image_height: u32,
        truncated: bool,
        root: Box<SnapShotNode>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotNode {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub bounds: Option<SnapShotBounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SnapShotState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    pub children: Vec<SnapShotNode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapShotChecked {
    On,
    Off,
    Mixed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<SnapShotChecked>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

fn nonempty(text: &str, limit: usize) -> bool {
    !text.trim().is_empty() && bounded(text, limit)
}

impl SnapShotSource {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !nonempty(&self.app_name, 255)
            || !bounded(&self.window_title, 1000)
            || self
                .app_identifier
                .as_ref()
                .is_some_and(|id| !nonempty(id, 255))
        {
            return Err("invalid snapshot source");
        }
        if self.app_icon_data_url.as_ref().is_some_and(|url| {
            !bounded(url, 100_000)
                || !url
                    .get(..ICON_PREFIX.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(ICON_PREFIX))
        }) {
            return Err("invalid snapshot app icon");
        }
        if let Some(accessibility) = &self.accessibility {
            accessibility.validate()?;
        }
        Ok(())
    }
}

impl SnapShotAccessibility {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::FlatText { text, .. } if !nonempty(text, ACCESSIBILITY_TEXT_LIMIT) => {
                return Err("invalid snapshot accessibility text");
            }
            Self::ElementTree {
                image_width,
                image_height,
                root,
                ..
            } => {
                if *image_width == 0 || *image_height == 0 {
                    return Err("invalid snapshot image size");
                }
                validate_tree(root)?;
                let json = serde_json::to_string(self)
                    .map_err(|_| "invalid snapshot accessibility tree")?;
                if !bounded(&json, ACCESSIBILITY_TEXT_LIMIT) {
                    return Err("snapshot accessibility tree is too large");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_tree(root: &SnapShotNode) -> Result<(), &'static str> {
    let mut stack = vec![(root, 1)];
    let mut count = 0;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > ACCESSIBILITY_NODE_LIMIT || depth > ACCESSIBILITY_DEPTH_LIMIT {
            return Err("snapshot accessibility tree has too many nodes or levels");
        }
        if !nonempty(&node.role, 100)
            || node.name.as_ref().is_some_and(|name| !nonempty(name, 1000))
            || node
                .value
                .as_ref()
                .is_some_and(|value| !nonempty(value, 8000))
            || node
                .description
                .as_ref()
                .is_some_and(|description| !nonempty(description, 2000))
            || node.actions.len() > 32
            || node.actions.iter().any(|action| !nonempty(action, 100))
            || node
                .bounds
                .as_ref()
                .is_some_and(|bounds| bounds.width == 0 || bounds.height == 0)
        {
            return Err("invalid snapshot accessibility node");
        }
        stack.extend(node.children.iter().map(|child| (child, depth + 1)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> SnapShotSource {
        SnapShotSource {
            captured_at_ms: 1,
            app_name: "Editor".into(),
            window_title: "Project".into(),
            app_identifier: None,
            app_icon_data_url: None,
            accessibility: Some(SnapShotAccessibility::FlatText {
                text: "Selected document".into(),
                truncated: false,
            }),
        }
    }
    #[test]
    fn snapshot_source_accepts_image_only_and_rejects_invalid_metadata() {
        let mut source = source();
        source.validate().unwrap();
        source.accessibility = None;
        source.app_icon_data_url = Some("data:image/png;base64,AA==".into());
        source.validate().unwrap();
        source.app_icon_data_url = Some("https://host/icon.png".into());
        assert!(source.validate().is_err());
        source.app_icon_data_url = None;
        source.app_name = " ".into();
        assert!(source.validate().is_err());
    }
    #[test]
    fn text_limits_count_native_utf16_units() {
        let text = "😀".repeat(ACCESSIBILITY_TEXT_LIMIT / 2);
        assert!(
            SnapShotAccessibility::FlatText {
                text: text.clone(),
                truncated: true
            }
            .validate()
            .is_ok()
        );
        assert!(
            SnapShotAccessibility::FlatText {
                text: text + "a",
                truncated: true
            }
            .validate()
            .is_err()
        );
        assert!(
            SnapShotAccessibility::FlatText {
                text: " ".into(),
                truncated: false
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn trees_bound_content_size_node_count_depth_and_dimensions() {
        let node = SnapShotNode {
            role: "window".into(),
            bounds: Some(SnapShotBounds {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
            }),
            ..Default::default()
        };
        let tree = |root| SnapShotAccessibility::ElementTree {
            image_width: 200,
            image_height: 100,
            truncated: false,
            root: Box::new(root),
        };
        tree(node.clone()).validate().unwrap();
        let mut invalid = node.clone();
        invalid.bounds.as_mut().unwrap().width = 0;
        assert!(tree(invalid).validate().is_err());
        let mut large = node.clone();
        large.children = vec![node.clone(); ACCESSIBILITY_NODE_LIMIT - 1];
        validate_tree(&large).unwrap();
        large.children.push(node.clone());
        assert!(tree(large).validate().unwrap_err().contains("too many"));
        let mut deep = node.clone();
        for _ in 1..ACCESSIBILITY_DEPTH_LIMIT {
            deep = SnapShotNode {
                role: "group".into(),
                children: vec![deep],
                ..Default::default()
            };
        }
        tree(deep.clone()).validate().unwrap();
        let context = crate::context::MessageContext {
            id: "capture".into(),
            label: "Editor".into(),
            content: crate::context::ContextContent::SnapShot {
                image_path: "/host/capture.png".into(),
                source: SnapShotSource {
                    accessibility: Some(tree(deep.clone())),
                    ..source()
                },
            },
        };
        context.validate().unwrap();
        let body = crate::context::user_message(
            None,
            vec![crate::items::MessagePart::Text {
                text: context.provider_text(),
            }],
        );
        assert!(
            matches!(body, crate::items::ItemBody::UserMessage {content,..} if content.iter().any(|part| matches!(part, crate::items::MessagePart::Context {context: decoded} if **decoded == context))),
            "the deepest accepted tree survives provider-text decoding"
        );
        deep = SnapShotNode {
            role: "group".into(),
            children: vec![deep],
            ..Default::default()
        };
        assert!(tree(deep).validate().is_err());
        let mut big = node.clone();
        big.children = vec![
            SnapShotNode {
                role: "text".into(),
                value: Some("x".repeat(8000)),
                ..Default::default()
            };
            5
        ];
        assert!(tree(big).validate().unwrap_err().contains("too large"));
        let mut invalid = tree(node);
        if let SnapShotAccessibility::ElementTree { image_height, .. } = &mut invalid {
            *image_height = 0;
        }
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn accessibility_nodes_enforce_each_field_boundary() {
        let valid = SnapShotNode {
            role: "r".repeat(100),
            name: Some("n".repeat(1000)),
            value: Some("v".repeat(8000)),
            description: Some("d".repeat(2000)),
            actions: vec!["a".repeat(100); 32],
            bounds: Some(SnapShotBounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }),
            ..Default::default()
        };
        validate_tree(&valid).unwrap();
        let mutations: [fn(&mut SnapShotNode); 12] = [
            |node| node.role.clear(),
            |node| node.role.push('r'),
            |node| node.name = Some(" ".into()),
            |node| node.name.as_mut().unwrap().push('n'),
            |node| node.value = Some(" ".into()),
            |node| node.value.as_mut().unwrap().push('v'),
            |node| node.description = Some(" ".into()),
            |node| node.description.as_mut().unwrap().push('d'),
            |node| node.actions.push("extra".into()),
            |node| node.actions[0].clear(),
            |node| node.actions[0].push('a'),
            |node| node.bounds.as_mut().unwrap().height = 0,
        ];
        for mutate in mutations {
            let mut invalid = valid.clone();
            mutate(&mut invalid);
            assert!(validate_tree(&invalid).is_err());
        }
    }
}
