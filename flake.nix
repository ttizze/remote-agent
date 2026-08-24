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
          }));
    in
    {
      devShells = forEachSystem (pkgs:
        let
          rustToolchain = pkgs.rust-bin.stable.latest.minimal.override {
            extensions = [ "clippy" "rustfmt" ];
            targets = [
              "aarch64-apple-ios"
              "aarch64-apple-ios-sim"
              "aarch64-linux-android"
              "x86_64-linux-android"
            ];
          };
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              gradle
              jdk21
              rustToolchain
            ];

            JAVA_HOME = pkgs.jdk21.home;

            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export MOBILE_CARGO="${rustToolchain}/bin/cargo"
              export MOBILE_RUSTC="${rustToolchain}/bin/rustc"
              if [ -d /Applications/Xcode.app/Contents/Developer ]; then
                export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
                unset SDKROOT
              fi
            '';
          };
        });
    };
}
