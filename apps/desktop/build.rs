fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI's debug render stack exceeds the Windows executable default.
        println!("cargo:rustc-link-arg-bin=bex-desktop=/STACK:16777216");
    }
}
