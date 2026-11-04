pub(super) fn file_mime_type(path: &str) -> Option<&'static str> {
    let name = super::super::markdown::file_basename(path);
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    FILE_MIME_TYPES
        .iter()
        .find(|(suffix, _)| *suffix == extension)
        .map(|(_, mime)| *mime)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn file_preview_truncation_notice() -> String {
    "先頭1 MiBだけ表示しています。全文はダウンロードできます。".into()
}

/// Resolved binary files use authenticated downloads and the platform viewer,
/// bypassing the UTF-8 editor's size limit. HTML and notebook JSON stay editable;
/// executable Java archives are only available through explicit downloads.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn file_preview_mime_type(path: String) -> Option<String> {
    if path.ends_with(['/', '\\']) {
        return None;
    }
    file_mime_type(&path)
        .filter(|mime| {
            !matches!(
                *mime,
                "text/html" | "application/x-ipynb+json" | "application/java-archive"
            )
        })
        .map(str::to_owned)
}

const FILE_MIME_TYPES: &[(&str, &str)] = &[
    ("3gp", "video/3gpp"),
    ("7z", "application/x-7z-compressed"),
    ("aac", "audio/aac"),
    ("aiff", "audio/aiff"),
    ("aif", "audio/aiff"),
    ("avi", "video/x-msvideo"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("bz2", "application/x-bzip2"),
    ("caf", "audio/x-caf"),
    ("db", "application/vnd.sqlite3"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("epub", "application/epub+zip"),
    ("flac", "audio/flac"),
    ("gif", "image/gif"),
    ("gz", "application/gzip"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("ico", "image/x-icon"),
    ("ipynb", "application/x-ipynb+json"),
    ("jar", "application/java-archive"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("m4a", "audio/mp4"),
    ("m4v", "video/mp4"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("ogv", "video/ogg"),
    ("opus", "audio/ogg"),
    ("otf", "font/otf"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("rtf", "application/rtf"),
    ("rar", "application/vnd.rar"),
    ("sqlite", "application/vnd.sqlite3"),
    ("sqlite3", "application/vnd.sqlite3"),
    ("svg", "image/svg+xml"),
    ("tar", "application/x-tar"),
    ("tgz", "application/gzip"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("ttf", "font/ttf"),
    ("wav", "audio/wav"),
    ("weba", "audio/webm"),
    ("webm", "video/webm"),
    ("webm-audio", "audio/webm"),
    ("webp", "image/webp"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("zip", "application/zip"),
    ("xz", "application/x-xz"),
    ("zst", "application/zstd"),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[rstest::rstest]
    #[case("/Users/tt/Movies/bex-promo-ja.mp4", Some("video/mp4"))]
    #[case(r"C:\Movies\CLIP.MOV", Some("video/quicktime"))]
    #[case("/repo/voice.m4a", Some("audio/mp4"))]
    #[case("/repo/photo.PNG", Some("image/png"))]
    #[case("/repo/document.pdf", Some("application/pdf"))]
    #[case("/repo/archive.zip", Some("application/zip"))]
    #[case("/repo/old.DOC", Some("application/msword"))]
    #[case("/repo/book.xls", Some("application/vnd.ms-excel"))]
    #[case("/repo/deck.ppt", Some("application/vnd.ms-powerpoint"))]
    #[case("/repo/bundle.7z", Some("application/x-7z-compressed"))]
    #[case("/repo/run.jar", None)]
    #[case("/repo/run.exe", None)]
    #[case("/repo/source.rs", None)]
    #[case("/repo/notes.md", None)]
    #[case("/repo/plain.txt", None)]
    #[case("/repo/index.HTML", None)]
    #[case("/repo/index.htm", None)]
    #[case("/repo/notebook.ipynb", None)]
    #[case("/repo/video.mp4.txt", None)]
    #[case("/repo/video.mp4/readme", None)]
    #[case("/repo/video.mp4/", None)]
    fn binary_files_bypass_text_reads(#[case] path: &str, #[case] mime: Option<&str>) {
        assert_eq!(file_preview_mime_type(path.into()).as_deref(), mime);
    }
}
