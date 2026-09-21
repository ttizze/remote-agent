{
  description = "Remote Agent development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay.url = "github:oxalica/rust-overlay";
  inputs.rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      supportedSystems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forEachSystem = function:
        nixpkgs.lib.genAttrs supportedSystems (system:
          function (import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
            config = {
              allowUnfree = true;
              android_sdk.accept_license = true;
            };
          }));
    in
    {
      packages = forEachSystem (pkgs: {
        agent-peer = pkgs.callPackage ./tools/agent-peer/package.nix { };
      });
      devShells = forEachSystem (pkgs:
        let
          rustToolchain = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
            targets = [
              "aarch64-apple-ios"
              "aarch64-apple-ios-sim"
              "aarch64-linux-android"
              "x86_64-linux-android"
            ];
          };
          androidSdk = (pkgs.androidenv.composeAndroidPackages {
            platformVersions = [ "37.0" ];
            buildToolsVersions = [ "36.0.0" ];
            includeNDK = true;
            includeEmulator = false;
            includeSystemImages = false;
          }).androidsdk;
          androidTestSdk = (pkgs.androidenv.composeAndroidPackages {
            platformVersions = [ "37.0" ];
            buildToolsVersions = [ "36.0.0" ];
            includeEmulator = true;
            includeSystemImages = true;
            systemImageTypes = [ "google_apis" ];
            abiVersions = [ (if pkgs.stdenv.hostPlatform.isAarch64 then "arm64-v8a" else "x86_64") ];
          }).androidsdk;
        in
        {
          native = pkgs.mkShell {
            RUST_TOOLCHAIN_VERSION = rustToolchain.version;
            packages = with pkgs; [ rustToolchain just jq python3 git lsof pkg-config cmake clang ]
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                alsa-lib fontconfig freetype libxkbcommon wayland libGL vulkan-loader
                libxcb libX11 libXcursor libXi libXrandr
                openssl gtk3 webkitgtk_4_1 procps
              ];
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux
              (pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader pkgs.libGL pkgs.libxkbcommon pkgs.wayland ]);
          };
          android-test = pkgs.mkShell {
            packages = [ androidTestSdk pkgs.jdk21 pkgs.python3 ];
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidTestSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidTestSdk}/libexec/android-sdk";
          };
          default = pkgs.mkShell {
            packages = with pkgs; [
              just
              jq
              python3
              lsof
              shellcheck
              gradle
              jdk21
              rustToolchain
              cargo-ndk
              lefthook
              kotlin-language-server
              androidSdk
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              pkgs.swiftlint
              pkgs.swiftformat
            ];

            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";

            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export MOBILE_CARGO="${rustToolchain}/bin/cargo"
              export MOBILE_RUSTC="${rustToolchain}/bin/rustc"
              if [ -d /Applications/Xcode.app/Contents/Developer ]; then
                export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
                unset SDKROOT
                # Xcode expects to drive clang itself. Nix's LD override makes
                # xcodebuild invoke ld directly with clang-only -Xlinker flags.
                unset LD
              fi
            '';
          };
        });
    };
}
