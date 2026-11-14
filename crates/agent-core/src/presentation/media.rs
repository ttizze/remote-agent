//! The image viewer, as the desktop client's: its magnification, from a
//! quarter to four times the size that fits the viewer, a quarter at a time,
//! by a pinch or a Ctrl (⌘ on the Mac) wheel in between, its reset titled with
//! the percentage; and its gallery of the conversation's generated images.
use crate::{
    client::SessionImage,
    presentation::markdown::{MarkdownFileSurface, markdown_file_target},
};

pub(crate) fn image_key(source: &str) -> String {
    let path = markdown_file_target(source.into(), String::new(), MarkdownFileSurface::Mobile)
        .map_or_else(|| source.to_owned(), |file| file.path);
    agent_protocol::paths::comparison_key(&path)
}

/// A generated image's encoded data as a source the image views load, as the
/// conversation shows it.
pub fn encoded_image_source(data: &str) -> String {
    format!("data:image/png;base64,{data}")
}

/// The gallery's images, by the sources the conversation shows them with.
pub fn gallery_sources(images: Vec<SessionImage>) -> Vec<String> {
    images
        .into_iter()
        .map(|image| {
            if image.encoded {
                encoded_image_source(&image.source)
            } else {
                image.source
            }
        })
        .collect()
}

/// The viewer's title: where `shown` is among the gallery's `sources`
/// (`2 / 3`), or `画像` for an image the gallery lacks.
pub fn gallery_title(sources: &[String], shown: &str) -> String {
    sources
        .iter()
        .position(|source| source == shown)
        .map_or_else(
            || "画像".into(),
            |index| format!("{} / {}", index + 1, sources.len()),
        )
}

/// How far the image viewer magnifies its image: `1` fits it to the viewer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageZoom(f32);

/// A zoom by hand over the viewer's image, as the platforms report it.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZoomGesture {
    /// A Ctrl (⌘ on the Mac) wheel of this many notches toward the content's
    /// end (down, zooming out); a touchpad reports fractions of a notch.
    Wheel { notches: f32 },
    /// A pinch's change, as AppKit reports it: `0.1` is a tenth larger.
    Pinch { magnification: f32 },
}

impl ImageZoom {
    const MIN: f32 = 0.25;
    const MAX: f32 = 4.0;
    const STEP: f32 = 0.25;
    /// How much a wheel notch magnifies: a step's worth at the fitted size.
    const NOTCH: f32 = 1.25;
    /// How close a scale counts as on a step, so a step from it moves on.
    const ON_STEP: f32 = 1e-3;
    /// The image fitted to the viewer, as it opens and after a reset.
    pub const FIT: Self = Self(1.0);

    /// `scale` within the viewer's range.
    fn new(scale: f32) -> Self {
        if scale.is_nan() {
            Self::FIT
        } else {
            Self(scale.clamp(Self::MIN, Self::MAX))
        }
    }

    pub fn scale(self) -> f32 {
        self.0
    }

    /// The next step larger, from a pinched scale between steps too; `None`
    /// at the largest, whose button is then disabled.
    pub fn zoomed_in(self) -> Option<Self> {
        let steps = (self.0 / Self::STEP + Self::ON_STEP).floor() + 1.0;
        (self.0 < Self::MAX).then(|| Self::new(steps * Self::STEP))
    }

    /// The next step smaller; `None` at the smallest.
    pub fn zoomed_out(self) -> Option<Self> {
        let steps = (self.0 / Self::STEP - Self::ON_STEP).ceil() - 1.0;
        (self.0 > Self::MIN).then(|| Self::new(steps * Self::STEP))
    }

    /// After `gesture`, within the viewer's range; a gesture that reports no
    /// usable change leaves it.
    pub fn gestured(self, gesture: ZoomGesture) -> Self {
        let factor = match gesture {
            ZoomGesture::Wheel { notches } => Self::NOTCH.powf(-notches),
            ZoomGesture::Pinch { magnification } => 1.0 + magnification,
        };
        if factor.is_finite() && factor > 0.0 {
            Self::new(self.0 * factor)
        } else {
            self
        }
    }

    /// The reset's title: the magnification in whole percent.
    pub fn label(self) -> String {
        format!("{:.0}%", self.0 * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn steps_by_a_quarter_between_a_quarter_and_four_times() {
        assert_eq!(ImageZoom::FIT.label(), "100%");
        assert_eq!(
            ImageZoom::FIT.zoomed_in().map(ImageZoom::label),
            Some("125%".into())
        );
        assert_eq!(
            ImageZoom::FIT.zoomed_out().map(ImageZoom::label),
            Some("75%".into())
        );
        assert_eq!(ImageZoom::new(4.0).zoomed_in(), None);
        assert_eq!(ImageZoom::new(0.25).zoomed_out(), None);
        assert_eq!(ImageZoom::new(3.9).zoomed_in(), Some(ImageZoom::new(4.0)));
        assert_eq!(ImageZoom::new(0.3).zoomed_out(), Some(ImageZoom::new(0.25)));
        assert_eq!(ImageZoom::new(9.0).scale(), 4.0);
        assert_eq!(ImageZoom::new(0.0).scale(), 0.25);
        assert_eq!(ImageZoom::new(f32::NAN), ImageZoom::FIT);
        assert_eq!(ImageZoom::new(1.126).label(), "113%");
    }

    #[test]
    fn the_gallery_shows_encoded_images_as_the_conversation_does_and_titles_the_shown_one() {
        let sources = gallery_sources(vec![
            SessionImage {
                source: "/tmp/one.png".into(),
                encoded: false,
            },
            SessionImage {
                source: "aGk=".into(),
                encoded: true,
            },
        ]);
        assert_eq!(sources, ["/tmp/one.png", "data:image/png;base64,aGk="]);
        assert_eq!(
            crate::presentation::body::item_body(
                crate::localization::UiLanguage::Japanese,
                &crate::models::Item::new(
                    "image".into(),
                    crate::models::ItemStatus::Completed,
                    crate::models::ItemBody::ImageGeneration {
                        saved_path: None,
                        data: Some("aGk=".into()),
                        revised_prompt: None,
                    },
                )
            )
            .images,
            [sources[1].clone()],
            "a thumbnail's source is its image's in the conversation",
        );
        assert_eq!(
            gallery_title(&sources, "data:image/png;base64,aGk="),
            "2 / 2"
        );
        assert_eq!(gallery_title(&sources, "/tmp/one.png"), "1 / 2");
        assert_eq!(gallery_title(&sources, "/tmp/attached.png"), "画像");
        assert_eq!(gallery_title(&[], "/tmp/one.png"), "画像");
    }

    #[test]
    fn wheels_and_pinches_scale_the_zoom_within_its_range_and_steps_return_to_quarters() {
        let wheel = |notches| ZoomGesture::Wheel { notches };
        let pinch = |magnification| ZoomGesture::Pinch { magnification };
        // A notch away from the reader is a step at the fitted size.
        let one = ImageZoom::FIT.gestured(wheel(-1.0));
        assert_eq!(one.label(), "125%");
        assert_eq!(one.gestured(wheel(1.0)), ImageZoom::FIT);
        let two = one.gestured(wheel(-1.0));
        assert_eq!(two.label(), "156%");
        // Between steps, the buttons go to the quarters on either side.
        assert_eq!(two.zoomed_in().map(ImageZoom::label), Some("175%".into()));
        assert_eq!(two.zoomed_out().map(ImageZoom::label), Some("150%".into()));
        assert_eq!(ImageZoom::FIT.gestured(pinch(0.5)).label(), "150%");
        assert_eq!(ImageZoom::FIT.gestured(pinch(-0.5)).label(), "50%");
        assert_eq!(
            ImageZoom::FIT.gestured(wheel(-100.0)).scale(),
            ImageZoom::MAX
        );
        assert_eq!(
            ImageZoom::FIT.gestured(wheel(100.0)).scale(),
            ImageZoom::MIN
        );
        assert_eq!(ImageZoom::FIT.gestured(pinch(-1.0)), ImageZoom::FIT);
        assert_eq!(ImageZoom::FIT.gestured(pinch(f32::NAN)), ImageZoom::FIT);
        assert_eq!(
            ImageZoom::FIT.gestured(wheel(f32::INFINITY)),
            ImageZoom::FIT
        );
        assert_eq!(
            serde_json::from_str::<ZoomGesture>(r#"{"wheel":{"notches":-1.5}}"#).unwrap(),
            wheel(-1.5)
        );
        assert_eq!(
            serde_json::from_str::<ZoomGesture>(r#"{"pinch":{"magnification":0.2}}"#).unwrap(),
            pinch(0.2)
        );
    }

    proptest! {
        #[test]
        fn gestures_keep_the_zoom_in_range_and_follow_their_direction(
            scale in 0.25f32..4.0,
            notches in -20.0f32..20.0,
        ) {
            let zoom = ImageZoom::new(scale);
            let after = zoom.gestured(ZoomGesture::Wheel { notches });
            prop_assert!((ImageZoom::MIN..=ImageZoom::MAX).contains(&after.scale()));
            if notches < 0.0 {
                prop_assert!(after.scale() >= zoom.scale());
            } else {
                prop_assert!(after.scale() <= zoom.scale());
            }
        }

        #[test]
        fn steps_land_on_quarters(scale in 0.25f32..4.0) {
            let zoom = ImageZoom::new(scale);
            for stepped in [zoom.zoomed_in(), zoom.zoomed_out()].into_iter().flatten() {
                let quarters = stepped.scale() / 0.25;
                prop_assert!((quarters - quarters.round()).abs() < 1e-4);
            }
        }

        #[test]
        fn stepping_stays_in_range_and_moves_toward_its_end(scale in -10.0f32..10.0) {
            let zoom = ImageZoom::new(scale);
            prop_assert!((ImageZoom::MIN..=ImageZoom::MAX).contains(&zoom.scale()));
            match zoom.zoomed_in() {
                Some(larger) => prop_assert!(larger.scale() > zoom.scale() && larger.scale() <= ImageZoom::MAX),
                None => prop_assert_eq!(zoom.scale(), ImageZoom::MAX),
            }
            match zoom.zoomed_out() {
                Some(smaller) => prop_assert!(smaller.scale() < zoom.scale() && smaller.scale() >= ImageZoom::MIN),
                None => prop_assert_eq!(zoom.scale(), ImageZoom::MIN),
            }
        }
    }
}
