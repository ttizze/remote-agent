{ lib, stdenvNoCC, fetchurl, unzip, makeWrapper, jdk21 }:

stdenvNoCC.mkDerivation {
  pname = "maestro";
  version = "2.11.0";
  src = fetchurl {
    url = "https://github.com/mobile-dev-inc/Maestro/releases/download/cli-2.11.0/maestro.zip";
    hash = "sha256-U4RZPLTnoQZInnWoIdFX3UP05Djfa8MIty6CxoXhKDo=";
  };
  nativeBuildInputs = [ unzip makeWrapper ];
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
