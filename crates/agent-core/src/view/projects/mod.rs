//! Projects: their actions, adding one, choosing one for a new task, and
//! importing existing agent sessions.
pub mod add;
pub mod import;
pub mod paths;
pub mod scripts;
pub mod selection;

/// An approximation of `localeCompare` for names: case-insensitive first,
/// lowercase before uppercase on ties.
pub(crate) fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| right.cmp(left))
}
