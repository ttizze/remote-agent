use gpui_kit::{Image, ImageFormat};
use std::{io::Write, path::Path};

pub(super) fn save_image(directory: &Path, image: Image) -> Result<String, String> {
    host_daemon::platform::create_state_directory(directory).map_err(|e| e.to_string())?;
    let passthrough = matches!(
        image.format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp
    );
    let extension = if passthrough {
        image.format.extension()
    } else {
        "png"
    };
    let mut file = tempfile::Builder::new()
        .prefix("clipboard-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(directory)
        .map_err(|e| e.to_string())?;
    if passthrough {
        file.write_all(&image.bytes).map_err(|e| e.to_string())?;
    } else {
        // macOS image copies can contain TIFF; Codex accepts the PNG conversion.
        image::load_from_memory(&image.bytes)
            .and_then(|image| image.write_to(file.as_file_mut(), image::ImageFormat::Png))
            .map_err(|e| e.to_string())?;
    }
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    // Drafts and localImage history reference this path after the window/app closes.
    let (_, path) = file.keep().map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_images_survive_reopening_and_do_not_overwrite_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = include_bytes!("../../assets/icon.png");
        let first = save_image(
            directory.path(),
            Image::from_bytes(ImageFormat::Png, bytes.to_vec()),
        )
        .unwrap();
        let second = save_image(
            directory.path(),
            Image::from_bytes(ImageFormat::Png, bytes.to_vec()),
        )
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
    }

    #[test]
    fn tiff_clipboard_becomes_a_readable_png() {
        let directory = tempfile::tempdir().unwrap();
        let pixels = image::DynamicImage::new_rgb8(2, 3);
        let mut bytes = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Tiff)
            .unwrap();
        let path = save_image(
            directory.path(),
            Image::from_bytes(ImageFormat::Tiff, bytes.into_inner()),
        )
        .unwrap();
        assert!(path.ends_with(".png"));
        let decoded = image::open(path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 3));
    }

    #[test]
    fn failed_conversion_leaves_no_attachment() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            save_image(
                directory.path(),
                Image::from_bytes(ImageFormat::Tiff, vec![0])
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
