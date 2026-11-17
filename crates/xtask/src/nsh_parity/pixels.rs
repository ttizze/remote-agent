//! What a screenshot shows inside an element's frame, measured the same way for
//! both clients: the fill around its edge, and the "ink" that differs from it
//! (text and glyphs) with its bounds, colour and first line's height. The DOM
//! and AppKit report fonts differently, so drawn pixels are what is compared.
use std::io::Cursor;

pub(super) type Rgb = [u8; 3];

/// A rectangle in points: x, y (from the top left), width, height.
pub(super) type Rect = [f64; 4];

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Rgb>,
    /// Pixels per point.
    pub scale: f64,
}

/// An RGB distance under which a pixel is the element's fill (antialiasing and
/// gradients stay below it; text and glyphs go above).
const INK: f64 = 48.0;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Ink {
    /// The fill: the most common colour on the frame's edge.
    pub background: Rgb,
    /// The bounds of the pixels that are not the fill, in points.
    pub bounds: Option<Rect>,
    /// The colour of the strongest ink.
    pub color: Option<Rgb>,
    /// The height of the first band of rows with ink: a line of text's drawn
    /// height (ascender to descender), which follows its font size.
    pub first_line: Option<f64>,
}

impl Image {
    pub(super) fn decode(bytes: &[u8], points_wide: f64) -> Result<Self, String> {
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
        let mut buffer = vec![
            0;
            reader
                .output_buffer_size()
                .ok_or("the image is too large")?
        ];
        let info = reader
            .next_frame(&mut buffer)
            .map_err(|error| error.to_string())?;
        let channels = info.color_type.samples();
        let pixels = buffer[..info.buffer_size()]
            .chunks_exact(channels)
            .map(|pixel| match channels {
                1 | 2 => [pixel[0]; 3],
                _ => [pixel[0], pixel[1], pixel[2]],
            })
            .collect();
        Ok(Self {
            width: info.width as usize,
            height: info.height as usize,
            pixels,
            scale: f64::from(info.width) / points_wide,
        })
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, self.width as u32, self.height as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("an in-memory PNG header");
            writer
                .write_image_data(self.pixels.as_flattened())
                .expect("in-memory PNG data");
        }
        bytes
    }

    fn at(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * self.width + x]
    }

    /// `rect` in pixels, clipped to the image: left, top, right, bottom (exclusive).
    fn span(&self, rect: Rect) -> Option<[usize; 4]> {
        let clip = |value: f64, limit: usize| {
            (value * self.scale).round().clamp(0.0, limit as f64) as usize
        };
        let span = [
            clip(rect[0], self.width),
            clip(rect[1], self.height),
            clip(rect[0] + rect[2], self.width),
            clip(rect[1] + rect[3], self.height),
        ];
        (span[2] > span[0] + 1 && span[3] > span[1] + 1).then_some(span)
    }

    /// What is drawn in `rect`, whose corners are rounded by `radius`: its fill
    /// is the most common colour a point inside the edge (clear of a border and
    /// the rounded corners), and its ink is what differs from that fill inside
    /// the corners' reach (a round button's glyph, not the page around it).
    pub(super) fn ink(&self, rect: Rect, radius: f64) -> Option<Ink> {
        let radius = radius.clamp(0.0, rect[2].min(rect[3]) / 2.0);
        let corner = radius * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        let inset = corner.max(1.0).min(rect[2].min(rect[3]) / 4.0);
        let ring = self.span([
            rect[0] + inset,
            rect[1] + inset,
            rect[2] - 2.0 * inset,
            rect[3] - 2.0 * inset,
        ])?;
        let [left, top, right, bottom] = self.span([
            rect[0] + corner,
            rect[1] + corner,
            rect[2] - 2.0 * corner,
            rect[3] - 2.0 * corner,
        ])?;
        let background = most_common(middle_of_edges(ring).map(|(x, y)| self.at(x, y)));
        let mut bounds: Option<[usize; 4]> = None;
        let mut rows = vec![false; bottom - top];
        let mut ink = Vec::new();
        for y in top..bottom {
            for x in left..right {
                let pixel = self.at(x, y);
                let distance = distance(pixel, background);
                if distance < INK {
                    continue;
                }
                rows[y - top] = true;
                bounds = Some(bounds.map_or([x, y, x, y], |[l, t, r, b]| {
                    [l.min(x), t.min(y), r.max(x), b.max(y)]
                }));
                ink.push((distance, pixel));
            }
        }
        let color = strongest_color(&mut ink);
        let points = |pixels: usize| pixels as f64 / self.scale;
        Some(Ink {
            background,
            bounds: bounds
                .map(|[l, t, r, b]| [points(l), points(t), points(r + 1 - l), points(b + 1 - t)]),
            color,
            first_line: first_band(&rows, (self.scale * 2.0).round() as usize).map(points),
        })
    }

    /// Outlines `rect` in `color`, two points wide.
    pub(super) fn outline(&mut self, rect: Rect, color: Rgb) {
        let Some([left, top, right, bottom]) = self.span(rect) else {
            return;
        };
        let width = (self.scale * 2.0).round().max(1.0) as usize;
        for y in top..bottom {
            for x in left..right {
                let near_edge = x < left + width
                    || x + width >= right
                    || y < top + width
                    || y + width >= bottom;
                if near_edge {
                    self.pixels[y * self.width + x] = color;
                }
            }
        }
    }
}

/// `left` and `right` beside each other on a dark gap, `right` scaled to
/// `left`'s pixels per point.
pub(super) fn side_by_side(left: &Image, right: &Image) -> Image {
    let right = resample(right, left.scale);
    let gap = (left.scale * 16.0).round() as usize;
    let width = left.width + gap + right.width;
    let height = left.height.max(right.height);
    let mut pixels = vec![[40, 40, 40]; width * height];
    for (image, offset) in [(left, 0), (&right, left.width + gap)] {
        for y in 0..image.height {
            let row = &image.pixels[y * image.width..(y + 1) * image.width];
            pixels[y * width + offset..y * width + offset + image.width].copy_from_slice(row);
        }
    }
    Image {
        width,
        height,
        pixels,
        scale: left.scale,
    }
}

fn resample(image: &Image, scale: f64) -> Image {
    if (image.scale - scale).abs() < f64::EPSILON {
        return image.clone();
    }
    let factor = image.scale / scale;
    let width = (image.width as f64 / factor).round() as usize;
    let height = (image.height as f64 / factor).round() as usize;
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let source =
                |value: usize, limit: usize| ((value as f64 * factor) as usize).min(limit - 1);
            image.at(source(x, image.width), source(y, image.height))
        })
        .collect();
    Image {
        width,
        height,
        pixels,
        scale,
    }
}

/// The pixels along the middle half of each side of `span`.
fn middle_of_edges([left, top, right, bottom]: [usize; 4]) -> impl Iterator<Item = (usize, usize)> {
    let (width, height) = (right - left, bottom - top);
    let across =
        (left + width / 4..right - width / 4).flat_map(move |x| [(x, top), (x, bottom - 1)]);
    let down =
        (top + height / 4..bottom - height / 4).flat_map(move |y| [(left, y), (right - 1, y)]);
    across.chain(down)
}

pub(super) fn distance(a: Rgb, b: Rgb) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (f64::from(*a) - f64::from(b)).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn most_common(pixels: impl Iterator<Item = Rgb>) -> Rgb {
    let mut counts = std::collections::HashMap::<Rgb, usize>::new();
    for pixel in pixels {
        *counts.entry(pixel).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(pixel, count)| (count, pixel))
        .map_or([0; 3], |(pixel, _)| pixel)
}

/// The mean of the quarter of ink pixels farthest from the fill: glyph cores,
/// not their antialiased edges.
fn strongest_color(pixels: &mut [(f64, Rgb)]) -> Option<Rgb> {
    if pixels.is_empty() {
        return None;
    }
    pixels.sort_by(|a, b| b.0.total_cmp(&a.0));
    let core = &pixels[..pixels.len().div_ceil(4)];
    let mean = |channel: usize| {
        (core
            .iter()
            .map(|(_, pixel)| f64::from(pixel[channel]))
            .sum::<f64>()
            / core.len() as f64)
            .round() as u8
    };
    Some([mean(0), mean(1), mean(2)])
}

/// The length of the first run of `true` rows, bridging gaps up to `gap` rows
/// (the dot of an "i" above its stem).
fn first_band(rows: &[bool], gap: usize) -> Option<usize> {
    let start = rows.iter().position(|&ink| ink)?;
    let mut end = start;
    let mut blank = 0;
    for (index, &ink) in rows.iter().enumerate().skip(start) {
        if ink {
            end = index;
            blank = 0;
        } else {
            blank += 1;
            if blank > gap {
                break;
            }
        }
    }
    Some(end + 1 - start)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Rgb = [10, 10, 10];
    const LIGHT: Rgb = [240, 240, 240];

    /// A 40×20 dark image at two pixels per point with `paint`'s pixels light.
    fn image(paint: impl Fn(usize, usize) -> bool) -> Image {
        let (width, height) = (80, 40);
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| if paint(x, y) { LIGHT } else { DARK })
            .collect();
        Image {
            width,
            height,
            pixels,
            scale: 2.0,
        }
    }

    #[test]
    fn ink_is_measured_in_points_against_the_edge_fill() {
        // Two "lines": rows 10–17 and 26–29, columns 20–59.
        let image =
            image(|x, y| (20..60).contains(&x) && ((10..18).contains(&y) || (26..30).contains(&y)));
        let ink = image.ink([0.0, 0.0, 40.0, 20.0], 0.0).unwrap();
        assert_eq!(ink.background, DARK);
        assert_eq!(ink.bounds, Some([10.0, 5.0, 20.0, 10.0]));
        assert_eq!(ink.color, Some(LIGHT));
        assert_eq!(ink.first_line, Some(4.0));
    }

    #[test]
    fn a_frame_without_ink_has_only_its_fill() {
        let ink = image(|_, _| false)
            .ink([5.0, 5.0, 10.0, 10.0], 0.0)
            .unwrap();
        assert_eq!((ink.bounds, ink.color, ink.first_line), (None, None, None));
        assert_eq!(image(|_, _| false).ink([50.0, 5.0, 10.0, 10.0], 0.0), None);
    }

    #[test]
    fn a_round_button_measures_its_fill_and_glyph_not_the_page_around_it() {
        const FILL: Rgb = [60, 90, 160];
        // A circle of radius 10 points centred at (20, 10), a 4-point glyph in its middle.
        let pixels = (0..40)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y): (usize, usize)| {
                let (dx, dy) = (x as f64 / 2.0 - 20.0, y as f64 / 2.0 - 10.0);
                if (-2.0..2.0).contains(&dx) && (-2.0..2.0).contains(&dy) {
                    LIGHT
                } else if dx * dx + dy * dy < 100.0 {
                    FILL
                } else {
                    DARK
                }
            })
            .collect();
        let image = Image {
            width: 80,
            height: 40,
            pixels,
            scale: 2.0,
        };
        let ink = image.ink([10.0, 0.0, 20.0, 20.0], 10.0).unwrap();
        assert_eq!(ink.background, FILL);
        assert_eq!(ink.bounds, Some([18.0, 8.0, 4.0, 4.0]));
        assert_eq!(ink.color, Some(LIGHT));
    }

    #[test]
    fn a_short_gap_stays_within_the_first_line() {
        assert_eq!(
            first_band(
                &[
                    false, true, false, false, true, true, false, false, false, true
                ],
                2
            ),
            Some(5)
        );
        assert_eq!(first_band(&[false, false], 2), None);
    }

    #[test]
    fn images_round_trip_and_sit_side_by_side_at_one_scale() {
        let left = image(|x, _| x < 4);
        let decoded = Image::decode(&left.encode(), 40.0).unwrap();
        assert_eq!(decoded, left);
        let mut right = resample(&left, 1.0);
        assert_eq!((right.width, right.height, right.scale), (40, 20, 1.0));
        right.outline([0.0, 0.0, 40.0, 20.0], [255, 0, 0]);
        let both = side_by_side(&left, &right);
        assert_eq!((both.width, both.height), (80 + 32 + 80, 40));
        assert_eq!(both.at(0, 0), LIGHT);
        assert_eq!(both.at(100, 0), [40, 40, 40]);
        assert_eq!(both.at(112, 20), [255, 0, 0]);
        assert_eq!(both.at(150, 20), DARK);
    }
}
