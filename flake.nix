{
  description = "Remote Agent development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay.url = "github:oxalica/rust-overlay";
  inputs.rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      supportedSystems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
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
        kani = pkgs.callPackage ./tools/kani/package.nix { };
        kache = pkgs.callPackage ./tools/kache/package.nix { };
        maestro = pkgs.callPackage ./tools/maestro/package.nix { };
      });
      devShells = forEachSystem (pkgs:
        let
          # Queue validation is not released yet: https://github.com/rhysd/actionlint/pull/654
          workflowLinter = pkgs.actionlint.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ [ (pkgs.fetchurl {
              url = "https://github.com/rhysd/actionlint/commit/644076a59742c2d1540ebd4686eab3c308f0e562.patch";
              hash = "sha256-H2y2MlM35dnlSmt7DFYRKVOzDRYzJ2Tw8hWR5nUnwak=";
            }) ];
            postCheck = (old.postCheck or "") + ''
              go test . -run '^TestLinterLint(OK|Error)$'
            '';
          });
          rustToolchain = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
            targets = [
              "aarch64-apple-ios"
              "aarch64-apple-ios-sim"
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              "aarch64-linux-android"
              "x86_64-linux-android"
            ];
          };
          kache = pkgs.callPackage ./tools/kache/package.nix { };
          kacheHook = ''
            # CI already restores Cargo outputs.
            if [ -z "''${CI:-}" ]; then
              export RUSTC_WRAPPER="${kache}/bin/kache"
              if [ -z "''${KACHE_CACHE_DIR:-}" ]; then
                bex_kache_build_root=$(git config --path --get bex.buildRoot 2>/dev/null || true)
                if [ -n "$bex_kache_build_root" ]; then
                  if [ "''${bex_kache_build_root#/}" = "$bex_kache_build_root" ] || [ ! -d "$bex_kache_build_root" ]; then
                    printf 'Build storage is unavailable: %s\n' "$bex_kache_build_root" >&2
                    exit 1
                  fi
                  # Keep the shared compiler cache on the build volume for CoW restores.
                  export KACHE_CACHE_DIR="$bex_kache_build_root/.kache"
                else
                  export KACHE_CACHE_DIR="$HOME/${if pkgs.stdenv.hostPlatform.isDarwin then "Library/Caches/kache" else ".cache/kache"}"
                fi
                unset bex_kache_build_root
              fi
            fi
          '';
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
          kani = pkgs.mkShell {
            packages = with pkgs; [
              (callPackage ./tools/kani/package.nix { })
              rustToolchain just git clang pkg-config
            ];
          };
          native = pkgs.mkShell {
            RUST_TOOLCHAIN_VERSION = rustToolchain.version;
            NEXTEST_VERSION = pkgs.cargo-nextest.version;
            packages = with pkgs; [ rustToolchain kache cargo-mutants cargo-nextest just jq git pkg-config cmake clang workflowLinter nodejs ]
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                lsof
                alsa-lib fontconfig freetype libxkbcommon wayland libGL vulkan-loader
                libxcb libX11 libXcursor libXi libXrandr
                openssl gtk3 webkitgtk_4_1 procps
              ];
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux
              (pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader pkgs.libGL pkgs.libxkbcommon pkgs.wayland ]);
            shellHook = kacheHook + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export PATH="/usr/sbin:$PATH"
            '';
          };
          android-test = pkgs.mkShell {
            packages = [ androidTestSdk pkgs.jdk21 rustToolchain pkgs.coreutils ];
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidTestSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidTestSdk}/libexec/android-sdk";
          };
          default = pkgs.mkShell ({
            packages = with pkgs; [
              just
              jq
              shellcheck
              workflowLinter
              nodejs
              rustToolchain
              kache
              cargo-mutants
              cargo-nextest
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              lsof
              gradle
              jdk21
              cargo-ndk
              kotlin-language-server
              androidSdk
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              pkgs.swiftlint
              pkgs.swiftformat
            ];

            shellHook = kacheHook + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              # Use macOS's kernel-matched process inspector. The Nix lsof
              # build scans this host much more slowly under Simulator load.
              export PATH="/usr/sbin:$PATH"
              # The Nix compiler setup overwrites DEVELOPER_DIR with its own SDK.
              if [ -d "''${BEX_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}" ]; then
                export DEVELOPER_DIR="''${BEX_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
                unset SDKROOT
                # Xcode expects to drive clang itself. Nix's LD override makes
                # xcodebuild invoke ld directly with clang-only -Xlinker flags.
                unset LD
              fi
            '';
          } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";
            ANDROID_NDK_HOME = "${androidSdk}/libexec/android-sdk/ndk-bundle";
          });
        });
    };
}
