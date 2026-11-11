//! T3 Code's live activity shimmer, drawn by NSH: a band of the foreground
//! color sweeps across a muted label every 2.2 s in 30 steps (T3's
//! `live-activity-focus` and `live-tool-shine`). The label is one text of
//! spans, the glyphs before the band, one per slice of it and after it, each
//! slice in its own blend of the two colors; a step moves glyphs between them.
//!
//! A line's icon, as in T3's Thinking row, takes the band's color at its
//! center.
//!
//! As T3's `observeVisibleAnimation`, it moves only while it shows: its row
//! is in the list (the list makes only rows near its viewport) and the app's
//! window is on screen, focused or not. It stops where it is, and never moves
//! under Reduce Motion, where the line reads plainly. A step that leaves
//! every glyph where it was changes nothing.
use crate::conversation::look;
use nsh_core::{platform::Lifecycle, prelude::*};
use std::time::Duration;

/// Which of T3 Code's two sweeps.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Sweep {
    /// `live-activity-focus`: a soft band that leaves the label before it
    /// returns (the Thinking row, setting up a worktree).
    Focus,
    /// `live-tool-shine`: a plain band (a live work row's label).
    Shine,
}

const STEPS: u32 = 30;
const PERIOD: Duration = Duration::from_millis(2200);
/// `4.5rem`.
const BAND: f32 = 72.0;
const SLICES: usize = 8;
/// Where a line's label starts: after T3 Code's work log icon box and gap.
const LEAD: f32 = look::ICON_BOX + look::ICON_GAP;

impl Sweep {
    /// How much of the foreground color the band gives at `x` points into it.
    fn opacity(self, x: f32) -> f32 {
        match self {
            // The stops of `live-activity-focus`'s mask.
            Sweep::Focus => {
                const STOPS: [(f32, f32); 7] = [
                    (0.0, 0.0),
                    (10.8, 0.12),
                    (25.2, 0.55),
                    (36.0, 1.0),
                    (46.8, 0.55),
                    (61.2, 0.12),
                    (BAND, 0.0),
                ];
                STOPS
                    .windows(2)
                    .find(|pair| x >= pair[0].0 && x <= pair[1].0)
                    .map_or(0.0, |pair| {
                        let (start, end) = (pair[0], pair[1]);
                        start.1 + (end.1 - start.1) * (x - start.0) / (end.0 - start.0)
                    })
            }
            Sweep::Shine => (1.0 - (x - BAND / 2.0).abs() / (BAND / 2.0)).max(0.0),
        }
    }

    /// The band's left edge at `step` over a label `width` wide: from just
    /// before the label to past its end (Focus also leaves it).
    fn band_left(self, step: u32, width: f32) -> f32 {
        let travel = match self {
            Sweep::Focus => width + 2.0 * BAND,
            Sweep::Shine => width + BAND,
        };
        -BAND + travel * (step % STEPS) as f32 / STEPS as f32
    }
}

/// `label`'s glyphs split by where `left` puts the band: before it, in each
/// of its slices, after it. Glyphs are placed by their centers (`centers`).
fn parts(label: &str, centers: &[f32], left: f32) -> Vec<String> {
    let mut parts = vec![String::new(); SLICES + 2];
    for (glyph, center) in label.chars().zip(centers) {
        let offset = (center - left) / (BAND / SLICES as f32);
        let part = if offset < 0.0 {
            0
        } else {
            (offset as usize + 1).min(SLICES + 1)
        };
        parts[part].push(glyph);
    }
    parts
}

/// The center of each glyph of `label` in `size`-point system text, and the
/// label's width.
fn centers(label: &str, size: f32) -> (Vec<f32>, f32) {
    let font = format!("{size}px system-ui");
    let mut x = 0.0;
    let centers = label
        .chars()
        .map(|glyph| {
            let width = measure_text(&font, glyph.encode_utf8(&mut [0; 4])).0;
            x += width;
            x - width / 2.0
        })
        .collect();
    (centers, x)
}

/// `from` with `opacity` of `to` over it.
fn blend(from: Rgba, to: Rgba, opacity: f32) -> Rgba {
    let mix = |a: f32, b: f32| a + (b - a) * opacity;
    Rgba {
        r: mix(from.r, to.r),
        g: mix(from.g, to.g),
        b: mix(from.b, to.b),
        a: 1.0,
    }
}

/// `label` on one line of T3 Code's work log after `icon`, swept by `sweep`:
/// the muted label and icon take the foreground under the band.
pub(crate) fn line<I: IntoElement>(
    icon: Option<impl Fn() -> I + 'static>,
    label: impl Fn() -> String + 'static,
    sweep: Sweep,
) -> View {
    let step = signal(0_u32);
    let width = signal(0_f32);
    let label = computed(label);
    let centers = computed(move || label.with(|label| centers(label, look::TEXT)));
    let moving = computed(move || lifecycle() != Lifecycle::Background && !reduce_motion());
    effect(move || {
        if moving.get() {
            // Spawned under this effect: it stops when the effect runs again
            // or the row leaves the list.
            set_interval(PERIOD / STEPS, move || {
                step.update(|step| *step = step.wrapping_add(1))
            });
        }
    });
    // Positions are the line's own: the icon first, the label after it.
    let lead = if icon.is_some() { LEAD } else { 0.0 };
    let left = computed(move || {
        if reduce_motion() {
            // Everything is before the band.
            f32::INFINITY
        } else {
            // As T3's band, across the line as wide as it shows.
            let shown = centers.with(|(_, extent)| width.get().min(*extent));
            sweep.band_left(step.get(), lead + shown)
        }
    });
    let split = computed(move || {
        centers.with(|(centers, _)| label.with(|label| parts(label, centers, left.get() - lead)))
    });
    let slice = BAND / SLICES as f32;
    let spans = (0..SLICES + 2).fold(text(""), |line, part| {
        let color = match part {
            0 => look::muted(),
            part if part == SLICES + 1 => look::muted(),
            part => blend(
                look::muted(),
                look::foreground(),
                sweep.opacity((part as f32 - 0.5) * slice),
            ),
        };
        line.child(
            text(move || SharedString::from(split.with(|split| split[part].clone())))
                .text_color(color),
        )
    });
    let label = view()
        .flex_1()
        .min_w_0()
        .on_layout(move |bounds: &Bounds<Points>| width.set(bounds.size.width.0))
        .child(
            spans
                .number_of_lines(1)
                .text_size(px(look::TEXT))
                .line_height(px(look::LINE)),
        );
    match icon {
        Some(icon) => crate::activity::line(
            view()
                // T3 Code's `live-tool-shine` sweeps the label alone; its
                // focus overlay takes the icon too.
                .text_color(move || match sweep {
                    Sweep::Shine => look::icon(),
                    Sweep::Focus => blend(
                        look::icon(),
                        look::foreground(),
                        sweep.opacity(look::ICON_BOX / 2.0 - left.get()),
                    ),
                })
                .child(icon()),
            label,
        ),
        None => view()
            .flex_1()
            .min_w_0()
            .flex_row()
            .items_center()
            .child(label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_band_follows_t3s_mask_and_the_shine_peaks_in_its_middle() {
        for (x, opacity) in [
            (0.0, 0.0),
            (10.8, 0.12),
            (36.0, 1.0),
            (61.2, 0.12),
            (72.0, 0.0),
        ] {
            assert!((Sweep::Focus.opacity(x) - opacity).abs() < 1e-6, "{x}");
        }
        assert!((Sweep::Focus.opacity(18.0) - 0.335).abs() < 1e-6);
        assert_eq!(Sweep::Focus.opacity(-1.0), 0.0);
        assert_eq!(Sweep::Focus.opacity(80.0), 0.0);
        assert_eq!(Sweep::Shine.opacity(36.0), 1.0);
        assert_eq!(Sweep::Shine.opacity(18.0), 0.5);
        assert_eq!(Sweep::Shine.opacity(0.0), 0.0);
        assert_eq!(Sweep::Shine.opacity(100.0), 0.0);
    }

    #[test]
    fn the_band_enters_before_the_label_and_repeats_after_thirty_steps() {
        for sweep in [Sweep::Focus, Sweep::Shine] {
            assert_eq!(sweep.band_left(0, 100.0), -BAND);
            assert_eq!(sweep.band_left(STEPS, 100.0), -BAND);
        }
        // T3's `translateX(100%)` of a label and both margins, and the shine's
        // `background-position` from `-4.5rem` to `100% + 4.5rem`.
        assert_eq!(
            Sweep::Focus.band_left(15, 100.0),
            -BAND + (100.0 + 2.0 * BAND) / 2.0
        );
        assert_eq!(
            Sweep::Shine.band_left(15, 100.0),
            -BAND + (100.0 + BAND) / 2.0
        );
        assert!(Sweep::Focus.band_left(STEPS - 1, 100.0) > 100.0);
    }

    #[test]
    fn glyphs_fall_before_in_or_after_the_band_and_keep_their_order() {
        let centers: Vec<f32> = (0..20).map(|index| index as f32 * 10.0 + 5.0).collect();
        let label = "abcdefghijklmnopqrst";
        let split = parts(label, &centers, 40.0);
        assert_eq!(split.concat(), label);
        assert_eq!(split[0], "abcd");
        // Slices of 9 points from 40: e (45) and f (55) in the first two.
        assert_eq!(split[1], "e");
        assert_eq!(split[2], "f");
        // The band ends at 112: l (115) is past it.
        assert_eq!(split[SLICES], "k");
        assert_eq!(split[SLICES + 1], "lmnopqrst");
        assert_eq!(parts(label, &centers, 500.0)[0], label);
        assert_eq!(parts(label, &centers, -500.0)[SLICES + 1], label);
    }

    #[test]
    fn blending_runs_from_the_muted_color_to_the_foreground() {
        assert_eq!(blend(look::muted(), look::foreground(), 0.0), look::muted());
        assert_eq!(
            blend(look::muted(), look::foreground(), 1.0),
            look::foreground()
        );
    }
}
