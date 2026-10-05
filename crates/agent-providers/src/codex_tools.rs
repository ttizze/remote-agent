//! Codex dynamic and MCP tool items: qualified names, native results and
//! browser or computer presentation.
use crate::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn http_url(value: &Value) -> Option<String> {
    let raw = value.as_str().filter(|raw| raw.len() <= 4096)?;
    let url = url::Url::parse(raw).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.as_str().len() <= 4096)
        .then(|| url.as_str().to_owned())
}
fn image_url(value: &Value) -> Option<String> {
    let raw = value.as_str().filter(|raw| raw.len() <= 4096)?;
    let url = url::Url::parse(raw).ok()?;
    matches!(url.scheme(), "http" | "https" | "data").then(|| url.as_str().to_owned())
}
fn display_name(value: &Value) -> Option<String> {
    let name = value
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty() && name.encode_utf16().count() <= 160).then_some(name)
}
fn app_id(value: &Value) -> Option<String> {
    let id = value.as_str()?.trim();
    (!id.is_empty()
        && id.len() <= 512
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    .then(|| id.to_owned())
}
fn native_app_key(app_id: &str) -> String {
    let key = format!("native-app:{}", app_id.to_lowercase());
    if key.len() <= 512 {
        return key;
    }
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    format!("{}:{digest}", &key[..512 - digest.len() - 1])
}
fn browser_name(value: &Value) -> Option<String> {
    let name = display_name(value)?;
    let lower = name.to_lowercase();
    Some(
        if lower.contains("chrome") || lower == "chromium" {
            "Chrome"
        } else if lower.contains("edge") {
            "Microsoft Edge"
        } else if lower.contains("firefox") {
            "Firefox"
        } else if lower.contains("safari") {
            "Safari"
        } else if lower.contains("arc") {
            "Arc"
        } else if lower == "iab" || lower.contains("in-app") {
            "Browser"
        } else {
            return Some(name);
        }
        .into(),
    )
}
fn browser_app(name: &str) -> Option<Value> {
    match name {
        "Chrome" => Some(json!({"_tag":"display-name","displayName":"Google Chrome"})),
        "Microsoft Edge" | "Firefox" | "Safari" | "Arc" => {
            Some(json!({"_tag":"display-name","displayName":name}))
        }
        _ => None,
    }
}
fn app_name_from_id(app_id: &str) -> Option<&'static str> {
    Some(match app_id.to_lowercase().as_str() {
        "com.apple.finder" => "Finder",
        "com.apple.safari" => "Safari",
        "com.google.chrome" => "Chrome",
        "com.microsoft.edgemac" => "Microsoft Edge",
        "org.mozilla.firefox" => "Firefox",
        "company.thebrowser.browser" => "Arc",
        _ => return None,
    })
}
fn native_app(value: &Value) -> Option<Value> {
    match value["kind"].as_str()? {
        "appId" => app_id(&value["appId"]).map(|id| json!({"_tag":"app-id","appId":id})),
        "displayName" => display_name(&value["displayName"])
            .map(|name| json!({"_tag":"display-name","displayName":name})),
        _ => None,
    }
}
fn themed_logo(records: &[&Value]) -> Option<Value> {
    records.iter().find_map(|record| {
        let logo = image_url(&record["logoUrl"])?;
        let mut icon = json!({"_tag":"themed-logo","logoUrl":logo});
        let dark = if record["logoUrlDark"].is_null() {
            &record["logoDarkUrl"]
        } else {
            &record["logoUrlDark"]
        };
        if let Some(dark) = image_url(dark) {
            icon["logoUrlDark"] = json!(dark);
        }
        Some(icon)
    })
}
fn first<'a>(record: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter()
        .map(|key| &record[*key])
        .find(|value| !value.is_null())
        .unwrap_or(&Value::Null)
}
fn mcp_presentation(item: &Value) -> ToolPresentation {
    let metadata = &item["result"]["_meta"];
    let surface = &metadata["codex/toolSurface"];
    let app_context = &item["appContext"];
    let logo = themed_logo(&[surface, &metadata["source"], app_context]);
    match surface["kind"].as_str() {
        Some("browserUse") => {
            let open_tab = surface["openTabs"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|tab| http_url(&tab["url"]).is_some())
                .unwrap_or(&Value::Null);
            let browser_use = &metadata["browser_use"];
            let page = [
                (&surface["screenshot"], &surface["screenshot"]["pageUrl"]),
                (browser_use, &browser_use["url"]),
                (open_tab, &open_tab["url"]),
            ]
            .into_iter()
            .find_map(|(record, url)| http_url(url).map(|url| (record, url)));
            let name = browser_name(&app_context["appName"])
                .or_else(|| browser_name(&surface["browserFamily"]))
                .or_else(|| browser_name(&surface["backend"]))
                .unwrap_or_else(|| "Browser".into());
            let icon = page.map(|(record, url)| {
                let mut icon = json!({"_tag":"website","pageUrl":url});
                if let Some(favicon) = image_url(first(record, &["faviconUrl", "favIconUrl"])) {
                    icon["faviconUrl"] = json!(favicon);
                }
                if let Some(dark) = image_url(first(record, &["faviconUrlDark", "favIconUrlDark"]))
                {
                    icon["faviconUrlDark"] = json!(dark);
                }
                Json(icon)
            });
            let mut source = json!({
                "key": format!("browser-use:{}", match name.trim().to_lowercase() {
                    key if key.is_empty() => "browser".into(),
                    key => key,
                }),
                "name": name,
                "kind": if name == "Browser" { "browser" } else { "integration" },
            });
            if let Some(source_icon) = logo
                .or_else(|| browser_app(&name).map(|app| json!({"_tag":"native-app","app":app})))
            {
                source["icon"] = source_icon;
            }
            ToolPresentation {
                title: None,
                source: Some(Json(source)),
                surface: Some("browser".into()),
                icon,
            }
        }
        Some("computerUse") => {
            let app = native_app(&surface["app"]);
            let arguments = &item["arguments"];
            let name = display_name(&app_context["appName"])
                .or_else(|| display_name(&arguments["appName"]))
                .or_else(|| display_name(&arguments["application"]))
                .or_else(|| display_name(&arguments["app"]))
                .or_else(|| {
                    app.as_ref().and_then(|app| match app["_tag"].as_str() {
                        Some("display-name") => app["displayName"].as_str().map(str::to_owned),
                        _ => app["appId"]
                            .as_str()
                            .and_then(app_name_from_id)
                            .map(str::to_owned),
                    })
                })
                .unwrap_or_else(|| "Computer Use".into());
            let key = match &app {
                Some(app) if app["_tag"] == "app-id" => native_app_key(&string(app, "appId")),
                Some(app) => format!(
                    "native-app-name:{}",
                    string(app, "displayName").trim().to_lowercase()
                ),
                None => "computer-use".into(),
            };
            let mut source = json!({"key":key,"name":name,"kind":"computer"});
            if let Some(source_icon) = logo.or_else(|| {
                app.clone()
                    .map(|app| json!({"_tag":"native-app","app":app}))
            }) {
                source["icon"] = source_icon;
            }
            ToolPresentation {
                title: None,
                source: Some(Json(source)),
                surface: Some("computer".into()),
                icon: app.map(|app| Json(json!({"_tag":"native-app","app":app}))),
            }
        }
        _ => ToolPresentation::default(),
    }
}
fn tool_title(name: &str, input: &Value) -> Option<String> {
    let trimmed = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    match name {
        "cua_repl.js" => trimmed(&input["title"]),
        "Skill" => trimmed(&input["skill"]).map(|skill| format!("Skill: {skill}")),
        _ => None,
    }
}
pub(crate) fn codex_tool(item: &Value) -> ProviderItem {
    let mcp = item["type"] == "mcpToolCall";
    let name = if mcp {
        format!("{}.{}", string(item, "server"), string(item, "tool"))
    } else {
        [
            item["namespace"].as_str().map(str::trim),
            item["tool"].as_str(),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
    };
    let output = if mcp {
        let result = &item["result"];
        let result = (!result.is_null()).then(|| {
            if result["structuredContent"].is_null() {
                result["content"].clone()
            } else {
                result["structuredContent"].clone()
            }
        });
        match (&item["error"]["message"], result) {
            (Value::Null, result) if item["error"].is_null() => result,
            (message, None) => Some(json!({"error":message})),
            (message, Some(result)) => Some(json!({"error":message,"result":result})),
        }
    } else if !item["contentItems"].is_null() {
        Some(item["contentItems"].clone())
    } else if item["success"] == false {
        Some(json!({"success":false}))
    } else {
        None
    };
    let mut presentation = if mcp {
        mcp_presentation(item)
    } else {
        ToolPresentation::default()
    };
    presentation.title = tool_title(&name, &item["arguments"]);
    ProviderItem::Tool {
        presentation,
        name,
        input: Json(item["arguments"].clone()),
        output: output.map(Json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool(item: Value) -> (String, Option<Value>, ToolPresentation) {
        match codex_tool(&item) {
            ProviderItem::Tool {
                name,
                output,
                presentation,
                ..
            } => (name, output.map(|o| o.0), presentation),
            other => panic!("{other:?}"),
        }
    }
    // T3 CodexAdapterV2.test.ts dynamic and MCP tool projections.
    #[test]
    fn tools_keep_qualified_names_native_results_and_presentation() {
        let (name, output, _) = tool(
            json!({"type":"mcpToolCall","server":"t3-code","tool":"create_threads","arguments":{"threads":[]},"result":{"content":[{"type":"text"}],"structuredContent":{"threads":[{"threadId":"thread:mcp:fixture:0"}]}}}),
        );
        assert_eq!(name, "t3-code.create_threads");
        assert_eq!(
            output,
            Some(json!({"threads":[{"threadId":"thread:mcp:fixture:0"}]}))
        );
        let (name, output, _) = tool(
            json!({"type":"dynamicToolCall","namespace":"workspace","tool":"inspect","arguments":{"path":"package.json"},"status":"failed","contentItems":[{"type":"inputText","text":"inspection failed"}],"success":false}),
        );
        assert_eq!(name, "workspace.inspect");
        assert_eq!(
            output,
            Some(json!([{"type":"inputText","text":"inspection failed"}]))
        );
        let (name, output, _) = tool(
            json!({"type":"dynamicToolCall","namespace":null,"tool":"wait","arguments":{},"success":false}),
        );
        assert_eq!(
            (name.as_str(), output),
            ("wait", Some(json!({"success":false})))
        );
        let (_, output, _) = tool(
            json!({"type":"mcpToolCall","server":"github","tool":"x","arguments":{},"error":{"message":"denied"}}),
        );
        assert_eq!(output, Some(json!({"error":"denied"})));
        let (_, _, presentation) = tool(
            json!({"type":"dynamicToolCall","tool":"cua_repl.js","arguments":{"title":"Inspect Saga music screen"}}),
        );
        assert_eq!(
            presentation.title.as_deref(),
            Some("Inspect Saga music screen")
        );
        let (_, _, presentation) =
            tool(json!({"type":"dynamicToolCall","tool":"cua_repl.js","arguments":{"title":"  "}}));
        assert_eq!(presentation.title, None);
        let (_, _, browser) = tool(
            json!({"type":"mcpToolCall","server":"browser","tool":"open","arguments":{},"appContext":{"appName":"Google Chrome"},"result":{"_meta":{"codex/toolSurface":{"kind":"browserUse","screenshot":{"pageUrl":"https://example.com/docs","faviconUrl":"https://example.com/icon.png"}}}}}),
        );
        assert_eq!(browser.surface.as_deref(), Some("browser"));
        assert_eq!(
            browser.icon.unwrap().0,
            json!({"_tag":"website","pageUrl":"https://example.com/docs","faviconUrl":"https://example.com/icon.png"})
        );
        assert_eq!(browser.source.unwrap().0["name"], "Chrome");
        let (_, _, finder) = tool(
            json!({"type":"mcpToolCall","server":"computer","tool":"click","arguments":{},"result":{"_meta":{"codex/toolSurface":{"kind":"computerUse","app":{"kind":"appId","appId":"com.apple.finder"}}}}}),
        );
        assert_eq!(
            finder.icon.unwrap().0,
            json!({"_tag":"native-app","app":{"_tag":"app-id","appId":"com.apple.finder"}})
        );
        assert_eq!(finder.source.unwrap().0["name"], "Finder");
    }
}
