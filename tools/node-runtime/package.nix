{ lib, stdenvNoCC, fetchurl }:
let
  version = "24.21.0";
in
stdenvNoCC.mkDerivation {
  pname = "bex-node-runtime";
  inherit version;
  src = fetchurl {
    url = "https://nodejs.org/dist/v${version}/node-v${version}-darwin-arm64.tar.xz";
    sha256 = "6239d4cf92d864487ec8cd3615038f7b67e7f58b77b21cd2f09ea9fbd68065fe";
  };
  # Keep the official portable binary and its signature intact in the store.
  dontFixup = true;
  installPhase = ''
    runHook preInstall
    install -Dm755 bin/node "$out/bin/node"
    install -Dm644 LICENSE "$out/share/doc/node/LICENSE"
    runHook postInstall
  '';
  meta = {
    description = "Portable Node.js runtime for the Mac app";
    homepage = "https://nodejs.org/";
    license = lib.licenses.mit;
    platforms = [ "aarch64-darwin" ];
  };
}
