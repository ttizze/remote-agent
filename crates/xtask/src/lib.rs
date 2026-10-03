//! Repository verification and diagnostics tools.
#[cfg(unix)]
pub mod android_e2e;
#[cfg(unix)]
pub mod build_cleanup;
pub mod connection_diagnostics;
#[cfg(unix)]
pub mod ios_e2e;
#[cfg(unix)]
pub mod ios_markdown;
pub mod supervision;
#[cfg(unix)]
pub mod terminal_probe;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(all(test, unix))]
#[path = "../tests/support/mod.rs"]
mod test_support;

#[cfg(unix)]
macro_rules! args {
    (vec; $($value:expr),* $(,)?) => { Vec::from($crate::args![$($value),*]) };
    ($($value:expr),* $(,)?) => { [$(std::ffi::OsString::from($value)),*] };
}
#[cfg(unix)]
pub(crate) use args;
