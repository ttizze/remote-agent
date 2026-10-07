//! The composer: its primary action, placeholder, controls, menus and drafts.
pub mod actions;
pub mod chips;
pub mod commands;
pub mod context_meter;
pub mod controls;
pub mod dictation;
pub mod hero;
pub mod menu;
pub mod prompt;
pub mod stash;
pub mod view;

/// `120000` as `120,000`, as the web formats counts in English.
fn group_thousands(value: impl ToString) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
