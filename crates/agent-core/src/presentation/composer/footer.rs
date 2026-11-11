//! The model control fits between attaching and sending. Its name shortens
//! to T3 Code's `min-w-13`; narrower footers hide it. Approval settings live
//! in the model picker, so the composer has no additional controls to fold.

/// The narrowest model control, with its name shortened.
pub const MODEL_MINIMUM_WIDTH: f32 = 52.0;

/// The control's width, or none when it cannot fit. A hidden control needs
/// one extra point to return, preventing flicker at the visibility boundary.
pub fn model_width(natural: f32, available: f32, previously_visible: bool) -> Option<f32> {
    let threshold = MODEL_MINIMUM_WIDTH + if previously_visible { 0.0 } else { 1.0 };
    (available >= threshold)
        .then(|| available.clamp(MODEL_MINIMUM_WIDTH, natural.max(MODEL_MINIMUM_WIDTH)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_shorten_and_short_names_keep_the_minimum_hit_area() {
        assert_eq!(model_width(180.0, 250.0, true), Some(180.0));
        assert_eq!(model_width(180.0, 100.0, true), Some(100.0));
        assert_eq!(model_width(180.0, 52.0, true), Some(52.0));
        assert_eq!(model_width(20.0, 100.0, true), Some(52.0));
        assert_eq!(model_width(180.0, 51.9, true), None);
        assert_eq!(model_width(180.0, 0.0, true), None);
    }

    #[test]
    fn returning_waits_for_room_without_delaying_hiding() {
        assert_eq!(model_width(180.0, 52.0, false), None);
        assert_eq!(model_width(180.0, 52.9, false), None);
        assert_eq!(model_width(180.0, 53.0, false), Some(53.0));
        assert_eq!(model_width(180.0, 52.0, true), Some(52.0));
        assert_eq!(model_width(180.0, 51.9, true), None);
    }
}
