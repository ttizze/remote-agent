{
  description = "Claude Code and Codex peer CLI and skill";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
      forEachSystem = function: nixpkgs.lib.genAttrs systems
        (system: function nixpkgs.legacyPackages.${system});
    in
    {
      packages = forEachSystem (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.agent-peer;
        agent-peer = pkgs.callPackage ./package.nix { };
      });
      checks = forEachSystem (pkgs: {
        agent-peer = self.packages.${pkgs.stdenv.hostPlatform.system}.agent-peer;
      });
      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell { packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy ]; };
      });
    };
}
