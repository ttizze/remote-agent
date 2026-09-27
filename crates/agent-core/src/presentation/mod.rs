//! Conversation display policy shared by native desktop and mobile clients.
//! Grouping borrows item metadata; body formatting runs only for changed items
//! or explicit expansion. Native views own rendering and local expansion state.

pub mod body;
pub mod conversation;
pub mod diff;
pub mod error;
pub mod grouping;
pub mod item;
pub mod list;
pub mod markdown;
pub mod model_settings;

pub use grouping::{ItemMetadata, Role, Segment, project, project_items, source_order};
pub use item::{ItemPresentation, item_presentation};
