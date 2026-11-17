//! Discover local project branding, following T3 Code's common icon locations.
use agent_protocol::models::ProjectIcon;
use base64::Engine;
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
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

mod appearance;
mod native;
mod web;

struct IconSource {
    light: Vec<PathBuf>,
    dark: Vec<PathBuf>,
}

impl IconSource {
    fn image(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            light: vec![path.clone()],
            dark: vec![path],
        }
    }
}

pub(crate) fn scratch() -> Option<ProjectIcon> {
    let render = |dark| {
        let color = if dark { "#a1a1aa" } else { "#71717a" };
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="{color}" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path stroke-dasharray="3 3" d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2Z"/></svg>"##
        );
        Some(base64::engine::general_purpose::STANDARD.encode(thumbnail(
            svg.as_bytes(),
            true,
            dark,
        )?))
    };
    Some(ProjectIcon {
        light_png: render(false)?,
        dark_png: render(true)?,
    })
}

/// Resolve both appearances off the async executor. Clients can change their
/// appearance without a refresh, and a missing icon never blocks the list.
pub(crate) fn resolve(root: &Path) -> Option<ProjectIcon> {
    if !root.is_absolute() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let declared = SOURCES.iter().filter_map(|source| {
        let bytes = read_within(&root, Path::new(source))?;
        web::source(std::str::from_utf8(&bytes).ok()?)
    });
    declared
        .chain(CANDIDATES.iter().map(|path| IconSource::image(*path)))
        .find_map(|source| resolve_source(&root, source))
        .or_else(|| {
            native::sources(&root)
                .into_iter()
                .find_map(|source| resolve_source(&root, source))
        })
}

fn resolve_source(root: &Path, source: IconSource) -> Option<ProjectIcon> {
    let light = render_paths(root, &source.light, false);
    let dark = render_paths(root, &source.dark, true);
    let (light_png, dark_png) = match (light, dark) {
        (Some(light), Some(dark)) => (light, dark),
        (Some(png), None) | (None, Some(png)) => (png.clone(), png),
        (None, None) => return None,
    };
    Some(ProjectIcon {
        light_png,
        dark_png,
    })
}

fn render_paths(root: &Path, paths: &[PathBuf], dark: bool) -> Option<String> {
    paths.iter().find_map(|path| {
        let bytes = read_within(root, path)?;
        let png = thumbnail(
            &bytes,
            path.extension().is_some_and(|ext| ext == "svg"),
            dark,
        )?;
        Some(base64::engine::general_purpose::STANDARD.encode(png))
    })
}

fn read_within(root: &Path, relative: &Path) -> Option<Vec<u8>> {
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

fn thumbnail(bytes: &[u8], svg: bool, dark: bool) -> Option<Vec<u8>> {
    if svg {
        let themed = std::str::from_utf8(bytes)
            .ok()
            .and_then(|svg| appearance::svg(svg, dark));
        let bytes = themed.as_ref().map_or(bytes, |svg| svg.as_bytes());
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

    fn decoded(root: &Path, dark: bool) -> image::RgbaImage {
        let icon = resolve(root).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(if dark { icon.dark_png } else { icon.light_png })
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
        let icon = decoded(root, false);
        assert_eq!(icon.dimensions(), (64, 64));
        assert_eq!(icon.get_pixel(32, 32).0, [255, 0, 0, 255]);
        assert_eq!(icon.get_pixel(32, 0).0[3], 0);
        std::fs::write(root.join("favicon.svg"), "invalid SVG").unwrap();
        assert_eq!(decoded(root, false).get_pixel(32, 32).0, [0, 0, 255, 255]);
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
            assert_eq!(decoded(root, false).get_pixel(16, 16).0, [0, 255, 0, 255]);
        }
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
        assert_eq!(decoded(&root, false).get_pixel(32, 32).0, [255, 0, 0, 255]);
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
        assert!(decoded(&root, false).pixels().all(|pixel| pixel.0[3] == 0));
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

    #[cfg(unix)]
    #[test]
    fn skips_non_files_without_blocking_task_list_refresh() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(directory.path().join("favicon.svg"))
                .status()
                .unwrap()
                .success()
        );
        let root = directory.path().to_path_buf();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || sender.send(resolve(&root)).unwrap());
        let icon = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("icon discovery blocked on a non-file");
        reader.join().unwrap();
        assert!(icon.is_none());
    }
    fn color_file(root: &Path, path: &str, color: [u8; 4]) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(64, 64, image::Rgba(color))
            .save(path)
            .unwrap();
    }

    #[test]
    fn native_app_icons_keep_their_logo_and_switch_declared_appearances() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let catalog = "apps/mobile/iosApp/Native/Assets.xcassets/AppIcon.appiconset";
        color_file(root, &format!("{catalog}/light.png"), [255, 0, 0, 255]);
        color_file(root, &format!("{catalog}/dark.png"), [0, 255, 0, 255]);
        color_file(root, &format!("{catalog}/tinted.png"), [0, 0, 255, 255]);
        color_file(root, "apps/nsh/assets/icon.png", [255, 0, 255, 255]);
        std::fs::write(root.join(catalog).join("Contents.json"), serde_json::json!({"images":[
            {"filename":"tinted.png","size":"1024x1024","appearances":[{"appearance":"luminosity","value":"tinted"}]},
            {"filename":"dark.png","size":"1024x1024","appearances":[{"appearance":"luminosity","value":"dark"}]},
            {"filename":"light.png","size":"1024x1024"}
        ]}).to_string()).unwrap();
        assert_eq!(decoded(root, false).get_pixel(32, 32).0, [255, 0, 0, 255]);
        assert_eq!(decoded(root, true).get_pixel(32, 32).0, [0, 255, 0, 255]);
        std::fs::remove_file(root.join(catalog).join("dark.png")).unwrap();
        assert_eq!(decoded(root, true).get_pixel(32, 32).0, [255, 0, 0, 255]);
        std::fs::write(root.join(catalog).join("Contents.json"), "invalid").unwrap();
        assert_eq!(decoded(root, false).get_pixel(32, 32).0, [255, 0, 255, 255]);
    }

    #[test]
    fn the_real_bex_app_icon_is_found_in_its_native_project_layout() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory
            .path()
            .join("apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset");
        std::fs::create_dir_all(&catalog).unwrap();
        let png = include_bytes!(
            "../../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
        );
        std::fs::write(catalog.join("AppIcon.png"), png).unwrap();
        std::fs::write(catalog.join("Contents.json"), include_bytes!("../../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/Contents.json")).unwrap();
        let expected = image::load_from_memory(png)
            .unwrap()
            .thumbnail(64, 64)
            .into_rgba8();
        assert_eq!(decoded(directory.path(), false), expected);
        assert_eq!(decoded(directory.path(), true), expected);
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert_eq!(decoded(repository, false), expected);
    }

    #[test]
    fn web_media_links_override_neutral_fallbacks_and_render_both_variants() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        color_file(root, "public/light.png", [255, 0, 0, 255]);
        color_file(root, "public/dark.png", [0, 255, 0, 255]);
        std::fs::write(root.join("favicon.svg"), SVG).unwrap();
        std::fs::write(root.join("index.html"), r#"<link rel="icon" href="/favicon.svg"><link media="(prefers-color-scheme: dark)" rel="icon" href="/dark.png"><link media="(prefers-color-scheme: light)" rel="icon" href="/light.png">"#).unwrap();
        assert_eq!(decoded(root, false).get_pixel(32, 32).0, [255, 0, 0, 255]);
        assert_eq!(decoded(root, true).get_pixel(32, 32).0, [0, 255, 0, 255]);
    }

    #[test]
    fn svg_media_rules_and_current_color_follow_the_client_appearance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("favicon.svg");
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><style>path { fill: black; } @media (prefers-color-scheme: dark) { path { fill: white; } }</style><path d="M0 0h64v64H0z"/></svg>"#).unwrap();
        assert_eq!(
            decoded(directory.path(), false).get_pixel(32, 32).0,
            [0, 0, 0, 255]
        );
        assert_eq!(
            decoded(directory.path(), true).get_pixel(32, 32).0,
            [255, 255, 255, 255]
        );
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><path fill="currentColor" d="M0 0h64v64H0z"/></svg>"#).unwrap();
        assert_eq!(
            decoded(directory.path(), false).get_pixel(32, 32).0,
            [0, 0, 0, 255]
        );
        assert_eq!(
            decoded(directory.path(), true).get_pixel(32, 32).0,
            [255, 255, 255, 255]
        );
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" color="red" width="64" height="64"><path fill="currentColor" d="M0 0h64v64H0z"/></svg>"#).unwrap();
        assert_eq!(
            decoded(directory.path(), true).get_pixel(32, 32).0,
            [255, 0, 0, 255]
        );
        std::fs::write(&path, SVG).unwrap();
        assert_eq!(
            decoded(directory.path(), true).get_pixel(32, 32).0,
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn native_catalogs_cannot_read_images_outside_the_project() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        let catalog = root.join("AppIcon.appiconset");
        std::fs::create_dir_all(&catalog).unwrap();
        color_file(directory.path(), "private.png", [255, 0, 0, 255]);
        std::fs::write(
            catalog.join("Contents.json"),
            r#"{"images":[{"filename":"../../private.png"}]}"#,
        )
        .unwrap();
        assert!(resolve(&root).is_none());
    }
    #[test]
    fn a_single_declared_appearance_remains_visible_in_both_themes() {
        for media in ["light", "dark"] {
            let directory = tempfile::tempdir().unwrap();
            color_file(directory.path(), "brand.png", [0, 255, 0, 255]);
            std::fs::write(
                directory.path().join("index.html"),
                format!(
                    r#"<link rel="icon" href="/brand.png" media="(prefers-color-scheme: {media})">"#
                ),
            )
            .unwrap();
            assert_eq!(
                decoded(directory.path(), false).get_pixel(32, 32).0,
                [0, 255, 0, 255]
            );
            assert_eq!(
                decoded(directory.path(), true).get_pixel(32, 32).0,
                [0, 255, 0, 255]
            );
        }
    }
}
