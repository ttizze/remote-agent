{
  description = "Remote Agent development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay.url = "github:oxalica/rust-overlay";
  inputs.rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      # The release runtime serves Host Preview recording and the native
      # desktop/device transcode path. Keep it reviewable: a small shared
      # FFmpeg output provides the built-in H.264/MJPEG codecs, libvpx VP8/VP9,
      # image/WebM muxers, pipe I/O, and swscale without shipping the full
      # codec/filter closure. The staging script verifies the resulting binary
      # rather than trusting this option list alone.
      ffmpegRuntime = pkgs:
        (pkgs.ffmpeg.override {
          ffmpegVariant = "small";
          withHeadlessDeps = false;
          withSmallDeps = false;
          withFullDeps = false;
          withVpx = true;
          withGPL = true;
          withVersion3 = true;
          withNetwork = false;
          withStatic = false;
          withShared = true;
          withSmallBuild = true;
          buildFfmpeg = true;
          buildFfplay = false;
          buildFfprobe = false;
          buildQtFaststart = false;
          buildAvcodec = true;
          buildAvdevice = false;
          buildAvfilter = true;
          buildAvformat = true;
          buildAvresample = false;
          buildAvutil = true;
          # The pinned FFmpeg 9.0 test suite compiles tests/pixelutils.c even
          # for the small variant; keep the built-in utility enabled so checks
          # and the runtime use the same configured subsystem.
          withPixelutils = true;
          buildPostproc = false;
          buildSwresample = true;
          # H.264/MJPEG frames need pixel-format and size conversion before the
          # MJPEG output used by desktop/device previews. Keep this enabled in
          # every pinned release build.
          buildSwscale = true;
          withDocumentation = false;
          withHtmlDoc = false;
          withManPages = false;
          withPodDoc = false;
          withTxtDoc = false;
        }).overrideAttrs (old: {
          # FFmpeg's upstream tools list unconditionally builds uncoded_frame,
          # but that helper calls avdevice_register_all. The release runtime
          # intentionally omits libavdevice, so gate only this incompatible
          # helper and retain the rest of the check suite.
          postPatch = (old.postPatch or "") + ''
            substituteInPlace tools/Makefile \
              --replace-fail \
                "TOOLS = enc_recon_frame_test enum_options qt-faststart scale_slice_test trasher uncoded_frame" \
                "TOOLS = enc_recon_frame_test enum_options qt-faststart scale_slice_test trasher"
          '';
        });
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
        ffmpeg = ffmpegRuntime pkgs;
        kani = pkgs.callPackage ./tools/kani/package.nix { };
        kache = pkgs.callPackage ./tools/kache/package.nix { };
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
              export KACHE_CACHE_DIR="''${KACHE_CACHE_DIR:-$HOME/${if pkgs.stdenv.hostPlatform.isDarwin then "Library/Caches/kache" else ".cache/kache"}}"
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
          android = pkgs.mkShell {
            packages = [ androidSdk pkgs.jdk21 pkgs.cargo-ndk pkgs.jq pkgs.git
              (pkgs.rust-bin.stable.latest.minimal.override {
                extensions = [ "rustfmt" "clippy" ];
                targets = [ "aarch64-linux-android" "x86_64-linux-android" ];
              })
            ];
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";
            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export DEVELOPER_DIR="''${APP_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
            '';
          };
          kani = pkgs.mkShell {
            packages = with pkgs; [
              (callPackage ./tools/kani/package.nix { })
              rustToolchain just git clang pkg-config
            ];
          };
          native = pkgs.mkShell {
            RUST_TOOLCHAIN_VERSION = rustToolchain.version;
            NEXTEST_VERSION = pkgs.cargo-nextest.version;
            packages = with pkgs; [ rustToolchain kache cargo-mutants cargo-nextest just jq git pkg-config cmake clang workflowLinter nodejs (ffmpegRuntime pkgs) ]
              ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                patchelf
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
              (ffmpegRuntime pkgs)
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
              if [ -d "''${APP_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}" ]; then
                export DEVELOPER_DIR="''${APP_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
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
