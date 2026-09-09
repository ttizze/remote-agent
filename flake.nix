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
          androidSdkOptions = {
            platformVersions = [ "37.0" ];
            # AGP's KMP library target requires its default 36.0.0 build tools.
            buildToolsVersions = [ "36.0.0" "37.0.0" ];
            includeNDK = true;
            includeEmulator = false;
            includeSystemImages = false;
          };
          androidSdk = (pkgs.androidenv.composeAndroidPackages androidSdkOptions).androidsdk;
          androidUiSdk = (pkgs.androidenv.composeAndroidPackages (androidSdkOptions // {
            includeEmulator = true;
            includeSystemImages = true;
            systemImageTypes = [ "google_apis" ];
            abiVersions = [ (if pkgs.stdenv.hostPlatform.isAarch64 then "arm64-v8a" else "x86_64") ];
          })).androidsdk;
        in
        rec {
          default = pkgs.mkShell {
            packages = with pkgs; [
              gradle_9
              jdk21
              rustToolchain
              cargo-ndk
              flyctl
              beamPackages.elixir
              beamPackages.elixir-ls
              kotlin-language-server
              nodejs_22
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
          android-ui = pkgs.mkShell {
            inputsFrom = [ default ];
            packages = [ androidUiSdk ];
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidUiSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidUiSdk}/libexec/android-sdk";
          };
        });
    };
}
