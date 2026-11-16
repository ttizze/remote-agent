//! Pixel effects used over native Windows controls. Native text, selection,
//! accessibility and commands remain owned by those controls.
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
pub(crate) struct Shimmer {
    pub period: f64,
    pub band: f64,
    pub focus: bool,
    pub stops: Vec<(f64, f64)>,
}

impl Shimmer {
    pub fn opacity(&self, x: f64, extent: f64, seconds: f64, scale: f64) -> f64 {
        let band = self.band * scale;
        if band <= 0.0 || self.period <= 0.0 {
            return 0.0;
        }
        let travel = extent + band * if self.focus { 2.0 } else { 1.0 };
        let left = -band + travel * seconds.rem_euclid(self.period) / self.period;
        let position = (x - left) / band;
        self.stops
            .windows(2)
            .find(|pair| pair[0].0 <= position && position <= pair[1].0)
            .map_or(0.0, |pair| {
                let (start, end) = (pair[0], pair[1]);
                (start.1
                    + (end.1 - start.1) * (position - start.0)
                        / (end.0 - start.0).max(f64::EPSILON))
                .clamp(0.0, 1.0)
            })
    }
}

pub(crate) fn tint(pixel: &mut [u8], shine: f64, fade: f64, background: u32) {
    // Leave native selection's chromatic background intact. The band changes
    // muted glyphs, including their antialiasing, rather than the whole row.
    let low = pixel[..3].iter().copied().min().unwrap_or_default();
    let high = pixel[..3].iter().copied().max().unwrap_or_default();
    let glyph = high.saturating_sub(low) < 24 && low > 48;
    for (index, channel) in pixel[..3].iter_mut().enumerate() {
        let base = f64::from(*channel);
        let lit = if glyph {
            base + (255.0 - base) * shine
        } else {
            base
        };
        let back = f64::from((background >> (index * 8)) & 255);
        *channel = (lit + (back - lit) * fade).clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_band_moves_over_glyphs_then_leaves_without_changing_the_selection_background() {
        let shimmer = Shimmer {
            period: 2.2,
            band: 72.0,
            focus: true,
            stops: vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
        };
        assert_eq!(shimmer.opacity(20.0, 200.0, 0.0, 1.0), 0.0);
        let at_center = 2.2 * (20.0 + 36.0) / (200.0 + 144.0);
        assert!((shimmer.opacity(20.0, 200.0, at_center, 1.0) - 1.0).abs() < 0.001);
        assert!((shimmer.opacity(40.0, 400.0, at_center, 2.0) - 1.0).abs() < 0.001);
        assert_eq!(shimmer.opacity(20.0, 200.0, 2.19, 1.0), 0.0);
        let mut selected = [180, 80, 20, 255];
        tint(&mut selected, 1.0, 0.0, 0x141414);
        assert_eq!(selected, [180, 80, 20, 255]);
        let mut text = [170, 170, 170, 255];
        tint(&mut text, 1.0, 0.0, 0x141414);
        assert_eq!(text, [255, 255, 255, 255]);
        tint(&mut text, 0.0, 1.0, 0x141414);
        assert_eq!(text, [20, 20, 20, 255]);
    }
}
