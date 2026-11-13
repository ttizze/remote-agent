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
        cargo = pkgs.callPackage ./tools/cargo/package.nix { };
        maestro = pkgs.callPackage ./tools/maestro/package.nix { };
      } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
        node-runtime = pkgs.callPackage ./tools/node-runtime/package.nix { };
      });
      devShells = forEachSystem (pkgs:
        let
          # Queue validation is not released yet: https://github.com/rhysd/actionlint/pull/654
          workflowLinter = pkgs.actionlint.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ [ (pkgs.fetchpatch {
              url = "https://github.com/rhysd/actionlint/commit/644076a59742c2d1540ebd4686eab3c308f0e562.patch";
              hash = "sha256-Lpd7CTkLbzgvAZV2H8G4GTUoPNDCK7PiW3A40+cquUk=";
            }) ];
            postCheck = (old.postCheck or "") + ''
              go test . -run '^TestLinterLint(OK|Error)$'
            '';
          });
          buildRustToolchain = pkgs.rust-bin.stable.latest.minimal;
          hostRustToolchain = buildRustToolchain.override {
            extensions = [ "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
          };
          mobileRustToolchain = hostRustToolchain.override {
            targets = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              "aarch64-apple-ios"
              "aarch64-apple-ios-sim"
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              "aarch64-linux-android"
              "x86_64-linux-android"
            ];
          };
          cccc = pkgs.callPackage ./tools/cccc/package.nix { };
          cargo = pkgs.callPackage ./tools/cargo/package.nix { };
          withCargo = toolchain: pkgs.symlinkJoin {
            name = "rust-${toolchain.version}-cargo-huacnlee";
            inherit (toolchain) version;
            paths = [ toolchain ];
            postBuild = ''
              ln -sfn ${cargo}/bin/cargo "$out/bin/cargo"
            '';
          };
          nodeRuntime = pkgs.callPackage ./tools/node-runtime/package.nix { };
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
          androidBuildEnvironment = {
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidSdk}/libexec/android-sdk";
            ANDROID_NDK_HOME = "${androidSdk}/libexec/android-sdk/ndk-bundle";
          };
          nativeBuildTools = with pkgs; [ pkg-config cmake clang ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.patchelf ];
          # The Host and its tests use these on Linux.
          linuxHostTools = with pkgs; [ lsof openssl procps ];
          runtimeShellHook = ''
            # Keep child processes and interactive sessions on the pinned shell.
            export SHELL="${pkgs.zsh}/bin/zsh"
            ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              # Use the kernel-matched process inspector on macOS.
              export PATH="/usr/sbin:$PATH"
            ''}
            case $- in *i*) exec "$SHELL" ;; esac
          '';
          appleSdkHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
            # The Nix compiler setup overwrites DEVELOPER_DIR with its own SDK.
            if [ -d "''${AGENT_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}" ]; then
              export DEVELOPER_DIR="''${AGENT_XCODE_DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
              unset SDKROOT
              # Xcode expects clang, not Nix's LD override with clang-only flags.
              unset LD
            fi
          '';
          mkDevShell = attrs: pkgs.mkShell (attrs // {
            RUSTC_WRAPPER = "";
            packages = [ pkgs.zsh ] ++ (attrs.packages or [ ]);
            shellHook = (attrs.shellHook or "") + runtimeShellHook;
          });
        in
        {
          repository = pkgs.mkShellNoCC {
            packages = with pkgs; [ bash git jq just nodejs perl powershell workflowLinter cccc ];
          };
          kani = mkDevShell {
            packages = with pkgs; [
              (callPackage ./tools/kani/package.nix { })
              hostRustToolchain just git clang pkg-config
            ];
          };
          native = mkDevShell {
            AGENT_NODE_RUNTIME = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin "${nodeRuntime}";
            RUST_TOOLCHAIN_VERSION = hostRustToolchain.version;
            NEXTEST_VERSION = pkgs.cargo-nextest.version;
            packages = with pkgs; [ (withCargo hostRustToolchain) cccc cargo-mutants cargo-nextest just jq git perl ] ++ nativeBuildTools
              ++ [ workflowLinter nodejs powershell ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux linuxHostTools;
            shellHook = appleSdkHook;
          };
          ios-test = pkgs.mkShellNoCC {
            packages = with pkgs; [ just gitMinimal nodejs gh zsh ];
            shellHook = appleSdkHook + runtimeShellHook;
          };
          android-test = pkgs.mkShell {
            packages = [ androidTestSdk pkgs.jdk21 (withCargo buildRustToolchain) pkgs.coreutils pkgs.zsh ];
            RUSTC_WRAPPER = "";
            JAVA_HOME = pkgs.jdk21.home;
            ANDROID_HOME = "${androidTestSdk}/libexec/android-sdk";
            ANDROID_SDK_ROOT = "${androidTestSdk}/libexec/android-sdk";
            shellHook = runtimeShellHook;
          };
          default = mkDevShell ({
            AGENT_NODE_RUNTIME = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin "${nodeRuntime}";
            packages = with pkgs; [
              just
              jq
              shellcheck
              workflowLinter
              nodejs
              perl
              powershell
              (withCargo mobileRustToolchain)
              cccc
              cargo-mutants
              cargo-nextest
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (nativeBuildTools ++ linuxHostTools ++ [
              # Host worktree management needs a newer Git than many distributions ship.
              git
              gradle
              jdk21
              cargo-ndk
              kotlin-language-server
              androidSdk
            ]) ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              pkgs.swiftlint
              pkgs.swiftformat
            ];

            shellHook = appleSdkHook;
          } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux androidBuildEnvironment);
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          host-build = mkDevShell {
            packages = with pkgs; [
              (withCargo buildRustToolchain) just jq gitMinimal nodejs pkg-config cmake clang
            ];
          };
          android-build = pkgs.mkShell (androidBuildEnvironment // {
            packages = with pkgs; [
              (withCargo (buildRustToolchain.override {
                targets = [ "aarch64-linux-android" "x86_64-linux-android" ];
              }))
              zsh just jq gitMinimal nodejs cargo-ndk jdk21 androidSdk
            ];
            RUSTC_WRAPPER = "";
            shellHook = runtimeShellHook;
          });
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          ios-build = mkDevShell {
            # Browser acceptance packages the Mac app as the fixture's native browser owner.
            AGENT_NODE_RUNTIME = "${nodeRuntime}";
            packages = with pkgs; [
              (withCargo (buildRustToolchain.override {
                targets = [ "aarch64-apple-ios-sim" ];
              }))
              just jq gitMinimal nodejs swiftlint swiftformat
            ];
            shellHook = appleSdkHook;
          };
        });
    };
}
