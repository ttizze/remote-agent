"""The isolated backend tests must preserve the repository's dependency pins."""
import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "test-terminal.py"
spec = importlib.util.spec_from_file_location("test_terminal", SCRIPT)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class TerminalRunnerTest(unittest.TestCase):
    def setUp(self):
        self.registry = {"name": "dependency", "version": "1.2.3", "source": "registry+https://example.invalid/index", "checksum": "pinned"}
        self.git = {"name": "other", "version": "0.1.0", "source": "git+https://example.invalid/other#revision"}
        self.original = {"package": [self.registry, self.git, {"name": "alacritty_terminal", "version": "0.26.0"}]}

    def test_pruning_keeps_registry_and_git_identity_and_checksum(self):
        self.assertEqual(runner.verify_lock_subset(self.original, {"package": [self.registry]}), 1)
        self.assertEqual(runner.verify_lock_subset(self.original, {"package": [self.git]}), 1)
        self.assertEqual(runner.verify_lock_subset(self.original, self.original), 2)

    def test_new_versions_sources_and_checksums_are_rejected(self):
        for change in ({"name": "extra"}, {"version": "1.2.4"},
                       {"source": "registry+https://elsewhere.invalid/index"},
                       {"checksum": "changed"}, {"checksum": None}):
            with self.subTest(change=change):
                with self.assertRaisesRegex(ValueError, "changed locked dependencies"):
                    runner.verify_lock_subset(self.original, {"package": [self.registry | change]})

    def test_changed_git_revision_is_rejected(self):
        changed = self.git | {"source": "git+https://example.invalid/other#different"}
        with self.assertRaisesRegex(ValueError, "changed locked dependencies"):
            runner.verify_lock_subset(self.original, {"package": [changed]})


if __name__ == "__main__":
    unittest.main()
