{ lib, rust-bin, makeRustPlatform, fetchurl, pkg-config, openssl, curl, zlib, git, makeWrapper, cacert }:

let
  # Cargo itself needs a newer compiler; application builds keep stable Rust.
  toolchain = rust-bin.nightly."2026-08-21".minimal;
  rustPlatform = makeRustPlatform { cargo = toolchain; rustc = toolchain; };
in
rustPlatform.buildRustPackage {
  pname = "cargo-huacnlee";
  version = "0.102.0-9-pr10-pr11";
  src = fetchurl {
    url = "https://github.com/huacnlee/cargo/archive/refs/tags/v0.102.0-9.tar.gz";
    sha256 = "49a63fc1e05890657c151034f67c2217a51aa65c7835ce423e1aa3bc2379547e";
  };
  # Protect queued units from GC and keep patched dependency types identical.
  patches = [ (fetchurl {
    # https://github.com/huacnlee/cargo/pull/10
    url = "https://github.com/huacnlee/cargo/compare/6ebb89197f5b3a88b637e28672e6d39d67343401...c8890c4515732718349ac350672e8ed900b77c51.diff";
    sha256 = "54ff6964566909af75921ef73dfa89813980d62d8b6386df025b960a2b476fa2";
  }) (fetchurl {
    # https://github.com/huacnlee/cargo/pull/11
    url = "https://github.com/huacnlee/cargo/compare/6ebb89197f5b3a88b637e28672e6d39d67343401...0d289379554626589abec942295dec40cf4468aa.diff";
    sha256 = "5262aadcbf2a70e70bc5a18c752fa2983b7566ef90ab66b2707031f71cc405b6";
  }) ];
  cargoHash = "sha256-c2jOozCbWUIq1rrqQm4KXNglyWgbc1dljaJn6hhv31U=";
  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = [ openssl curl zlib ];
  nativeCheckInputs = [ git ];
  cargoBuildFlags = [ "--package" "cargo" "--bin" "cargo" ];
  cargoTestFlags = [ "--package" "cargo" "--test" "testsuite" "fork_defaults::" ];
  CARGO_FORK_AUTO_UPDATE = "0";
  postInstall = ''
    wrapProgram "$out/bin/cargo" \
      --set CARGO_FORK_AUTO_UPDATE 0 \
      --set-default SSL_CERT_FILE "${cacert}/etc/ssl/certs/ca-bundle.crt"
  '';
  meta = {
    description = "Cargo with shared build storage and queued-unit GC protection";
    homepage = "https://github.com/huacnlee/cargo";
    license = with lib.licenses; [ mit asl20 ];
    platforms = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
    mainProgram = "cargo";
  };
}
