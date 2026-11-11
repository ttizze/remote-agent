//! T3 Code's interface contrast (`appearanceContrast`, `--contrast-*` in its
//! `index.css`): below 100 a color fades toward its surface, above it toward
//! white, and borders gain a quarter of that toward the foreground.

pub const MIN_CONTRAST: u32 = 50;
pub const MAX_CONTRAST: u32 = 200;
pub const DEFAULT_CONTRAST: u32 = 100;
pub const CONTRAST_STEP: u32 = 5;

type Rgb = [u8; 3];
type Oklab = [f32; 3];

const WHITE: Rgb = [255, 255, 255];

/// A text or icon color at `contrast`, drawn over `surface`.
pub fn contrast_text(color: Rgb, surface: Rgb, contrast: u32) -> Rgb {
    let (base, boost) = amounts(contrast);
    let faded = mix(oklab(color), oklab(surface), base);
    rgb(mix(faded, oklab(WHITE), 1.0 - boost))
}

/// A border at `contrast`, over `surface`, against `foreground`. Mixed in
/// sRGB, as T3 Code's borders are.
pub fn contrast_border(color: Rgb, surface: Rgb, foreground: Rgb, contrast: u32) -> Rgb {
    let (base, boost) = amounts(contrast);
    let faded = mix_srgb(color, surface, base);
    mix_srgb(faded, foreground, 1.0 - boost / 4.0)
}

/// How much of the color stays, and how much the boost adds.
fn amounts(contrast: u32) -> (f32, f32) {
    let contrast = contrast.clamp(MIN_CONTRAST, MAX_CONTRAST);
    (
        contrast.min(100) as f32 / 100.0,
        contrast.saturating_sub(100) as f32 / 100.0,
    )
}

fn mix(first: Oklab, second: Oklab, amount: f32) -> Oklab {
    std::array::from_fn(|index| first[index] * amount + second[index] * (1.0 - amount))
}

fn mix_srgb(first: Rgb, second: Rgb, amount: f32) -> Rgb {
    std::array::from_fn(|index| {
        (f32::from(first[index]) * amount + f32::from(second[index]) * (1.0 - amount)).round() as u8
    })
}

fn linear(channel: u8) -> f32 {
    let channel = f32::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn encoded(channel: f32) -> u8 {
    let channel = channel.clamp(0.0, 1.0);
    let channel = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (channel * 255.0).round() as u8
}

fn oklab(color: Rgb) -> Oklab {
    let [r, g, b] = color.map(linear);
    let [l, m, s] = [
        0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b,
        0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b,
        0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b,
    ]
    .map(f32::cbrt);
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_05 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn rgb(color: Oklab) -> Rgb {
    let [lightness, a, b] = color;
    let [l, m, s] = [
        lightness + 0.396_337_78 * a + 0.215_803_76 * b,
        lightness - 0.105_561_346 * a - 0.063_854_17 * b,
        lightness - 0.089_484_18 * a - 1.291_485_5 * b,
    ]
    .map(|channel| channel * channel * channel);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
    .map(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREY: Rgb = [161, 161, 161];
    const BLACK: Rgb = [0, 0, 0];

    fn close(actual: Rgb, expected: Rgb) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                actual.abs_diff(expected) <= 1,
                "{actual} against {expected}"
            );
        }
    }

    #[test]
    fn the_default_leaves_a_color_as_it_is() {
        close(contrast_text(GREY, BLACK, DEFAULT_CONTRAST), GREY);
        close(
            contrast_border([48, 48, 48], BLACK, WHITE, DEFAULT_CONTRAST),
            [48, 48, 48],
        );
    }

    #[test]
    fn the_extremes_reach_the_surface_half_way_and_white() {
        // Oklab's lightness is what halves: grey 161 at 0.709, so 0.354 is 60.
        close(contrast_text(GREY, BLACK, MIN_CONTRAST), [60, 60, 60]);
        close(contrast_text(GREY, BLACK, MAX_CONTRAST), WHITE);
    }

    #[test]
    fn borders_gain_a_quarter_of_the_boost() {
        close(
            contrast_border([48, 48, 48], BLACK, WHITE, MAX_CONTRAST),
            [100, 100, 100],
        );
    }

    #[test]
    fn contrast_outside_the_range_is_held_to_it() {
        assert_eq!(
            contrast_text(GREY, BLACK, 10),
            contrast_text(GREY, BLACK, MIN_CONTRAST)
        );
        assert_eq!(
            contrast_text(GREY, BLACK, 500),
            contrast_text(GREY, BLACK, MAX_CONTRAST)
        );
    }
}
