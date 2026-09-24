//! Shared reference resolution and offline sandbox document for both native WebViews.
use std::path::{Component, Path, PathBuf};

pub fn visualization_path(source: &str, cwd: &str) -> Result<PathBuf, String> {
    if source.is_empty() || source.contains('\0') || source.contains("://") {
        return Err("visualize requires a local HTML path".into());
    }
    let path = Path::new(source);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        Path::new(cwd).join(path)
    };
    if !path.is_absolute()
        || !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("html" | "htm")
        )
    {
        return Err("visualize requires an absolute workspace and an HTML file".into());
    }
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    Ok(normalized)
}

/// No origin, native bridge, external resources, forms, popups or top navigation.
/// The outer CSP also constrains fragments that attempt to replace their own policy.
pub fn visualization_document(fragment: &str) -> String {
    let policy = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
    let inner = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\"><style>{}</style><script>{}</script><script>{}</script></head><body><details><summary>Bexでの表示について</summary>選択とプレビューは利用できます。Tweakの調整パネル、Codex専用操作、外部通信は利用できません。選択状態は開き直すと初期値に戻ります。</details>{fragment}</body></html>",
        include_str!("visualize/style.css"),
        include_str!("visualize/lucide.min.js"),
        include_str!("visualize/runtime.js")
    );
    let escaped = inner
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"{}\"><style>html,body,iframe{{margin:0;width:100%;height:100vh;border:0}}iframe{{display:block}}html{{color-scheme:light dark}}</style></head><body><iframe title=\"インタラクティブ表示\" sandbox=\"allow-scripts\" allow=\"camera 'none'; microphone 'none'; geolocation 'none'; clipboard-read 'none'; clipboard-write 'none'\" srcdoc=\"{escaped}\"></iframe></body></html>",
        policy.replace("frame-src 'none'", "frame-src about:")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_reference_resolution_and_sandbox_contract() {
        assert_eq!(
            visualization_path("target/../options.html", "/fixture").unwrap(),
            Path::new("/fixture/options.html")
        );
        for source in [
            "https://example.com/x.html",
            "file:///private/x.html",
            "secrets.txt",
            "",
            "a\0.html",
        ] {
            assert!(visualization_path(source, "/fixture").is_err());
        }
        let doc = visualization_document(include_str!(
            "../../agent-core/tests/fixtures/visualize/icon-options.html"
        ));
        assert!(doc.contains("sandbox=\"allow-scripts\""));
        assert!(!doc.contains("allow-same-origin"));
        assert!(doc.contains("connect-src 'none'"));
        assert!(doc.contains("git-merge"));
        assert!(doc.contains("srcdoc=\"&lt;!doctype"));
    }
}
