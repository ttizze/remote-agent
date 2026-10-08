//! Discover local project branding, following T3 Code's common icon locations.
use base64::Engine;
use std::{
    io::{Cursor, Read},
    path::Path,
    sync::LazyLock,
};

const ICON_SIZE: u32 = 64;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const CANDIDATES: &[&str] = &[
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "app/favicon.ico",
    "app/favicon.png",
    "app/icon.svg",
    "app/icon.png",
    "app/icon.ico",
    "src/favicon.ico",
    "src/favicon.svg",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    ".idea/icon.svg",
];
const SOURCES: &[&str] = &[
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];

/// Optional branding must never prevent opening the task list. Resolve afresh on
/// refresh, and keep file reads and image decoding off the async executor.
pub(crate) fn resolve(root: &Path) -> Option<String> {
    if !root.is_absolute() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let mut candidates = CANDIDATES.iter().map(|path| (*path).to_owned()).chain(
        SOURCES
            .iter()
            .filter_map(|source| {
                let bytes = read_within(&root, source)?;
                icon_href(std::str::from_utf8(&bytes).ok()?)
            })
            .flat_map(|href| {
                let href = href.trim_start_matches('/');
                [format!("public/{href}"), href.to_owned()]
            }),
    );
    candidates
        .find_map(|path| {
            let bytes = read_within(&root, &path)?;
            thumbnail(
                &bytes,
                Path::new(&path).extension().is_some_and(|ext| ext == "svg"),
            )
        })
        .map(|png| base64::engine::general_purpose::STANDARD.encode(png))
}

fn read_within(root: &Path, relative: &str) -> Option<Vec<u8>> {
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

fn icon_href(source: &str) -> Option<String> {
    static LINKS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)<link\b[^>]*>").unwrap());
    static REL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\brel\s*([=:])\s*["'](?:icon|shortcut icon)["']"#).unwrap()
    });
    static HREF: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)\bhref\s*[=:]\s*["']([^"'?#]+)"#).unwrap());
    LINKS
        .find_iter(source)
        .map(|tag| tag.as_str())
        .filter(|entry| REL.is_match(entry))
        .find_map(|entry| HREF.captures(entry).map(|capture| capture[1].to_owned()))
        .or_else(|| {
            source
                .split(['{', '}'])
                .filter(|entry| {
                    REL.captures(entry)
                        .is_some_and(|capture| &capture[1] == ":")
                })
                .find_map(|entry| HREF.captures(entry).map(|capture| capture[1].to_owned()))
        })
}

fn thumbnail(bytes: &[u8], svg: bool) -> Option<Vec<u8>> {
    if svg {
        let options = resvg::usvg::Options {
            // Project icons are self-contained; don't load referenced host files.
            image_href_resolver: resvg::usvg::ImageHrefResolver {
                resolve_data: Box::new(|_, _, _| None),
                resolve_string: Box::new(|_, _| None),
            },
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_data(bytes, &options).ok()?;
        let size = tree.size();
        let scale = ICON_SIZE as f32 / size.width().max(size.height());
        let mut pixmap = resvg::tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE)?;
        let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(
            (ICON_SIZE as f32 - size.width() * scale) / 2.,
            (ICON_SIZE as f32 - size.height() * scale) / 2.,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        pixmap.encode_png().ok()
    } else {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?.thumbnail(ICON_SIZE, ICON_SIZE);
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, image::ImageFormat::Png).ok()?;
        Some(output.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="64"><rect width="128" height="64" fill="#ff0000"/></svg>"##;

    fn decoded(root: &Path) -> image::RgbaImage {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(resolve(root).unwrap())
            .unwrap();
        image::load_from_memory(&bytes).unwrap().into_rgba8()
    }

    #[test]
    fn resolves_priority_and_refreshes_changed_or_deleted_branding() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join("public")).unwrap();
        image::RgbaImage::from_pixel(256, 256, image::Rgba([0, 0, 255, 255]))
            .save(root.join("public/favicon.png"))
            .unwrap();
        std::fs::write(root.join("favicon.svg"), SVG).unwrap();
        let icon = decoded(root);
        assert_eq!(icon.dimensions(), (64, 64));
        assert_eq!(icon.get_pixel(32, 32).0, [255, 0, 0, 255]);
        assert_eq!(icon.get_pixel(32, 0).0[3], 0);
        std::fs::write(root.join("favicon.svg"), "invalid SVG").unwrap();
        assert_eq!(decoded(root).get_pixel(32, 32).0, [0, 0, 255, 255]);
        std::fs::remove_file(root.join("public/favicon.png")).unwrap();
        assert!(resolve(root).is_none());
    }

    #[test]
    fn resolves_html_and_route_metadata_and_normalizes_ico() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir_all(root.join("public/brand")).unwrap();
        image::RgbaImage::from_pixel(32, 32, image::Rgba([0, 255, 0, 255]))
            .save(root.join("public/brand/app.ico"))
            .unwrap();
        for source in [
            r#"<link href="/brand/app.ico?v=2" rel="shortcut icon">"#,
            r#"const links = [{ rel: 'icon', href: '/brand/app.ico' }];"#,
        ] {
            std::fs::write(root.join("index.html"), source).unwrap();
            assert_eq!(decoded(root).get_pixel(16, 16).0, [0, 255, 0, 255]);
        }
        assert!(
            icon_href(r#"<link rel="icon"><link rel="stylesheet" href="/style.css">"#).is_none()
        );
        assert!(icon_href(r#"[{rel: 'icon'}, {href: '/unrelated.png'}]"#).is_none());
    }

    #[test]
    fn rejects_outside_workspace_and_oversized_or_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(directory.path().join("private.svg"), SVG).unwrap();
        std::fs::write(
            root.join("index.html"),
            r#"<link rel="icon" href="../private.svg">"#,
        )
        .unwrap();
        assert!(resolve(&root).is_none());
        let file = std::fs::File::create(root.join("favicon.png")).unwrap();
        let mut svg = SVG.as_bytes().to_vec();
        svg.resize(MAX_FILE_BYTES as usize, b' ');
        std::fs::write(root.join("favicon.svg"), &svg).unwrap();
        assert_eq!(decoded(&root).get_pixel(32, 32).0, [255, 0, 0, 255]);
        svg.push(b' ');
        std::fs::write(root.join("favicon.svg"), svg).unwrap();
        assert!(resolve(&root).is_none());
        std::fs::remove_file(root.join("favicon.svg")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(resolve(&root).is_none());
        assert!(resolve(Path::new(".")).is_none());
        std::fs::write(
            root.join("favicon.svg"),
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><image href="{}" width="64" height="64"/></svg>"#,
                directory.path().join("private.svg").display()
            ),
        )
        .unwrap();
        assert!(decoded(&root).pixels().all(|pixel| pixel.0[3] == 0));
        std::fs::remove_file(root.join("favicon.svg")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                directory.path().join("private.svg"),
                root.join("favicon.svg"),
            )
            .unwrap();
            assert!(resolve(&root).is_none());
        }
    }

    proptest::proptest! {
        #[test]
        fn icon_declarations_accept_attribute_order_and_whitespace(
            name in "[a-z]{1,20}", whitespace in "[ \\t]{0,5}", href_first in proptest::bool::ANY,
        ) {
            let href = format!("href{whitespace}={whitespace}\"/{name}.png?revision=1\"");
            let rel = format!("rel{whitespace}={whitespace}\"icon\"");
            let source = if href_first { format!("<link {href} {rel}>") } else { format!("<link {rel} {href}>") };
            proptest::prop_assert_eq!(icon_href(&source), Some(format!("/{name}.png")));
        }
    }
}
