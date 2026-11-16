use agent_protocol::snapshot::{SnapShotAccessibility, SnapShotSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SnapShotDetailsFormat {
    Text,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SnapShotDetails {
    pub content: String,
    pub format: SnapShotDetailsFormat,
    pub truncated: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn snapshot_accessibility_details(source: SnapShotSource) -> Option<SnapShotDetails> {
    source.validate().ok()?;
    match source.accessibility? {
        SnapShotAccessibility::FlatText { text, truncated } => Some(SnapShotDetails {
            content: text,
            format: SnapShotDetailsFormat::Text,
            truncated,
        }),
        tree @ SnapShotAccessibility::ElementTree { truncated, .. } => Some(SnapShotDetails {
            content: serde_json::to_string_pretty(&tree).ok()?,
            format: SnapShotDetailsFormat::Json,
            truncated,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn details_keep_accessibility_format_and_truncation() {
        let mut source = SnapShotSource {
            captured_at_ms: 1,
            app_name: "Editor".into(),
            window_title: "".into(),
            app_identifier: None,
            app_icon_data_url: None,
            accessibility: None,
        };
        assert_eq!(snapshot_accessibility_details(source.clone()), None);
        source.accessibility = Some(SnapShotAccessibility::FlatText {
            text: "Document".into(),
            truncated: true,
        });
        let text = snapshot_accessibility_details(source.clone()).unwrap();
        assert_eq!(text.format, SnapShotDetailsFormat::Text);
        assert_eq!(text.content, "Document");
        assert!(text.truncated);
        source.accessibility = Some(SnapShotAccessibility::ElementTree {
            image_width: 200,
            image_height: 100,
            truncated: false,
            root: Box::new(agent_protocol::snapshot::SnapShotNode {
                role: "window".into(),
                ..Default::default()
            }),
        });
        let tree = snapshot_accessibility_details(source).unwrap();
        assert_eq!(tree.format, SnapShotDetailsFormat::Json);
        assert!(!tree.truncated);
        assert!(tree.content.contains("\"role\": \"window\""));
    }
}
