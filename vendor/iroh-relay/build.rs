use cfg_aliases::cfg_aliases;

fn main() {
    println!("cargo:rerun-if-changed=src/client/tcp_info_apple.c");
    if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        cc::Build::new()
            .file("src/client/tcp_info_apple.c")
            .compile("bex_relay_tcp_info");
    }
    // Setup cfg aliases
    cfg_aliases! {
        // Convenience aliases
        wasm_browser: { all(target_family = "wasm", target_os = "unknown") },
        with_crypto_provider: { any(feature = "tls-ring", feature = "tls-aws-lc-rs") }
    }
}
