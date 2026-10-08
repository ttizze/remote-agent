//! How large a conversation image is drawn.

pub const MARKDOWN_IMAGE_MAX_WIDTH: f64 = 480.;
pub const MARKDOWN_IMAGE_MAX_HEIGHT: f64 = 480.;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImageDisplaySize {
    pub width: f64,
    pub height: f64,
}

/// Fits an image inside explicit bounds while retaining its aspect ratio.
pub fn fit_image_display_size(
    source_width: f64,
    source_height: f64,
    max_width: f64,
    max_height: f64,
) -> Option<ImageDisplaySize> {
    if !source_width.is_finite()
        || !source_height.is_finite()
        || !max_width.is_finite()
        || !max_height.is_finite()
        || source_width <= 0.
        || source_height <= 0.
        || max_width <= 0.
        || max_height <= 0.
    {
        return None;
    }
    let scale = (max_width / source_width).min(max_height / source_height);
    if !scale.is_finite() || scale <= 0. {
        return None;
    }
    Some(ImageDisplaySize {
        width: (source_width * scale).min(max_width),
        height: (source_height * scale).min(max_height),
    })
}

/// Small images keep their intrinsic size; larger ones fit the chat width and
/// 480 points either way, keeping their aspect ratio. `None` when the sizes
/// cannot produce a stable layout.
pub fn markdown_image_display_size(
    source_width: f64,
    source_height: f64,
    available_width: f64,
) -> Option<ImageDisplaySize> {
    if !available_width.is_finite() || available_width <= 0. {
        return None;
    }
    fit_image_display_size(
        source_width,
        source_height,
        source_width
            .min(available_width)
            .min(MARKDOWN_IMAGE_MAX_WIDTH),
        source_height.min(MARKDOWN_IMAGE_MAX_HEIGHT),
    )
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

    #[test]
    fn bounds_document_rasters_even_for_extreme_page_shapes() {
        for (width, height) in [(1., i32::MAX as f64), (i32::MAX as f64, 1.), (612., 792.)] {
            let fitted = fit_image_display_size(width, height, 1200., 1600.).unwrap();
            assert!(fitted.width > 0. && fitted.width <= 1200.);
            assert!(fitted.height > 0. && fitted.height <= 1600.);
            assert!(
                (fitted.width / fitted.height - width / height).abs() <= (width / height) * 1e-12
            );
        }
    }

    #[test]
    fn document_rasters_can_scale_up_without_exceeding_bounds() {
        assert_eq!(
            fit_image_display_size(300., 400., 1200., 1600.),
            size(1200., 1600.)
        );
        assert_eq!(
            fit_image_display_size(100., 100., f64::INFINITY, 1600.),
            None
        );
        assert_eq!(fit_image_display_size(100., 100., 1200., 0.), None);
    }

    proptest::proptest! {
        #[test]
        fn document_raster_budget_holds_for_positive_page_dimensions(
            width in 1..i32::MAX,
            height in 1..i32::MAX,
        ) {
            let fitted = fit_image_display_size(width as f64, height as f64, 1200., 1600.).unwrap();
            proptest::prop_assert!(fitted.width > 0. && fitted.width <= 1200.);
            proptest::prop_assert!(fitted.height > 0. && fitted.height <= 1600.);
            let aspect = width as f64 / height as f64;
            proptest::prop_assert!((fitted.width / fitted.height - aspect).abs() <= aspect * 1e-12);
        }
    }
}
