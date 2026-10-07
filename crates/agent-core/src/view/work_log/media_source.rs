//! Where the bytes of an authored image or video source are loaded from.
//! Filesystem paths belong to the Host and never reach an image view directly.
use crate::presentation::markdown::links::{
    file_basename, is_windows_absolute_path, normalize_markdown_link_destination,
    parse_file_url_href, safe_decode_uri_component, split_markdown_link_search_and_hash,
    strip_slash_prefixed_windows_drive,
};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MarkdownImageSource {
    Direct { uri: String },
    WorkspaceFile { path: String },
    Blocked,
}

static DIRECT_IMAGE_SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:https?:|data:|blob:|//)").expect("direct source pattern compiles")
});
static URI_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("scheme pattern compiles"));
static POSITION_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":[0-9]+(?::[0-9]+)?$").expect("position pattern compiles"));

fn join_workspace_path(workspace_root: &str, relative_path: &str) -> String {
    let separator = if is_windows_absolute_path(workspace_root) {
        "\\"
    } else {
        "/"
    };
    let root = workspace_root.trim_end_matches(['\\', '/']);
    let path = relative_path.replace(['\\', '/'], separator);
    let path = path.trim_start_matches(['\\', '/']);
    format!("{root}{separator}{path}")
}

/// Classifies an image or video source by where its bytes must be loaded from.
pub fn classify_markdown_image_source(
    value: Option<&str>,
    workspace_root: Option<&str>,
) -> MarkdownImageSource {
    let Some(value) = value else {
        return MarkdownImageSource::Blocked;
    };
    let source = normalize_markdown_link_destination(value);
    let source = source.as_str();
    if source.is_empty() || source.starts_with('#') || source.starts_with('?') {
        return MarkdownImageSource::Blocked;
    }
    if DIRECT_IMAGE_SOURCE.is_match(source) {
        return MarkdownImageSource::Direct { uri: source.into() };
    }
    if source
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        return match parse_file_url_href(source).map(|url| url.path) {
            Some(path) => MarkdownImageSource::WorkspaceFile {
                path: strip_slash_prefixed_windows_drive(&safe_decode_uri_component(&path)),
            },
            None => MarkdownImageSource::Blocked,
        };
    }
    let decoded = safe_decode_uri_component(&split_markdown_link_search_and_hash(source).path);
    let path = strip_slash_prefixed_windows_drive(&decoded);
    let path = path.as_str();
    if path.is_empty() {
        return MarkdownImageSource::Blocked;
    }
    if path.starts_with('/') || is_windows_absolute_path(path) {
        return MarkdownImageSource::WorkspaceFile { path: path.into() };
    }
    if URI_SCHEME.is_match(path) || path.starts_with("~/") || path.starts_with("~\\") {
        return MarkdownImageSource::Blocked;
    }
    match workspace_root.filter(|root| !root.is_empty()) {
        Some(root) => MarkdownImageSource::WorkspaceFile {
            path: join_workspace_path(root, path),
        },
        None => MarkdownImageSource::Blocked,
    }
}

pub fn markdown_image_source_fragment(source: &str) -> String {
    split_markdown_link_search_and_hash(&normalize_markdown_link_destination(source)).hash
}

/// The image or video type of a literal filesystem extension such as `.png`.
pub(crate) fn media_mime_type_from_extension(extension: &str) -> Option<&'static str> {
    let name = extension.strip_prefix('.')?;
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    Some(match name.to_ascii_lowercase().as_str() {
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avi" => "video/x-msvideo",
        "m4v" | "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// A Host file an authored media source names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMedia {
    /// The path without a `:line:column` suffix.
    pub path: String,
    pub name: String,
    pub mime_type: String,
    pub src_fragment: String,
}

/// The media a source resolved to `resolved_path` names, or `None` when the
/// path has no image or video extension.
pub(crate) fn workspace_media(source: &str, resolved_path: &str) -> Option<WorkspaceMedia> {
    let path = match POSITION_SUFFIX.find(resolved_path) {
        Some(suffix) => &resolved_path[..suffix.start()],
        None => resolved_path,
    };
    let basename = file_basename(path);
    let basename = basename.as_str();
    let mime_type = media_mime_type_from_extension(&basename[basename.rfind('.')?..])?;
    let windows = is_windows_absolute_path(path) || path.starts_with("//");
    let reference_name = if windows {
        path.rsplit(['\\', '/']).next()
    } else {
        path.rsplit('/').next()
    }
    .filter(|name| !name.is_empty());
    let kind = if mime_type.starts_with("video/") {
        "video"
    } else {
        "image"
    };
    let name = reference_name
        .or((!basename.is_empty()).then_some(basename))
        .unwrap_or(kind);
    Some(WorkspaceMedia {
        path: path.into(),
        name: name.into(),
        mime_type: mime_type.into(),
        src_fragment: markdown_image_source_fragment(source),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("https://example.com/image.png")]
    #[case("HTTP://example.com/image.png")]
    #[case("data:image/png;base64,AAAA")]
    #[case("blob:https://app.example.com/image-id")]
    #[case("//cdn.example.com/image.png")]
    fn keeps_directly_loadable(#[case] uri: &str) {
        assert_eq!(
            classify_markdown_image_source(Some(uri), Some("/workspace/project")),
            MarkdownImageSource::Direct { uri: uri.into() }
        );
    }

    #[rstest]
    #[case(
        "images/result.png",
        Some("/workspace/project"),
        "/workspace/project/images/result.png"
    )]
    #[case(
        "./images/result.png",
        Some("/workspace/project"),
        "/workspace/project/./images/result.png"
    )]
    #[case(
        "images/result.png",
        Some("C:\\Users\\dara\\project"),
        "C:\\Users\\dara\\project\\images\\result.png"
    )]
    #[case(
        "images\\result.png",
        Some("C:\\Users\\dara\\project"),
        "C:\\Users\\dara\\project\\images\\result.png"
    )]
    #[case("/workspace/project/image.png", None, "/workspace/project/image.png")]
    #[case(
        "/C:/Users/dara/project/image.png",
        None,
        "C:/Users/dara/project/image.png"
    )]
    #[case(
        "C:/Users/dara/project/image.png",
        None,
        "C:/Users/dara/project/image.png"
    )]
    #[case("\\\\server\\share\\image.png", None, "\\\\server\\share\\image.png")]
    #[case(
        "file:///workspace/project/image%20one.png",
        None,
        "/workspace/project/image one.png"
    )]
    #[case(
        "file:///C:/Users/dara/project/image.png",
        None,
        "C:/Users/dara/project/image.png"
    )]
    #[case(
        "file://localhost/C:/Users/dara/project/image.png",
        None,
        "C:/Users/dara/project/image.png"
    )]
    #[case("file://server/share/image.png", None, "\\\\server\\share\\image.png")]
    fn maps_to_a_workspace_file(
        #[case] source: &str,
        #[case] workspace_root: Option<&str>,
        #[case] path: &str,
    ) {
        assert_eq!(
            classify_markdown_image_source(Some(source), workspace_root),
            MarkdownImageSource::WorkspaceFile { path: path.into() }
        );
    }

    #[rstest]
    #[case(None)]
    #[case(Some(""))]
    #[case(Some("#image"))]
    #[case(Some("?image=1"))]
    #[case(Some("image.png"))]
    #[case(Some("~/image.png"))]
    #[case(Some("javascript:alert(1)"))]
    #[case(Some("ftp://example.com/image.png"))]
    #[case(Some("content://media/image/1"))]
    #[case(Some("custom:image.png"))]
    #[case(Some("file://%"))]
    fn blocks_unsupported_or_unresolved_source(#[case] source: Option<&str>) {
        assert_eq!(
            classify_markdown_image_source(source, None),
            MarkdownImageSource::Blocked
        );
    }

    #[rstest]
    #[case("<icons.svg?version=2#logo>", "#logo")]
    #[case("icons.svg?version=2", "")]
    fn extracts_the_fragment(#[case] source: &str, #[case] fragment: &str) {
        assert_eq!(markdown_image_source_fragment(source), fragment);
    }
}
