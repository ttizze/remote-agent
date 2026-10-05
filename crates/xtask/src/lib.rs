//! Repository verification and diagnostics tools.
#[cfg(unix)]
pub mod build_cleanup;
pub mod connection_diagnostics;
pub mod supervision;
#[cfg(unix)]
pub mod terminal_probe;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(all(test, unix))]
#[path = "../tests/support/mod.rs"]
mod test_support;
