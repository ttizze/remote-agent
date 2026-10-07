use super::*;

#[test]
fn unwraps_shell_scripts_without_executing_them() {
    for (command, expected) in [
        (
            "/bin/zsh -lc 'vp test run apps/web/src/session-logic.test.ts'",
            Some("vp"),
        ),
        ("/bin/zsh -lc 'git diff --check'", Some("git")),
        (
            "/bin/zsh -lc \"npx -y react-doctor@latest apps/web\"",
            Some("npx"),
        ),
        (
            "/bin/zsh -lc 'rg -n \"registerHooks|worker\" apps/web/src'",
            Some("rg"),
        ),
        (
            "/bin/bash --noprofile --norc -l -c 'sed -n 1,270p file.ts'",
            Some("sed"),
        ),
        ("/bin/bash -o pipefail -lc 'vp test run'", Some("vp")),
        (
            "/bin/bash --rcfile /tmp/config -c 'git status'",
            Some("git"),
        ),
        ("sh -ec 'node scripts/check.js'", Some("node")),
        ("fish --command 'rg --files'", Some("rg")),
        (
            "zsh -lc 'CI=1 env -u DEBUG sudo -u root vp test run'",
            Some("vp"),
        ),
        (
            "env CI=1 /bin/zsh -lc '\"/Applications/My Tools/bin/check\" --verbose'",
            Some("check"),
        ),
        ("bash -lc \"zsh -c 'git status'\"", Some("git")),
        (
            "\"C:\\Program Files\\Git\\bin\\bash.exe\" -lc \"git status\"",
            Some("git"),
        ),
        (
            "/bin/zsh -lc 'git status\nsed -n '\"'1,20p' apps/web/src/components/DiffPanel.tsx\"",
            Some("git"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn preserves_ordinary_programs_and_actual_shell_launches() {
    for (command, expected) in [
        ("vp test run", Some("vp")),
        ("sudo -u root pnpm test", Some("pnpm")),
        (
            "env --split-string='CI=1 node scripts/check.js'",
            Some("node"),
        ),
        (
            "\"C:\\Program Files\\nodejs\\node.exe\" script.js",
            Some("node.exe"),
        ),
        ("/bin/zsh", Some("zsh")),
        ("/bin/bash -l", Some("bash")),
        ("zsh script.sh -c 'git status'", Some("zsh")),
        ("bash -- -c 'git status'", Some("bash")),
        ("bash --rcfile config.sh", Some("bash")),
        ("my-shell -c 'git status'", Some("my-shell")),
        ("$HOME/.bun/bin/bun test", Some("bun")),
        (
            "\"$ANDROID_HOME/emulator/emulator\" -list-avds",
            Some("emulator"),
        ),
        ("${ROOT}/bin/tool --version", Some("tool")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn falls_back_for_missing_or_malformed_scripts() {
    for (command, expected) in [
        ("", None),
        ("zsh -lc", None),
        ("zsh -lc ''", None),
        ("zsh -lc 'git status", None),
        ("zsh -lc 'env'", None),
        ("zsh -lc \"git \\\"", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn falls_back_for_shell_syntax_and_internal_control_commands() {
    for (command, expected) in [
        ("if test -f package.json; then vp test; fi", None),
        ("[ -f package.json ]", None),
        ("[[ -f package.json ]]", None),
        ("test -f package.json", None),
        ("for file in *; do echo \"$file\"; done", None),
        ("while true; do sleep 1; done", None),
        ("until false; do sleep 1; done", None),
        ("case $name in test) vp test;; esac", None),
        ("select item in one two; do echo \"$item\"; done", None),
        ("function check() { vp test; }", None),
        ("check() { vp test; }", None),
        ("k(){ echo ok; }; k", None),
        ("{ vp test; }", None),
        ("(vp test)", None),
        ("(( count += 1 ))", None),
        ("! vp test", None),
        (":", None),
        (". ./script.sh", None),
        ("source ./script.sh", None),
        ("eval 'vp test'", None),
        ("cd packages/client-runtime", None),
        ("export NODE_ENV=test", None),
        ("local name=value", None),
        ("set -e", None),
        ("alias ll='ls -la'", None),
        ("repeat 3 echo ok", None),
        ("and vp test", None),
        ("return 1", None),
        ("break", None),
        ("continue", None),
        ("true", None),
        ("false", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn uses_the_first_executable_looking_program() {
    for (command, expected) in [
        ("rg -n \"if|for|while\" src", Some("rg")),
        ("printf '%s\\n' 'a;b|c'", Some("printf")),
        ("node -e \"if (true) console.log('ok')\"", Some("node")),
        ("echo '$(git status)'", Some("echo")),
        ("vp test && git status", Some("vp")),
        ("vp test || git status", Some("vp")),
        ("rg needle src | head", Some("rg")),
        ("vp test; git status", Some("vp")),
        ("vp test &", Some("vp")),
        ("vp test\ngit status", Some("vp")),
        ("echo \"$(git status)\"", Some("echo")),
        ("echo `git status`", Some("echo")),
        ("cat <(rg needle src)", Some("cat")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_leading_cd_commands_and_uses_the_next_useful_program() {
    for (command, expected) in [
        ("cd packages/client-runtime && vp test run", Some("vp")),
        ("cd \"a path with spaces\"; git status", Some("git")),
        ("cd apps/web\npnpm test", Some("pnpm")),
        ("cd first && cd second && bun test", Some("bun")),
        ("CI=1 cd apps/web && npm test", Some("npm")),
        ("PATH+=:/tools npm test", Some("npm")),
        ("PATH+=:/tools && npm test", Some("npm")),
        (
            "TMP=$(mktemp -d); cd \"$TMP\"; npm pack ./package",
            Some("npm"),
        ),
        ("cd $(find . -type d | head -1) && git status", Some("git")),
        (
            "cd `find . -type d | head -1` && node script.js",
            Some("node"),
        ),
        ("cd /tmp 2>&1 && npm test", Some("npm")),
        ("cd /tmp 2<&0 && pnpm test", Some("pnpm")),
        ("cd /tmp &>/dev/null && bun test", Some("bun")),
        ("cd work |& npm test", Some("npm")),
        (
            "cd /tmp && # use the selected workspace\nnpm test",
            Some("npm"),
        ),
        (
            "export CI=1; # first note\n# second note\npnpm test",
            Some("pnpm"),
        ),
        ("cd&&npm test", Some("npm")),
        ("export CI=1;pnpm test", Some("pnpm")),
        ("cd ${ROOT:-path;with;semicolons} && bun test", Some("bun")),
        ("cd ${ROOT:-path&&fallback} && node app.js", Some("node")),
        ("cd @(first|second) && npm test", Some("npm")),
        ("cd /tmp \\\n&& npm test", Some("npm")),
        ("/bin/zsh -lc 'cd apps/web && vp test run'", Some("vp")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_shell_setup_commands_and_uses_the_next_useful_program() {
    for (command, expected) in [
        ("source ~/.nvm/nvm.sh && nvm use", Some("nvm")),
        (". ./.env && pnpm test", Some("pnpm")),
        ("export CI=1 && vp test run", Some("vp")),
        ("unset DEBUG; node app.js", Some("node")),
        ("export CI=1 && cd apps/web && pnpm test", Some("pnpm")),
        (
            "/bin/zsh -lc 'source ~/.nvm/nvm.sh && nvm use'",
            Some("nvm"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_non_descriptive_shell_commands_before_a_useful_program() {
    for (command, expected) in [
        ("set -eu; npm test", Some("npm")),
        (": && npm test", Some("npm")),
        ("true && npm test", Some("npm")),
        ("false || npm test", Some("npm")),
        ("false; npm test", Some("npm")),
        ("sudo -n true && npm test", Some("npm")),
        ("sudo -n true; echo checked", Some("echo")),
        ("test -d node_modules || vp i", Some("vp")),
        ("[ -d node_modules ] || vp i", Some("vp")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn handles_shell_setup_followed_by_every_command_separator() {
    for (command, expected) in [
        ("cd /tmp&&npm test", Some("npm")),
        ("cd /tmp || npm test", Some("npm")),
        ("cd /tmp;npm test", Some("npm")),
        ("cd /tmp\nnpm test", Some("npm")),
        ("cd /tmp|npm test", Some("npm")),
        ("cd /tmp |& npm test", Some("npm")),
        ("cd /tmp & npm test", Some("npm")),
        ("export CI=1&&npm test", Some("npm")),
        ("export CI=1 || npm test", Some("npm")),
        ("export CI=1;npm test", Some("npm")),
        ("export CI=1\nnpm test", Some("npm")),
        ("export CI=1|npm test", Some("npm")),
        ("export CI=1 |& npm test", Some("npm")),
        ("export CI=1 & npm test", Some("npm")),
        ("unset DEBUG&&npm test", Some("npm")),
        ("unset DEBUG || npm test", Some("npm")),
        ("unset DEBUG;npm test", Some("npm")),
        ("unset DEBUG\nnpm test", Some("npm")),
        ("unset DEBUG|npm test", Some("npm")),
        ("unset DEBUG |& npm test", Some("npm")),
        ("unset DEBUG & npm test", Some("npm")),
        ("source env.sh&&npm test", Some("npm")),
        ("source env.sh || npm test", Some("npm")),
        ("source env.sh;npm test", Some("npm")),
        ("source env.sh\nnpm test", Some("npm")),
        ("source env.sh|npm test", Some("npm")),
        ("source env.sh |& npm test", Some("npm")),
        ("source env.sh & npm test", Some("npm")),
        (". env.sh&&npm test", Some("npm")),
        (". env.sh || npm test", Some("npm")),
        (". env.sh;npm test", Some("npm")),
        (". env.sh\nnpm test", Some("npm")),
        (". env.sh|npm test", Some("npm")),
        (". env.sh |& npm test", Some("npm")),
        (". env.sh & npm test", Some("npm")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn unwraps_shell_command_wrappers() {
    for (command, expected) in [
        ("command git status", Some("git")),
        ("command -p git status", Some("git")),
        ("command -- git status", Some("git")),
        ("builtin printf ok", Some("printf")),
        ("builtin -- echo ok", Some("echo")),
        ("command cd /tmp && npm test", Some("npm")),
        ("builtin cd /tmp && pnpm test", Some("pnpm")),
        ("exec node app.js", Some("node")),
        ("exec -cl -a worker node app.js", Some("node")),
        ("exec env CI=1 /opt/tools/check --verbose", Some("check")),
        (
            "exec \"C:\\Program Files\\nodejs\\node.exe\" app.js",
            Some("node.exe"),
        ),
        ("exec sh -c 'cd /tmp && npm test'", Some("npm")),
        ("exec sh -c 'cd /tmp\nnpm test'", Some("npm")),
        ("exec bash -c 'set -e\nnpm test'", Some("npm")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn unwraps_process_launch_wrappers() {
    for (command, expected) in [
        ("timeout 10 pnpm test", Some("pnpm")),
        ("timeout 10s python3 script.py", Some("python3")),
        ("gtimeout 1.5 node app.js", Some("node")),
        ("nohup npx expo start >/tmp/metro.log 2>&1 &", Some("npx")),
        ("nohup -- env CI=1 bun test", Some("bun")),
        (
            "arch -x86_64 ./build/app-under-test",
            Some("app-under-test"),
        ),
        ("arch -arch arm64 /opt/tools/check", Some("check")),
        ("bundle exec pod install", Some("pod")),
        ("timeout 30 nohup env CI=1 node app.js", Some("node")),
        (
            "timeout 60 script -q /dev/null env CI=1 node app.js",
            Some("node"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn resolves_literal_powershell_call_operators() {
    for (command, expected) in [
        (
            "& 'C:\\Program Files\\nodejs\\node.exe' script.js",
            Some("node.exe"),
        ),
        (
            "& \"$env:WINDIR\\Microsoft.NET\\Framework64\\v4\\csc.exe\" file.cs",
            Some("csc.exe"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn labels_commands_inside_simple_powershell_assignments() {
    for (command, expected) in [
        ("$env:CI='1'; npm test", Some("npm")),
        ("$value = 'configured'; node app.js", Some("node")),
        (
            "$process = Get-Process node; $process.Id",
            Some("Get-Process"),
        ),
        (
            "$tmp = Join-Path $env:TEMP repo; git clone example",
            Some("Join-Path"),
        ),
        (
            "$html = (Invoke-WebRequest https://example.com).Content",
            Some("Invoke-WebRequest"),
        ),
        (
            "$process = Start-Process -FilePath .\\app.exe -PassThru; $process.Id",
            Some("app.exe"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn unwraps_windows_shell_launchers() {
    for (command, expected) in [
        ("cmd /c bcdedit /enum", Some("bcdedit")),
        ("cmd /c cd C:\\work && npm test", Some("npm")),
        ("cmd.exe /d /s /c \"cd C:\\work && npm test\"", Some("npm")),
        (
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"C:\\work\\scripts\\doctor.ps1\"",
            Some("doctor.ps1"),
        ),
        (
            "pwsh -NoProfile -Command \"Set-Location C:\\work; pnpm test\"",
            Some("pnpm"),
        ),
        (
            "pwsh -Command Set-Location C:\\work; pnpm test",
            Some("pnpm"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn handles_common_powershell_setup_and_launch_commands() {
    for (command, expected) in [
        ("Set-Location C:\\work; npm test", Some("npm")),
        ("Push-Location C:\\work; node app.js", Some("node")),
        (
            "Start-Process -FilePath node -ArgumentList server.js",
            Some("node"),
        ),
        (
            "Start-Process -ArgumentList '-FilePath helper' node",
            Some("node"),
        ),
        (
            "Start-Process -ErrorAction Stop -FilePath node",
            Some("node"),
        ),
        (
            "Start-Process -NoNewWindow -WorkingDirectory C:\\work node",
            Some("node"),
        ),
        (
            "Start-Process \"C:\\Program Files\\Example\\app.exe\"",
            Some("app.exe"),
        ),
        (
            "Start-Process -FilePath \".\\dist\\Example App.exe\" -PassThru",
            Some("Example App.exe"),
        ),
        (
            ".\\.venv\\Scripts\\python.exe script.py",
            Some("python.exe"),
        ),
        (
            "$env:LOCALAPPDATA\\Programs\\tool.exe --version",
            Some("tool.exe"),
        ),
        (
            "\"=== CHECK FILE ===\"; Get-Content file.txt",
            Some("Get-Content"),
        ),
        (
            "$x = @'\ndata'; Get-Fake\n'@\nGet-Process",
            Some("Get-Process"),
        ),
        ("@'\nprint('ok; still data')\n'@ | python -", Some("python")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn keeps_process_launch_wrappers_when_no_safe_payload_is_present() {
    for (command, expected) in [
        ("timeout --help", Some("timeout")),
        ("nohup --version", Some("nohup")),
        ("arch", Some("arch")),
        ("bundle install", Some("bundle")),
        ("script output.log", Some("script")),
        ("/usr/bin/timeout 10 node app.js", Some("timeout")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_treat_shell_lookup_and_commandless_wrapper_forms_as_executions() {
    for (command, expected) in [
        ("command -v git", None),
        ("command -V git", None),
        ("command -a git", None),
        ("command -pv git", None),
        ("builtin -p", None),
        ("exec", None),
        ("exec > output.log", None),
        ("exec --", None),
        ("exec cd /tmp && npm test", None),
        ("exec env CI=1 cd /tmp && npm test", None),
        ("exec CI=1 npm test", None),
        ("env CI=1 cd /tmp && npm test", None),
        ("env CI=1; npm test", None),
        ("sudo cd /tmp && npm test", None),
        ("command CI=1 npm test", None),
        ("command false && npm test", None),
        ("cmd /c false && npm test", None),
        ("cd /tmp <<EOF\nunterminated heredoc", None),
        ("cd /tmp && >/tmp/log", None),
        (
            "(xcrun simctl io booted recordVideo /tmp/video.mp4 &) ; wait",
            None,
        ),
        (
            "export CI=1 && (bundle exec pod install || pod install)",
            None,
        ),
        ("$PY scripts/check.py", None),
        ("${TOOL} --version", None),
        ("%TOOL% --version", None),
        ("!TOOL! --version", None),
        ("& $tool --version", None),
        ("& { Get-Process }", None),
        ("$value = 'configured'", None),
        (
            "$headers = @{ 'Accept' = 'application/json'; 'Content-Type' = 'application/json' }; Invoke-WebRequest https://example.com",
            None,
        ),
        ("\"sha256(value)=$hash\"", None),
        ("broken{", None),
        ("@echo off", None),
        (":: comment", None),
        ("time -- npm test", None),
        ("time -v npm test", None),
        ("coproc npm test", None),
        ("coproc worker { npm test; }", None),
        ("cd [first|second] && pnpm test", None),
        ("npm) --version", None),
        (
            "try { Invoke-WebRequest https://example.com } catch { Write-Error $_ }",
            None,
        ),
        ("for($i=0; $i -lt 2; $i++){ Start-Sleep 1 }", None),
        ("# comment only", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_hide_legitimate_or_case_distinct_program_names() {
    for (command, expected) in [
        ("parallel -j4", Some("parallel")),
        ("hash --help", Some("hash")),
        ("process --help", Some("process")),
        ("rem comment", Some("rem")),
        ("Exec node app.js", Some("Exec")),
        ("CD /tmp && npm test", Some("CD")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn parses_shell_setup_inside_an_explicitly_launched_shell() {
    for (command, expected) in [
        ("env sh -c 'cd /tmp && npm test'", Some("npm")),
        ("sudo zsh -lc 'export CI=1 && pnpm test'", Some("pnpm")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_shell_precommand_modifiers() {
    for (command, expected) in [
        ("nocorrect pnpm test", Some("pnpm")),
        ("noglob bun test", Some("bun")),
        ("time node app.js", Some("node")),
        ("time -p deno test", Some("deno")),
        ("time nocorrect npm test", Some("npm")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn uses_the_command_after_leading_shell_comments() {
    assert_eq!(
        command_program_name("# first comment\n  # second comment\ngit status").as_deref(),
        Some("git")
    );
}

#[test]
fn skips_comments_after_commandless_shell_setup() {
    for (command, expected) in [
        ("CI=1 # note\nnpm test", Some("npm")),
        ("CI=1 # it's configured\nnpm test", Some("npm")),
        ("CI=1 # \"unterminated quote\nbun test", Some("bun")),
        (">/tmp/log # note\npnpm test", Some("pnpm")),
        (">/tmp/log # it's configured\ndeno test", Some("deno")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_treat_qualified_paths_as_shell_syntax() {
    for (command, expected) in [
        ("./cd /tmp && npm test", Some("cd")),
        ("/opt/exec node app.js", Some("exec")),
        ("/usr/bin/time npm test", Some("time")),
        ("/usr/bin/test -f package.json", Some("test")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_a_shell_array_assignment_before_the_command() {
    assert_eq!(
        command_program_name("items=(one two); npm test").as_deref(),
        Some("npm")
    );
}

#[test]
fn resolves_literal_command_aliases_from_earlier_shell_segments() {
    for (command, expected) in [
        (
            "EMU=\"/opt/android/emulator\"; \"$EMU\" -list-avds",
            Some("emulator"),
        ),
        (
            "AAPT=/opt/android/aapt2\n$AAPT dump badging app.apk",
            Some("aapt2"),
        ),
        (
            "SSH=(ssh -i /tmp/key -o IdentitiesOnly=yes); HOST=user@example; \"${SSH[@]}\" \"$HOST\" uptime",
            Some("ssh"),
        ),
        (
            "SCP=(/usr/bin/scp -i /tmp/key); if true; then ${SCP[@]} file user@example:/tmp; fi",
            Some("scp"),
        ),
        (
            "TOOL=\"/Applications/My Tool/bin/check\"; \"$TOOL\" --verbose",
            Some("check"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_evaluate_dynamic_or_non_persistent_command_aliases() {
    for (command, expected) in [
        ("TOOL=$(pick-command); \"$TOOL\" --version", None),
        ("TOOL=\"git status\"; \"$TOOL\"", None),
        ("# TOOL=git\n$TOOL status", None),
        ("TOOL=git true; \"$TOOL\" status", None),
        ("TOOL=git; TOOL=$(pick-command); \"$TOOL\" status", None),
        ("TOOL=git; unset TOOL; \"$TOOL\" status", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_retain_aliases_assigned_inside_control_flow() {
    assert_eq!(
        command_program_name("TOOL=git; if false; then\nTOOL=npm\nfi\n\"$TOOL\" status").as_deref(),
        Some("git")
    );
}

#[test]
fn keeps_expansions_inside_assignment_words() {
    for (command, expected) in [
        ("ROOT=${BASE:-path with spaces}; npm test", Some("npm")),
        ("ROOT=`printf 'path with spaces'`; pnpm test", Some("pnpm")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_redirections_before_the_next_command() {
    for (command, expected) in [
        (">/tmp/log && npm test", Some("npm")),
        ("2>/tmp/error.log; pnpm test", Some("pnpm")),
        ("cd /tmp && >/tmp/log npm test", Some("npm")),
        ("cd /tmp && > /tmp/log pnpm test", Some("pnpm")),
        ("cd /tmp && 2>&1 bun test", Some("bun")),
        ("cd /tmp && 2>& 1 node app.js", Some("node")),
        ("cd /tmp && &>/tmp/log git status", Some("git")),
        ("cd /tmp && *>>/tmp/log vp test", Some("vp")),
        ("cd /tmp && {output}>/tmp/log deno test", Some("deno")),
        ("cd /tmp && <<<input ruby script.rb", Some("ruby")),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn skips_heredoc_bodies_before_finding_the_next_command() {
    for (command, expected) in [
        ("cd /tmp <<EOF\nnot-a-command\nEOF\nnpm test", Some("npm")),
        (
            "cd /tmp <<'EOF'\nnot-a-command\nEOF\npnpm test",
            Some("pnpm"),
        ),
        (
            "cd /tmp <<'EOF'\nnot-a-command\\\nEOF\npnpm test",
            Some("pnpm"),
        ),
        (
            "cd /tmp <<-EOF\n\tnot-a-command\n\tEOF\nbun test",
            Some("bun"),
        ),
        ("cd /tmp <<A <<B\none\nA\ntwo\nB\ngit status", Some("git")),
        (
            "cd /tmp <<EOF; # setup\nnot-a-command\nEOF\nnpm test",
            Some("npm"),
        ),
        (
            "cd /tmp <<EOF && pnpm test\nnot-a-command\nEOF\nbun test",
            Some("pnpm"),
        ),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn falls_back_when_no_useful_program_follows_cd() {
    for (command, expected) in [
        ("cd apps/web && [ -f package.json ]", None),
        ("cd apps/web || exit 1", None),
        ("cd one && cd two", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn does_not_label_commands_inside_multiline_shell_control_flow() {
    for (command, expected) in [
        ("if test -f package.json\nthen\n  npm test\nfi", None),
        ("[[ -d first && -d second ]] && npm test", None),
        ("for file in *\ndo\n  echo \"$file\"\ndone", None),
        ("while true\ndo\n  sleep 1\ndone", None),
        ("cd /tmp; build () { npm test; }", None),
    ] {
        assert_eq!(
            command_program_name(command).as_deref(),
            expected,
            "{command:?}"
        );
    }
}

#[test]
fn bounds_nested_shell_unwrapping() {
    let mut command = "git status".to_owned();
    for _ in 0..9 {
        command = format!("sh -c '{}'", command.replace('\'', r"'\''"));
    }
    assert_eq!(command_program_name(&command), None);
}

#[test]
fn does_not_spend_the_shell_nesting_budget_on_setup_commands() {
    let command = (0..10)
        .map(|index| format!("export VALUE_{index}=configured"))
        .chain(["npm test".to_owned()])
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(command_program_name(&command).as_deref(), Some("npm"));
}

#[test]
fn bounds_the_number_of_top_level_setup_segments() {
    let command = (0..2_000)
        .map(|index| format!("export VALUE_{index}=configured"))
        .chain(["npm test".to_owned()])
        .collect::<Vec<_>>()
        .join(";");
    assert_eq!(command_program_name(&command), None);
}

#[test]
fn bounds_nested_command_wrappers() {
    assert_eq!(
        command_program_name(&format!("{}git status", "command ".repeat(9))),
        None
    );
}

#[test]
fn shows_the_script_without_hiding_meaningful_outer_commands() {
    for (command, expected) in [
        ("/bin/zsh -lc 'git diff --stat'", "git diff --stat"),
        (
            "bash -c \"printf \\\"%s\\\" hello; git status\"",
            "printf \"%s\" hello; git status",
        ),
        (
            "/usr/bin/fish --command 'rg TODO src | head -20'",
            "rg TODO src | head -20",
        ),
        ("zsh -lc 'pwd\nls'", "pwd\nls"),
        ("git status", "git status"),
        ("zsh script.sh", "zsh script.sh"),
        (
            "zsh -lc 'echo $1' name value",
            "zsh -lc 'echo $1' name value",
        ),
        ("zsh -lc 'pwd'; git status", "zsh -lc 'pwd'; git status"),
        ("zsh -lc 'unterminated", "zsh -lc 'unterminated"),
    ] {
        assert_eq!(command_display_text(command), expected, "{command:?}");
    }
}
