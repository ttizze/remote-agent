/// Host paths are independent of the client's OS. Android's native path parser,
/// for example, does not recognize Windows drive and UNC paths as absolute.
pub fn is_absolute_host_path(path: &str) -> bool {
    if path.starts_with('/') {
        return !path.contains('\0');
    }
    is_windows_absolute_host_path(path)
}

/// Native clients use authenticated file RPCs for T3's attachment assets. The
/// reference names one file; it never carries an absolute Host path.
pub const ATTACHMENT_SCHEME: &str = "bex-attachment://";

pub fn attachment_file_name(source: &str) -> Option<&str> {
    let file = source.strip_prefix(ATTACHMENT_SCHEME)?;
    let (id, extension) = file.rsplit_once('.')?;
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        && !extension.is_empty()
        && extension.len() <= 10
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    .then_some(file)
}

pub fn attachment_extension(name: &str, image_mime: Option<&str>) -> String {
    match image_mime.map(str::to_ascii_lowercase).as_deref() {
        Some("image/png") => return "png".into(),
        Some("image/jpeg") => return "jpg".into(),
        Some("image/gif") => return "gif".into(),
        Some("image/webp") => return "webp".into(),
        _ => {}
    }
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 10
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                && !value.eq_ignore_ascii_case("part")
        })
        .unwrap_or("bin")
        .to_ascii_lowercase()
}

pub fn is_windows_absolute_host_path(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return true;
    }
    if let Some(unc) = path.strip_prefix(r"\\") {
        let mut components = unc.split(['\\', '/']);
        return components.next().is_some_and(|server| !server.is_empty())
            && components.next().is_some_and(|share| !share.is_empty());
    }
    false
}

/// T3's network-path guard is deliberately independent of the current OS.
/// Even filesystem metadata can authenticate to an untrusted Windows share.
pub fn is_network_host_path(path: &str) -> bool {
    path.as_bytes()
        .get(..2)
        .is_some_and(|prefix| prefix.iter().all(|byte| matches!(byte, b'/' | b'\\')))
}

/// A drive prefix is lexical metadata; a bare drive is not an absolute file path.
pub(crate) fn windows_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && (bytes.len() == 2 || matches!(bytes.get(2), Some(b'/' | b'\\')))
}

/// Comparison only, never a path to open. Keep literal percent escapes intact.
pub fn comparison_key(path: &str) -> String {
    let path = path.trim();
    let root = matches!(path, "/" | "\\") || windows_drive(path) && path.len() == 3;
    let trimmed = if root {
        path
    } else if path.starts_with('/') {
        path.trim_end_matches('/')
    } else {
        path.trim_end_matches(['/', '\\'])
    };
    let value = if trimmed.is_empty() {
        path.to_owned()
    } else if windows_drive(trimmed) && trimmed.len() == 2 && path.len() > 2 {
        format!("{trimmed}\\")
    } else {
        trimmed.to_owned()
    };
    if windows_drive(&value) || value.starts_with(r"\\") || is_network_host_path(&value) {
        value.replace('\\', "/").to_lowercase()
    } else {
        value
    }
}

pub fn decode_link_path(source: &str) -> String {
    if source.match_indices('%').any(|(index, _)| {
        source
            .as_bytes()
            .get(index + 1..index + 3)
            .is_none_or(|digits| !digits.iter().all(u8::is_ascii_hexdigit))
    }) {
        return source.into();
    }
    percent_encoding::percent_decode_str(source)
        .decode_utf8()
        .map_or_else(|_| source.to_owned(), |value| value.into_owned())
}

/// Decode a Markdown destination once; file URLs retain their remote host.
pub fn markdown_destination_host_path(source: &str) -> Option<String> {
    let path = if source
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"))
    {
        let url = url::Url::parse(source).ok()?;
        match url
            .host_str()
            .filter(|host| !host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
        {
            Some(host) => format!(r"\\{host}{}", url.path().replace('/', r"\")),
            None => url.path().to_owned(),
        }
    } else {
        source
            .split(['#', '?'])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let path = decode_link_path(&path);
    Some(if path.starts_with('/') && windows_drive(&path[1..]) {
        path[1..].into()
    } else {
        path
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_paths_are_recognized_before_any_platform_path_or_filesystem_calls() {
        for path in [
            "//server/share",
            r"\\server\share",
            "/\\server/share",
            "\\/server/share",
        ] {
            assert!(is_network_host_path(path));
        }
        for path in ["/local", r"C:\local", "~/local", "relative", ""] {
            assert!(!is_network_host_path(path));
        }
    }
    #[test]
    fn host_paths_are_classified_without_using_the_clients_platform() {
        for path in [
            "C:/workspace",
            r"D:\workspace",
            r"\\server\share",
            r"\\server/share/dir",
        ] {
            assert!(is_absolute_host_path(path));
            assert!(is_windows_absolute_host_path(path));
        }
        for path in ["/workspace", "/:data"] {
            assert!(is_absolute_host_path(path));
            assert!(!is_windows_absolute_host_path(path));
        }
        for path in [
            "",
            "C:",
            "relative",
            r"\drive-relative",
            r"\\server",
            "C:/null\0",
        ] {
            assert!(!is_absolute_host_path(path));
            assert!(!is_windows_absolute_host_path(path));
        }
    }
}
