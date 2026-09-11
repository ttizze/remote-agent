//! Isolated Host, Codex subprocess and pairing fixtures.
pub mod fixture;
pub mod pairing;
pub mod test_support;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
