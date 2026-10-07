//! The terminal's text size: the device's choice, kept within the mobile
//! bounds, and the "Text size" steps of the terminal menu.

pub const DEFAULT_TERMINAL_FONT_SIZE: f64 = 10.5;
pub const TERMINAL_FONT_SIZE_STEP: f64 = 0.5;
pub const MIN_TERMINAL_FONT_SIZE: f64 = 6.0;
pub const MAX_TERMINAL_FONT_SIZE: f64 = 14.0;

/// The size to use: the stored one within the bounds, else the default.
pub fn normalize_terminal_font_size(value: Option<f64>) -> f64 {
    match value {
        Some(size) if size.is_finite() => {
            size.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE)
        }
        _ => DEFAULT_TERMINAL_FONT_SIZE,
    }
}

/// One step smaller (`larger` false) or larger, within the bounds.
pub fn step_terminal_font_size(current: f64, larger: bool) -> f64 {
    let step = if larger {
        TERMINAL_FONT_SIZE_STEP
    } else {
        -TERMINAL_FONT_SIZE_STEP
    };
    normalize_terminal_font_size(Some(current + step))
}

/// A "Text size" menu item: "A- 10.0 pt" or "A+ 11.0 pt".
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalTextSizeStep {
    pub label: String,
    /// The size the item sets.
    pub size: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalTextSize {
    /// Points.
    pub size: f64,
    pub decrease: TerminalTextSizeStep,
    pub increase: TerminalTextSizeStep,
}

/// The terminal's size and its menu steps for the stored choice.
pub fn terminal_text_size(stored: Option<f64>) -> TerminalTextSize {
    let size = normalize_terminal_font_size(stored);
    let smaller = (size - TERMINAL_FONT_SIZE_STEP).max(MIN_TERMINAL_FONT_SIZE);
    let larger = (size + TERMINAL_FONT_SIZE_STEP).min(MAX_TERMINAL_FONT_SIZE);
    TerminalTextSize {
        size,
        decrease: TerminalTextSizeStep {
            label: format!("A- {smaller:.1} pt"),
            size: step_terminal_font_size(size, false),
            enabled: size > MIN_TERMINAL_FONT_SIZE,
        },
        increase: TerminalTextSizeStep {
            label: format!("A+ {larger:.1} pt"),
            size: step_terminal_font_size(size, true),
            enabled: size < MAX_TERMINAL_FONT_SIZE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_default_size_for_missing_or_invalid_values() {
        assert_eq!(
            normalize_terminal_font_size(None),
            DEFAULT_TERMINAL_FONT_SIZE
        );
        assert_eq!(
            normalize_terminal_font_size(Some(f64::NAN)),
            DEFAULT_TERMINAL_FONT_SIZE
        );
    }

    #[test]
    fn clamps_below_the_minimum() {
        assert_eq!(
            normalize_terminal_font_size(Some(MIN_TERMINAL_FONT_SIZE - 4.0)),
            MIN_TERMINAL_FONT_SIZE
        );
    }

    #[test]
    fn clamps_above_the_maximum() {
        assert_eq!(
            normalize_terminal_font_size(Some(MAX_TERMINAL_FONT_SIZE + 4.0)),
            MAX_TERMINAL_FONT_SIZE
        );
    }

    #[test]
    fn preserves_in_range_values() {
        assert_eq!(normalize_terminal_font_size(Some(9.5)), 9.5);
    }

    #[test]
    fn steps_terminal_sizes_within_bounds() {
        assert_eq!(step_terminal_font_size(10.5, true), 11.0);
        assert_eq!(step_terminal_font_size(10.5, false), 10.0);
        assert_eq!(step_terminal_font_size(6.0, false), 6.0);
        assert_eq!(step_terminal_font_size(14.0, true), 14.0);
    }

    #[test]
    fn the_menu_names_the_next_sizes_and_disables_a_step_at_its_bound() {
        let default = terminal_text_size(None);
        assert_eq!(default.size, 10.5);
        assert_eq!(default.decrease.label, "A- 10.0 pt");
        assert_eq!(default.increase.label, "A+ 11.0 pt");
        assert!(default.decrease.enabled && default.increase.enabled);
        let smallest = terminal_text_size(Some(6.0));
        assert_eq!(smallest.decrease.label, "A- 6.0 pt");
        assert!(!smallest.decrease.enabled);
        assert_eq!(smallest.increase.size, 6.5);
        let largest = terminal_text_size(Some(14.0));
        assert_eq!(largest.increase.label, "A+ 14.0 pt");
        assert!(!largest.increase.enabled);
    }
}
