//! QUIC client core for the Bex Mobile Client.
//!
//! The caller owns the device PKCS#8 document (normally in platform secure
//! storage). It is supplied only while connecting, used to sign the pairing
//! or authentication proof, and is never retained or logged by this crate.

mod client;
mod rpc;
mod transport;

pub mod ffi;

#[cfg(target_os = "android")]
mod android_jni;

pub use client::{
    ConnectedHost, MobileClient, MobileClientConfig, MobileClientError, Notification, ServerRequest,
};

#[cfg(test)]
mod tests;
