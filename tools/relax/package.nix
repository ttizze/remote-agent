{ lib, stdenv, rustPlatform, fetchFromGitHub, makeWrapper, openssl }:

rustPlatform.buildRustPackage {
  pname = "relax";
  version = "0.1.0-5aa7b49";

  src = fetchFromGitHub {
    owner = "makepad";
    repo = "relax";
    rev = "5aa7b49990ecdfb503b9af593f8a2d670ddb65e0";
    hash = "sha256-cXJO0SQ401fgnetF9FhkZH/lcp8ExnWeS6oDSTuGYUQ=";
  };

  # str UTF-8 constructors are backported from makepad/relax@f4e22466.
  patches = [ ./compat.patch ./precompile-tests.patch ];
  cargoLock.lockFile = ./Cargo.lock;
  postPatch = ''
    cp ${./Cargo.lock} Cargo.lock
  '';

  nativeBuildInputs = [ makeWrapper ];
  # The runner's native HTTP client links libssl and libcrypto on Linux.
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ openssl ];
  cargoBuildFlags = [ "-p" "relax-runner" "--bin" "relax" ];
  cargoTestFlags = [ "-p" "relax-compiler" "-p" "relax-front" "--lib" ];

  postInstall = ''
    mkdir -p "$out/share/relax"
    cp -r libs/relax/compiler/std "$out/share/relax/std"
    wrapProgram "$out/bin/relax" \
      --set MAKEPAD_RUN_STD "$out/share/relax/std"
  '';

  meta = {
    description = "Rust JIT compiler with the compatibility fixes used by Bex";
    homepage = "https://makepad.github.io/relax/";
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
    mainProgram = "relax";
  };
}
