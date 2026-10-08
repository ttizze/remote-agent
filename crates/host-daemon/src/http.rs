//! Shared HTTP client setup for Host-owned network operations.
//!
//! Reqwest is built with `rustls-no-provider`, so every cold client creation
//! must select the Host's pinned rustls backend before reqwest builds its
//! connector. Keeping that decision here prevents individual resource owners
//! from depending on process-start ordering or test-only initialization.

pub(crate) fn ensure_tls_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    ensure_tls_provider();
    reqwest::Client::builder()
}

pub(crate) fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("Host HTTP client configuration is valid")
}
