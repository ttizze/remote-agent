{ lib, stdenvNoCC, python3 }:

stdenvNoCC.mkDerivation {
  pname = "agent-peer";
  version = "0.1.0";
  src = ./.;
  nativeBuildInputs = [ python3 ];
  dontBuild = true;
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -v
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin" "$out/share/agent-peer"
    cp SKILL.md "$out/share/agent-peer/"
    cp -R scripts "$out/share/agent-peer/"
    patchShebangs "$out/share/agent-peer/scripts"
    for command in agent-peer agent-peer-install-skills; do
      ln -s "../share/agent-peer/scripts/$command" "$out/bin/$command"
    done
    runHook postInstall
  '';
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    PATH=/nonexistent "$out/bin/agent-peer" --help >/dev/null
    skill_homes=$(mktemp -d)
    PATH=/nonexistent "$out/bin/agent-peer-install-skills" \
      --codex-home "$skill_homes/codex" --claude-home "$skill_homes/claude"
    for provider in codex claude; do
      test -f "$skill_homes/$provider/skills/agent-peer/SKILL.md"
      PATH=/nonexistent "$skill_homes/$provider/skills/agent-peer/scripts/agent-peer" --help >/dev/null
    done
    runHook postInstallCheck
  '';
  meta = {
    description = "Ask Claude Code or Codex and continue their native conversations";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
    mainProgram = "agent-peer";
  };
}
