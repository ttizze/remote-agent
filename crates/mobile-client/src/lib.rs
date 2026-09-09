//! Native platform bindings for agent-core.
#[cfg(feature = "jni")]
mod android_jni;
pub mod client;
pub mod ffi;
#[cfg(test)]
mod tests;
mod transport;
