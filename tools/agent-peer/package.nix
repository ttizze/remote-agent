{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "agent-peer";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ./tests ./SKILL.md ./LICENSE ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  postInstall = ''
    mkdir -p "$out/share/agent-peer/scripts"
    cp SKILL.md "$out/share/agent-peer/"
    ln -s ../../../bin/agent-peer "$out/share/agent-peer/scripts/agent-peer"
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
