//! Palette color math: `oklch(...)` and `#rrggbb[aa]` values, mixing in
//! Oklab or sRGB the way CSS `color-mix` does, and WCAG contrast.

/// A color with straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Linear-light sRGB channels.
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> f64 {
    let converted = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    };
    converted.clamp(0., 1.)
}

fn channel_byte(value: f64) -> u8 {
    (value * 255.).round() as u8
}

/// Oklab coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Oklab {
    l: f64,
    a: f64,
    b: f64,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        red: 0.,
        green: 0.,
        blue: 0.,
        alpha: 0.,
    };

    /// `#rrggbb`, `#rrggbbaa` or `oklch(L C H)` (L as a fraction).
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Some(hex) = value.strip_prefix('#') {
            if !matches!(hex.len(), 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
            let channel = |at: usize| byte(at).map(|byte| srgb_to_linear(f64::from(byte) / 255.));
            return Some(Self {
                red: channel(0)?,
                green: channel(2)?,
                blue: channel(4)?,
                alpha: if hex.len() == 8 {
                    f64::from(byte(6)?) / 255.
                } else {
                    1.
                },
            });
        }
        let inner = value.strip_prefix("oklch(")?.strip_suffix(')')?;
        let mut parts = inner.split_whitespace().map(str::parse::<f64>);
        let (lightness, chroma, hue) = (
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        );
        if parts.next().is_some() {
            return None;
        }
        let hue = hue.to_radians();
        Some(Self::from_oklab(
            Oklab {
                l: lightness,
                a: chroma * hue.cos(),
                b: chroma * hue.sin(),
            },
            1.,
        ))
    }

    fn from_oklab(color: Oklab, alpha: f64) -> Self {
        let l = (color.l + 0.396_337_777_4 * color.a + 0.215_803_757_3 * color.b).powi(3);
        let m = (color.l - 0.105_561_345_8 * color.a - 0.063_854_172_8 * color.b).powi(3);
        let s = (color.l - 0.089_484_177_5 * color.a - 1.291_485_548 * color.b).powi(3);
        Self {
            red: 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
            green: -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
            blue: -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
            alpha,
        }
    }

    fn oklab(self) -> Oklab {
        let l = (0.412_221_470_8 * self.red
            + 0.536_332_536_3 * self.green
            + 0.051_445_992_9 * self.blue)
            .cbrt();
        let m = (0.211_903_498_2 * self.red
            + 0.680_699_545_1 * self.green
            + 0.107_396_956_6 * self.blue)
            .cbrt();
        let s = (0.088_302_461_9 * self.red
            + 0.281_718_837_6 * self.green
            + 0.629_978_700_5 * self.blue)
            .cbrt();
        Oklab {
            l: 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
            a: 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
            b: 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
        }
    }

    /// `#rrggbb`, or `#rrggbbaa` while translucent; out-of-gamut channels clip.
    pub fn hex(self) -> String {
        let [red, green, blue] =
            [self.red, self.green, self.blue].map(|c| channel_byte(linear_to_srgb(c)));
        let alpha = channel_byte(self.alpha.clamp(0., 1.));
        if alpha == 255 {
            format!("#{red:02x}{green:02x}{blue:02x}")
        } else {
            format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}")
        }
    }

    /// `color-mix(in oklab, self weight, other)`: `weight` is this color's
    /// share, 0 to 1, with premultiplied alpha.
    pub fn mix_oklab(self, other: Self, weight: f64) -> Self {
        let (from, to) = (self.oklab(), other.oklab());
        mix(
            [from.l, from.a, from.b, self.alpha],
            [to.l, to.a, to.b, other.alpha],
            weight,
            |[l, a, b], alpha| Self::from_oklab(Oklab { l, a, b }, alpha),
        )
    }

    /// `color-mix(in srgb, self weight, other)`, with premultiplied alpha.
    pub fn mix_srgb(self, other: Self, weight: f64) -> Self {
        let encode = |color: Self| {
            [
                linear_to_srgb(color.red),
                linear_to_srgb(color.green),
                linear_to_srgb(color.blue),
                color.alpha,
            ]
        };
        mix(
            encode(self),
            encode(other),
            weight,
            |[red, green, blue], alpha| Self {
                red: srgb_to_linear(red),
                green: srgb_to_linear(green),
                blue: srgb_to_linear(blue),
                alpha,
            },
        )
    }

    /// WCAG relative luminance of the opaque color.
    pub fn luminance(self) -> f64 {
        let [red, green, blue] = [self.red, self.green, self.blue]
            .map(|c| srgb_to_linear(f64::from(channel_byte(linear_to_srgb(c))) / 255.));
        0.2126 * red + 0.7152 * green + 0.0722 * blue
    }
}

/// Interpolates premultiplied channels, as CSS `color-mix` does.
fn mix(from: [f64; 4], to: [f64; 4], weight: f64, build: impl Fn([f64; 3], f64) -> Color) -> Color {
    let weight = weight.clamp(0., 1.);
    let alpha = from[3] * weight + to[3] * (1. - weight);
    if alpha <= 0. {
        return Color::TRANSPARENT;
    }
    let channel =
        |index: usize| (from[index] * from[3] * weight + to[index] * to[3] * (1. - weight)) / alpha;
    build([channel(0), channel(1), channel(2)], alpha)
}

/// The WCAG contrast ratio of two opaque colors.
pub fn contrast_ratio(first: Color, second: Color) -> f64 {
    let (first, second) = (first.luminance(), second.luminance());
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

/// `value` as `#rrggbb[aa]`, or itself when it is not a color.
pub fn to_hex(value: &str) -> String {
    Color::parse(value).map_or_else(|| value.to_owned(), Color::hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_keeps_alpha() {
        for value in ["#000000", "#ffffff", "#346bf1", "#fe9a0051"] {
            assert_eq!(to_hex(value), value);
        }
        assert_eq!(Color::parse("#12345"), None);
        assert_eq!(Color::parse("oklch(0.5 0.1)"), None);
    }

    #[test]
    fn oklch_values_convert_to_srgb() {
        assert_eq!(to_hex("oklch(1 0 0)"), "#ffffff");
        assert_eq!(to_hex("oklch(0 0 0)"), "#000000");
        assert_eq!(to_hex("oklch(0.627955 0.257683 29.234)"), "#ff0000");
    }

    #[test]
    fn mixing_weights_the_first_color_and_premultiplies_alpha() {
        let black = Color::parse("#000000").unwrap();
        let white = Color::parse("#ffffff").unwrap();
        assert_eq!(black.mix_oklab(white, 1.).hex(), "#000000");
        assert_eq!(black.mix_oklab(white, 0.).hex(), "#ffffff");
        // A color mixed with transparent keeps its hue and loses opacity.
        let faded = Color::parse("#346bf1")
            .unwrap()
            .mix_srgb(Color::TRANSPARENT, 0.7);
        assert_eq!(faded.hex(), "#346bf1b3");
    }

    #[test]
    fn contrast_ratio_spans_one_to_twenty_one() {
        let black = Color::parse("#000000").unwrap();
        let white = Color::parse("#ffffff").unwrap();
        assert!((contrast_ratio(black, white) - 21.).abs() < 1e-9);
        assert!((contrast_ratio(white, white) - 1.).abs() < 1e-9);
    }
}
