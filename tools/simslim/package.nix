{ lib, callPackage }:

let
  version = "0.14.0";
  mkReleaseBinary = callPackage ../github-release-binary.nix { };
in
mkReleaseBinary {
  pname = "simslim";
  inherit version;
  url = _: "https://github.com/MobAI-App/simslim/releases/download/v${version}/simslim-v${version}-macos-arm64.tar.gz";
  hashes.aarch64-apple-darwin = "a3db0de6f428f9f8fd5dff256b122f98791c138521dc1330d99643719d8fe9ff";
  meta = {
    description = "Reduce Simulator background services and memory usage";
    homepage = "https://github.com/MobAI-App/simslim";
    license = lib.licenses.mit;
  };
}
