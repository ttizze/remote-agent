//! A tool row's surface, icon, source and viewed image, read from the
//! provider-neutral presentation an item carries.
use super::command_label::{js_space, js_trim};
use super::{NativeApp, ToolIcon, ToolSource, ToolSourceKind, ToolSurface};
use agent_domain::{Item, ItemKind, ToolPresentation};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedToolPresentation {
    pub viewed_image_path: Option<String>,
    pub tool_surface: Option<ToolSurface>,
    pub tool_icon: Option<ToolIcon>,
    pub tool_source: Option<ToolSource>,
}

const WORKSPACE_IMAGE_PREVIEW_EXTENSIONS: &[&str] = &[
    ".avif", ".gif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".webp",
];

/// A path the workspace previews as an image, by its literal extension.
pub fn is_workspace_image_preview_path(path: &str) -> bool {
    let path = path.to_lowercase();
    WORKSPACE_IMAGE_PREVIEW_EXTENSIONS
        .iter()
        .any(|extension| path.ends_with(extension))
}

/// A trimmed string no longer than `max_length` UTF-16 code units.
fn trimmed_string(value: Option<&Value>, max_length: usize) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty() && trimmed.encode_utf16().count() <= max_length)
        .then(|| trimmed.to_owned())
}

fn url_with_scheme(value: Option<&Value>, schemes: &[&str]) -> Option<String> {
    let url = url::Url::parse(&trimmed_string(value, 4096)?).ok()?;
    schemes
        .contains(&url.scheme())
        .then(|| url.as_str().to_owned())
}

fn image_url(value: Option<&Value>) -> Option<String> {
    url_with_scheme(value, &["http", "https", "data"])
}

fn page_url(value: Option<&Value>) -> Option<String> {
    url_with_scheme(value, &["http", "https"])
}

fn tag(value: &Value) -> Option<&str> {
    value.get("_tag")?.as_str()
}

fn native_app(value: Option<&Value>) -> Option<NativeApp> {
    let app = value.filter(|app| app.is_object())?;
    let app_id = trimmed_string(app.get("appId"), 512);
    if tag(app) == Some("app-id")
        && let Some(app_id) = app_id.filter(|id| {
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
    {
        return Some(NativeApp::AppId(app_id));
    }
    let display_name = trimmed_string(app.get("displayName"), 160);
    if tag(app) == Some("display-name")
        && let Some(display_name) = display_name
    {
        return Some(NativeApp::DisplayName(display_name));
    }
    None
}

fn activity_icon(value: Option<&Value>) -> Option<ToolIcon> {
    let icon = value.filter(|icon| icon.is_object())?;
    match tag(icon) {
        Some("website") => Some(ToolIcon::Website {
            page_url: page_url(icon.get("pageUrl"))?,
            favicon_url: image_url(icon.get("faviconUrl")),
            favicon_url_dark: image_url(icon.get("faviconUrlDark")),
        }),
        Some("native-app") => native_app(icon.get("app")).map(ToolIcon::NativeApp),
        Some("themed-logo") => Some(ToolIcon::ThemedLogo {
            logo_url: image_url(icon.get("logoUrl"))?,
            logo_url_dark: image_url(icon.get("logoUrlDark")),
        }),
        _ => None,
    }
}

fn activity_source(value: Option<&Value>) -> Option<ToolSource> {
    let source = value.filter(|source| source.is_object())?;
    let key = trimmed_string(source.get("key"), 512)?;
    let name = trimmed_string(source.get("name"), 160)?;
    let kind = match source.get("kind")?.as_str()? {
        "browser" => ToolSourceKind::Browser,
        "computer" => ToolSourceKind::Computer,
        "integration" => ToolSourceKind::Integration,
        _ => return None,
    };
    Some(ToolSource {
        key,
        name,
        kind,
        icon: activity_icon(source.get("icon")),
    })
}

/// The image a Claude `Read` call viewed: its `file_path` or `path` input.
fn read_tool_path(name: &str, input: &Value) -> Option<String> {
    let normalized: String = name
        .to_lowercase()
        .chars()
        .filter(|&c| !js_space(c) && !matches!(c, '_' | '-'))
        .collect();
    if normalized != "read" && normalized != "read file" {
        return None;
    }
    ["file_path", "path"]
        .iter()
        .find_map(|key| trimmed_string(input.get(key), usize::MAX))
        .filter(|path| path.encode_utf16().count() <= 4096)
}

fn from_presentation(presentation: &ToolPresentation) -> ExtractedToolPresentation {
    ExtractedToolPresentation {
        viewed_image_path: None,
        tool_surface: match presentation.surface.as_deref() {
            Some("browser") => Some(ToolSurface::Browser),
            Some("computer") => Some(ToolSurface::Computer),
            _ => None,
        },
        tool_icon: activity_icon(presentation.icon.as_ref().map(|icon| &icon.0)),
        tool_source: activity_source(presentation.source.as_ref().map(|source| &source.0)),
    }
}

/// The presentation fields of a tool item; empty for any other item.
pub fn extract_tool_presentation(item: &Item) -> ExtractedToolPresentation {
    let ItemKind::DynamicTool {
        presentation,
        name,
        input,
        ..
    } = &item.kind
    else {
        return ExtractedToolPresentation::default();
    };
    ExtractedToolPresentation {
        viewed_image_path: read_tool_path(name, &input.0)
            .filter(|path| !path.contains(['\r', '\n']) && is_workspace_image_preview_path(path)),
        ..from_presentation(presentation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{ItemStatus, Json, Timestamp, TurnItemId};
    use serde_json::json;

    fn tool(name: &str, presentation: ToolPresentation, input: Value) -> Item {
        let now = Timestamp::parse("2026-06-20T00:00:00.000Z").unwrap();
        Item {
            id: TurnItemId::new("item-1").unwrap(),
            run: None,
            attempt: None,
            native_key: String::new(),
            ordinal: 0,
            kind: ItemKind::DynamicTool {
                presentation,
                name: name.into(),
                input: Json(input),
                output: None,
            },
            status: ItemStatus::Completed,
            text: String::new(),
            started_at: now.clone(),
            completed_at: Some(now),
            output_omitted: false,
            output_indicates_failure: false,
        }
    }

    fn presented(surface: Option<&str>, icon: Value, source: Value) -> Item {
        tool(
            "mcp__example__open",
            ToolPresentation {
                title: None,
                source: Some(Json(source)),
                surface: surface.map(Into::into),
                icon: Some(Json(icon)),
            },
            json!({}),
        )
    }

    fn read(path: &str) -> Item {
        tool(
            "Read",
            ToolPresentation::default(),
            json!({ "file_path": path }),
        )
    }

    #[test]
    fn retains_valid_image_paths_independently_of_redacted_output() {
        assert_eq!(
            extract_tool_presentation(&read(" /workspace/reference.webp ")),
            ExtractedToolPresentation {
                viewed_image_path: Some("/workspace/reference.webp".into()),
                ..Default::default()
            }
        );
        for path in [
            "/workspace/README.md".to_owned(),
            "/workspace/a.png\nother".to_owned(),
            "x".repeat(4097) + ".png",
        ] {
            assert_eq!(
                extract_tool_presentation(&read(&path)),
                ExtractedToolPresentation::default(),
                "{path}"
            );
        }
    }

    #[test]
    fn reads_provider_neutral_presentation_fields() {
        let item = presented(
            Some("browser"),
            json!({
                "_tag": "website",
                "pageUrl": "https://example.com/docs",
                "faviconUrl": "https://example.com/favicon.png",
                "faviconUrlDark": "https://example.com/favicon-dark.png",
            }),
            json!({
                "key": "integration:example",
                "name": "Example",
                "kind": "integration",
                "icon": {
                    "_tag": "themed-logo",
                    "logoUrl": "https://example.com/logo-light.png",
                    "logoUrlDark": "https://example.com/logo-dark.png",
                },
            }),
        );
        assert_eq!(
            extract_tool_presentation(&item),
            ExtractedToolPresentation {
                viewed_image_path: None,
                tool_surface: Some(ToolSurface::Browser),
                tool_icon: Some(ToolIcon::Website {
                    page_url: "https://example.com/docs".into(),
                    favicon_url: Some("https://example.com/favicon.png".into()),
                    favicon_url_dark: Some("https://example.com/favicon-dark.png".into()),
                }),
                tool_source: Some(ToolSource {
                    key: "integration:example".into(),
                    name: "Example".into(),
                    kind: ToolSourceKind::Integration,
                    icon: Some(ToolIcon::ThemedLogo {
                        logo_url: "https://example.com/logo-light.png".into(),
                        logo_url_dark: Some("https://example.com/logo-dark.png".into()),
                    }),
                }),
            }
        );
    }

    #[test]
    fn reads_provider_neutral_native_app_icons() {
        let item = presented(
            Some("computer"),
            json!({ "_tag": "native-app", "app": { "_tag": "app-id", "appId": "com.example.Editor" } }),
            json!({ "key": "native-app:com.example.editor", "name": "Editor", "kind": "computer" }),
        );
        assert_eq!(
            extract_tool_presentation(&item),
            ExtractedToolPresentation {
                viewed_image_path: None,
                tool_surface: Some(ToolSurface::Computer),
                tool_icon: Some(ToolIcon::NativeApp(NativeApp::AppId(
                    "com.example.Editor".into()
                ))),
                tool_source: Some(ToolSource {
                    key: "native-app:com.example.editor".into(),
                    name: "Editor".into(),
                    kind: ToolSourceKind::Computer,
                    icon: None,
                }),
            }
        );
    }

    #[test]
    fn does_not_infer_presentation_from_provider_specific_payload_data() {
        let item = tool(
            "mcp__computer__click",
            ToolPresentation::default(),
            json!({
                "data": {
                    "item": {
                        "arguments": { "code": "await sky.click({ app: \"Finder\" })" },
                        "result": {
                            "_meta": {
                                "codex/toolSurface": {
                                    "kind": "computerUse",
                                    "app": { "kind": "displayName", "displayName": "Finder" },
                                },
                            },
                        },
                    },
                },
            }),
        );
        assert_eq!(
            extract_tool_presentation(&item),
            ExtractedToolPresentation::default()
        );
    }

    #[test]
    fn rejects_unsafe_or_incomplete_presentation_values() {
        let item = presented(
            Some("terminal"),
            json!({ "_tag": "website", "pageUrl": "javascript:alert(1)" }),
            json!({ "key": "k", "name": "  ", "kind": "integration" }),
        );
        assert_eq!(
            extract_tool_presentation(&item),
            ExtractedToolPresentation::default()
        );
        let item = presented(
            None,
            json!({ "_tag": "native-app", "app": { "_tag": "app-id", "appId": "com example" } }),
            json!({ "key": "k", "name": "Name", "kind": "integration",
                    "icon": { "_tag": "themed-logo", "logoUrl": "data:image/png;base64,AA==" } }),
        );
        assert_eq!(
            extract_tool_presentation(&item),
            ExtractedToolPresentation {
                tool_source: Some(ToolSource {
                    key: "k".into(),
                    name: "Name".into(),
                    kind: ToolSourceKind::Integration,
                    icon: Some(ToolIcon::ThemedLogo {
                        logo_url: "data:image/png;base64,AA==".into(),
                        logo_url_dark: None,
                    }),
                }),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reads_the_viewed_image_only_from_read_tools() {
        let viewed = |item: &Item| extract_tool_presentation(item).viewed_image_path;
        assert_eq!(
            viewed(&tool(
                "read",
                ToolPresentation::default(),
                json!({ "file_path": " ", "path": "/w/Shot.PNG" })
            )),
            Some("/w/Shot.PNG".into())
        );
        assert_eq!(
            viewed(&tool(
                "Write",
                ToolPresentation::default(),
                json!({ "file_path": "/w/a.png" })
            )),
            None
        );
    }
}
