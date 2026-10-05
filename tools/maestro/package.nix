{ lib, stdenvNoCC, fetchurl, unzip, zip, makeWrapper, jdk21 }:

let version = "2.11.0";
in
stdenvNoCC.mkDerivation {
  pname = "maestro";
  inherit version;
  src = fetchurl {
    url = "https://github.com/mobile-dev-inc/Maestro/releases/download/cli-${version}/maestro.zip";
    hash = "sha256-U4RZPLTnoQZInnWoIdFX3UP05Djfa8MIty6CxoXhKDo=";
  };
  nativeBuildInputs = [ unzip zip makeWrapper ];
  # The CLI compiles its bundled XCTest sources. Upstream logs every input,
  # including SecureField contents, into the device logs retained by CI.
  prePatch = ''
    unzip -q lib/maestro-cli-${version}.jar \
      driver/ios/maestro-driver-iosUITests/Routes/Helpers/TextInputHelper.swift
  '';
  patches = [ ./no-input-logging.patch ];
  postPatch = ''
    touch -t 198001010000 driver/ios/maestro-driver-iosUITests/Routes/Helpers/TextInputHelper.swift
    zip -Xq lib/maestro-cli-${version}.jar \
      driver/ios/maestro-driver-iosUITests/Routes/Helpers/TextInputHelper.swift
    rm -r driver
  '';
  dontBuild = true;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/libexec/maestro" "$out/bin"
    cp -R bin lib "$out/libexec/maestro/"
    chmod +x "$out/libexec/maestro/bin/maestro"
    makeWrapper "$out/libexec/maestro/bin/maestro" "$out/bin/maestro" \
      --set JAVA_HOME "${jdk21.home}"
    runHook postInstall
  '';
  meta = {
    description = "Maestro mobile UI test driver";
    license = lib.licenses.asl20;
    platforms = lib.platforms.darwin ++ lib.platforms.linux;
    mainProgram = "maestro";
  };
}
