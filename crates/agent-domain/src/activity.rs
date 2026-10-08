//! Display bounds shared by operating-system activity delivery and rendering.

pub const ACTIVITY_SUMMARY_LIMIT: usize = 120;
pub const ACTIVITY_STATUS_LIMIT: usize = 40;
pub const ACTIVITY_LINK_LIMIT: usize = 512;
pub const ACTIVITY_ROWS_LIMIT: usize = 5;

/// Trims a display string and bounds it in UTF-16 units without splitting a
/// surrogate pair. The ellipsis is part of the display budget.
pub fn bounded_activity_text(text: &str, max_units: usize) -> String {
    let text = text.trim_matches(|c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace()));
    if text.encode_utf16().count() <= max_units {
        return text.to_owned();
    }
    let prefix_budget = max_units.saturating_sub(3);
    let mut units = 0;
    let end = text
        .char_indices()
        .find_map(|(offset, c)| {
            units += c.len_utf16();
            (units > prefix_budget).then_some(offset)
        })
        .unwrap_or(text.len());
    let prefix = text[..end]
        .trim_end_matches(|c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace()));
    format!("{prefix}{}", ".".repeat(max_units.min(3)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn display_bounds_trim_whitespace_and_reserve_the_ellipsis() {
        assert_eq!(
            bounded_activity_text("\u{feff} Project \u{3000}", 120),
            "Project"
        );
        assert_eq!(bounded_activity_text("\u{85}Project", 120), "\u{85}Project");
        assert_eq!(
            bounded_activity_text(&"x".repeat(121), 120),
            format!("{}...", "x".repeat(117))
        );
        assert_eq!(bounded_activity_text("abc  defg", 8), "abc...");
        assert_eq!(bounded_activity_text("😀😀😀", 5), "😀...");
    }

    proptest! {
        #[test]
        fn arbitrary_unicode_never_exceeds_the_display_budget(
            text in prop::collection::vec(any::<char>(), 0..2048),
            budget in 0usize..256,
        ) {
            let text = text.into_iter().collect::<String>();
            let bounded = bounded_activity_text(&text, budget);
            prop_assert!(bounded.encode_utf16().count() <= budget);
            prop_assert!(!bounded.contains('\u{fffd}') || text.contains('\u{fffd}'));
        }
    }
}
