fn main() {
    println!("cargo:rerun-if-env-changed=BEX_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=BEX_BUILD_CHANNEL");
    let channel = std::env::var("BEX_BUILD_CHANNEL").unwrap_or_else(|_| "main".into());
    let app_name = match channel.as_str() {
        "main" => "bex",
        "dev" => "Bex dev",
        _ => panic!("BEX_BUILD_CHANNEL must be main or dev"),
    };
    println!("cargo:rustc-env=BEX_APP_NAME={app_name}");
    println!(
        "cargo:rustc-env=BEX_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap()
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI's debug render stack exceeds the Windows executable default.
        println!("cargo:rustc-link-arg-bin=bex-desktop=/STACK:16777216");
    }
}
