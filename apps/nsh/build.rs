fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=ios-simulator.entitlements");
    // `desktop`: the window shell the Mac and Windows apps share.
    println!("cargo::rustc-check-cfg=cfg(desktop)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos" | "windows")
    ) {
        println!("cargo::rustc-cfg=desktop");
    }
    // `dictation`: the platforms whose microphone the app records.
    println!("cargo::rustc-check-cfg=cfg(dictation)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos" | "ios" | "windows")
    ) {
        println!("cargo::rustc-cfg=dictation");
    }
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("-apple-ios-sim")) {
        // Xcode embeds Simulator entitlements in Mach-O. Keeping them out of
        // the ad-hoc macOS signature also keeps the Simulator binary launchable.
        println!(
            "cargo:rustc-link-arg-bin=agent-nsh=-Wl,-sectcreate,__TEXT,__entitlements,{}",
            manifest.join("ios-simulator.entitlements").display()
        );
    }
    file_icons(&manifest);
}

/// The desktop client's file glyphs (T3 Code's Pierre tree icons) in the app's
/// dark look, by the names Core's `desktop_file_icon` gives.
fn file_icons(manifest: &std::path::Path) {
    let directory = manifest.join("assets/file-icons");
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut icons = std::fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name()?.to_str()?.strip_suffix("-dark.svg")?;
            Some((name.to_owned(), path.canonicalize().unwrap()))
        })
        .collect::<Vec<_>>();
    icons.sort();
    use base64::Engine;
    let arms = icons
        .iter()
        .map(|(name, path)| {
            let data = base64::engine::general_purpose::STANDARD.encode(render_icon(path));
            format!("        {name:?} => {data:?},\n")
        })
        .collect::<String>();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        out.join("file_icons.rs"),
        format!(
            "fn file_icon_data(name: &str) -> Option<&'static str> {{\n    Some(match name {{\n{arms}        _ => return None,\n    }})\n}}\n"
        ),
    )
    .unwrap();
}

/// Rasterize the shared SVG artwork once at build time. All OS text/image
/// adapters receive the same PNG, with enough pixels for Retina text scaling.
fn render_icon(source: &std::path::Path) -> Vec<u8> {
    use resvg::{tiny_skia, usvg};
    let tree =
        usvg::Tree::from_data(&std::fs::read(source).unwrap(), &usvg::Options::default()).unwrap();
    let scale = 128.0 / tree.size().width().max(tree.size().height());
    let size = tree
        .size()
        .scale_to(tiny_skia::Size::from_wh(128.0, 128.0).unwrap())
        .to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().unwrap()
}
