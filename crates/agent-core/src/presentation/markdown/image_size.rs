//! How large a conversation image is drawn.

pub const MARKDOWN_IMAGE_MAX_WIDTH: f64 = 480.;
pub const MARKDOWN_IMAGE_MAX_HEIGHT: f64 = 480.;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImageDisplaySize {
    pub width: f64,
    pub height: f64,
}

/// Small images keep their intrinsic size; larger ones fit the chat width and
/// 480 points either way, keeping their aspect ratio. `None` when the sizes
/// cannot produce a stable layout.
pub fn markdown_image_display_size(
    source_width: f64,
    source_height: f64,
    available_width: f64,
) -> Option<ImageDisplaySize> {
    if !source_width.is_finite()
        || !source_height.is_finite()
        || !available_width.is_finite()
        || source_width <= 0.
        || source_height <= 0.
        || available_width <= 0.
    {
        return None;
    }
    let scale = 1_f64
        .min(available_width / source_width)
        .min(MARKDOWN_IMAGE_MAX_WIDTH / source_width)
        .min(MARKDOWN_IMAGE_MAX_HEIGHT / source_height);
    Some(ImageDisplaySize {
        width: source_width * scale,
        height: source_height * scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f64, height: f64) -> Option<ImageDisplaySize> {
        Some(ImageDisplaySize { width, height })
    }

    #[test]
    fn keeps_small_images_at_their_intrinsic_size() {
        assert_eq!(markdown_image_display_size(96., 96., 332.), size(96., 96.));
    }

    #[test]
    fn fits_wide_images_to_the_available_chat_width() {
        assert_eq!(
            markdown_image_display_size(960., 540., 332.),
            size(332., 186.75)
        );
    }

    #[test]
    fn caps_wide_images_at_480_points_on_larger_screens() {
        assert_eq!(
            markdown_image_display_size(960., 540., 900.),
            size(MARKDOWN_IMAGE_MAX_WIDTH, 270.)
        );
    }

    #[test]
    fn caps_tall_images_by_height_without_changing_their_aspect_ratio() {
        assert_eq!(
            markdown_image_display_size(400., 800., 332.),
            size(240., MARKDOWN_IMAGE_MAX_HEIGHT)
        );
    }

    #[test]
    fn rejects_dimensions_that_cannot_produce_a_stable_layout() {
        assert_eq!(markdown_image_display_size(0., 100., 332.), None);
        assert_eq!(markdown_image_display_size(100., f64::NAN, 332.), None);
    }
}
