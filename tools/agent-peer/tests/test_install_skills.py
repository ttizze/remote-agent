import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "agent-peer-install-skills"
loader = importlib.machinery.SourceFileLoader("install_skills", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)


class InstallTests(unittest.TestCase):
    def test_custom_provider_homes_and_reinstall(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = dict(os.environ, CODEX_HOME=str(root / "codex"),
                       CLAUDE_CONFIG_DIR=str(root / "claude"))
            for _ in range(2):
                subprocess.run([sys.executable, str(SCRIPT)], env=env,
                               capture_output=True, text=True, check=True)
            for provider in ("codex", "claude"):
                installed = root / provider / "skills" / "agent-peer"
                self.assertEqual(installed.resolve(), SCRIPT.parents[1])
                subprocess.run([sys.executable, str(installed / "scripts" / "agent-peer"),
                                "--help"], capture_output=True, check=True)

    def test_preserves_real_directory_without_partial_install(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            targets = [root / "codex" / "skills" / "agent-peer",
                       root / "claude" / "skills" / "agent-peer"]
            targets[1].mkdir(parents=True)
            original = targets[1] / "SKILL.md"
            original.write_text("user-maintained skill")
            with self.assertRaisesRegex(ValueError, "Refusing"):
                installer.install(SCRIPT.parents[1], targets)
            self.assertFalse(targets[0].exists())
            self.assertEqual(original.read_text(), "user-maintained skill")

    def test_upgrades_links_without_modifying_old_package(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "old"
            old.mkdir()
            (old / "SKILL.md").write_text("old version")
            target = root / "skills" / "agent-peer"
            installer.install(old, [target])
            installer.install(SCRIPT.parents[1], [target])
            self.assertEqual(target.resolve(), SCRIPT.parents[1])
            self.assertEqual((old / "SKILL.md").read_text(), "old version")


if __name__ == "__main__":
    unittest.main()
