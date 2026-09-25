{ lib, stdenv, stdenvNoCC, fetchurl, rust-bin, autoPatchelfHook, makeWrapper }:
let
  version = "0.68.0";
  target = stdenvNoCC.hostPlatform.rust.rustcTarget;
  hashes = {
    aarch64-apple-darwin = "a5d39a5d5e748253a553aa62f295c6c397287927a28e5e32691d7ff2eda0c398";
    x86_64-apple-darwin = "b1367f694c1350abcae133701f78b3be3fd6e9beb66681a22ea0bcac0cc34808";
    aarch64-unknown-linux-gnu = "0042d2688bf1550270def4a79c96aaeea138631818cb4ab5480120f2d51a942c";
    x86_64-unknown-linux-gnu = "32e2b484d73ede0bbf64a2cf0879c4259422497d8aec4ee67d448de9ae7843d3";
  };
  # Must match the compiler used to build this Kani release.
  toolchain = rust-bin.nightly."2026-08-21".minimal.override {
    extensions = [ "rustc-dev" "llvm-tools" ];
  };
in
stdenvNoCC.mkDerivation {
  pname = "kani";
  inherit version;
  src = fetchurl {
    url = "https://github.com/model-checking/kani/releases/download/kani-${version}/kani-${version}-${target}.tar.gz";
    sha256 = hashes.${target};
  };
  nativeBuildInputs = [ makeWrapper ] ++ lib.optionals stdenvNoCC.hostPlatform.isLinux [ autoPatchelfHook ];
  buildInputs = [ toolchain ] ++ lib.optionals stdenvNoCC.hostPlatform.isLinux [ stdenv.cc.cc.lib ];
  dontStrip = true;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -R . "$out/"
    ln -s ${toolchain} "$out/toolchain"
    ln -s kani-driver "$out/bin/kani"
    ln -s kani-driver "$out/bin/cargo-kani"
    wrapProgram "$out/bin/goto-cc" --add-flags "--native-compiler ${stdenv.cc}/bin/cc"
    runHook postInstall
  '';
  meta = {
    description = "Kani Rust verifier with its matching Rust toolchain";
    homepage = "https://model-checking.github.io/kani/";
    license = with lib.licenses; [ asl20 mit ];
    platforms = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
  };
}
