import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "clean-builds.py"
spec = importlib.util.spec_from_file_location("clean_builds", SCRIPT)
cleanup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cleanup)


class BuildCleanupTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.common = self.root / "git-common"
        self.common.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("CARGO_", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"))}
        self.env["CARGO_HOME"] = str(self.root / "cargo-home")

    def project(self, name):
        root = self.root / name
        (root / "src").mkdir(parents=True)
        profile = tomllib.loads((SCRIPT.parents[1] / "Cargo.toml").read_text())["profile"]["dev"]
        (root / "Cargo.toml").write_text(
            '[package]\nname="cache-check"\nversion="0.1.0"\nedition="2024"\n'
            '[workspace]\n[profile.dev]\ndebug="' + profile["debug"] + '"\n')
        (root / "src/main.rs").write_text(
            'fn main() {\n'
            '    match std::env::args().nth(1).as_deref() {\n'
            '        Some("hold") => { let mut line = String::new(); std::io::stdin().read_line(&mut line).unwrap(); }\n'
            '        Some("panic") => fail(),\n'
            '        _ => println!("cache-ready"),\n'
            '    }\n}\n#[inline(never)]\nfn fail() { panic!("backtrace check"); }\n')
        self.build(root)
        return root

    def build(self, root, target=None):
        output = subprocess.run(["cargo", "build", "--offline", "--target-dir", str(target or root / "target")],
                                cwd=root, env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(output.returncode, 0, output.stderr)

    def executable(self, root):
        return root / "target/debug/cache-check"

    def runs(self, root):
        output = subprocess.run([str(self.executable(root))], capture_output=True, text=True, timeout=5)
        self.assertEqual(output.returncode, 0, output.stderr)
        self.assertEqual(output.stdout, "cache-ready\n")

    def age(self, path, days):
        timestamp = time.time() - days * 86400
        for directory, dirs, files in os.walk(path):
            for name in dirs + files:
                os.utime(Path(directory) / name, (timestamp, timestamp), follow_symlinks=False)
        os.utime(path, (timestamp, timestamp))

    def test_old_outputs_are_removed_then_rebuild_while_records_and_lock_inodes_survive(self):
        root = self.project("old")
        profile = root / "target/debug"
        evidence = root / "target/qa/result.summary.json"
        evidence.parent.mkdir()
        evidence.write_text('{"passedTests":1}')
        backup = root / "target/previous.app"
        backup.mkdir()
        (backup / "host").write_bytes(b"previous host")
        outside = self.root / "user-data"
        outside.mkdir()
        (outside / "draft").write_bytes(b"preserve me")
        (profile / "outside").symlink_to(outside, target_is_directory=True)
        locks = {name: (profile / name).stat().st_ino for name in cleanup.LOCKS}
        self.age(profile, 2)
        self.assertEqual(cleanup.prune([root], self.common)["entries"][0]["action"], "keep")
        self.age(profile, 4)
        report = cleanup.prune([root], self.common, dry_run=True)
        self.assertEqual(report["entries"][0]["action"], "would-clean")
        self.runs(root)
        report = cleanup.prune([root], self.common)
        self.assertEqual(report["entries"][0]["action"], "cleaned")
        self.assertGreater(report["reclaimedBytes"], 0)
        self.assertFalse(self.executable(root).exists())
        self.assertEqual({name: (profile / name).stat().st_ino for name in cleanup.LOCKS}, locks)
        self.assertEqual(evidence.read_text(), '{"passedTests":1}')
        self.assertEqual((backup / "host").read_bytes(), b"previous host")
        self.assertEqual((outside / "draft").read_bytes(), b"preserve me")
        self.build(root)
        self.runs(root)
        output = subprocess.run([str(self.executable(root)), "panic"], env={**self.env, "RUST_BACKTRACE": "1"},
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(output.returncode, 0)
        self.assertRegex(output.stderr, r"at .*src/main.rs:4")

    def test_capacity_evicts_oldest_recent_profile_and_keeps_newer_build(self):
        older = self.project("older")
        newer = self.project("newer")
        self.age(older / "target/debug", 1)
        budget = cleanup.usage(newer / "target/debug")[0]
        report = cleanup.prune([older, newer], self.common, max_bytes=budget)
        self.assertLessEqual(report["afterIdleBytes"], budget)
        self.assertFalse(self.executable(older).exists())
        self.runs(newer)
        self.assertEqual(cleanup.prune([older, newer], self.common, max_bytes=budget)["reclaimedBytes"], 0)

    def test_running_binary_and_active_worktree_are_preserved_until_process_exits(self):
        root = self.project("live")
        self.age(root / "target/debug", 30)
        for args, cwd in [([str(self.executable(root)), "hold"], self.root),
                          ([sys.executable, "-c", "input()"], root)]:
            with self.subTest(args=args):
                process = subprocess.Popen(args, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE)
                try:
                    report = cleanup.prune([root], self.common, max_bytes=0)
                    self.assertEqual(report["entries"][0]["action"], "in-use")
                    self.runs(root)
                finally:
                    process.communicate(b"\n", timeout=5)
        report = cleanup.prune([root], self.common, max_bytes=0)
        self.assertEqual(report["entries"][0]["action"], "cleaned")

    def test_real_cargo_waits_for_cleanup_lock(self):
        root = self.project("locked")
        with cleanup.profile_locks(root / "target/debug"):
            process = subprocess.Popen(["cargo", "build", "--offline", "--target-dir", str(root / "target")],
                                       cwd=root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                time.sleep(0.5)
                self.assertIsNone(process.poll(), "Cargo bypassed the cleanup locks")
            except BaseException:
                process.kill()
                process.communicate(timeout=5)
                raise
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertIn(b"Blocking waiting for file lock", stderr)
        self.runs(root)

    def test_active_cargo_in_shared_target_is_not_cleaned(self):
        root = self.project("shared")
        target = self.common / "bex-quality/cargo-target"
        self.build(root, target)
        (root / "build.rs").write_text(
            'fn main() { std::fs::write("ready", "").unwrap(); '
            'while !std::path::Path::new("release-build").exists() { '
            'std::thread::sleep(std::time::Duration::from_millis(20)); } }')
        process = subprocess.Popen(["cargo", "build", "--offline", "--target-dir", str(target)],
                                   cwd=root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 30
            while not (root / "ready").exists():
                self.assertIsNone(process.poll())
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            report = cleanup.prune([], self.common, max_bytes=0)
            self.assertIn(report["entries"][0]["action"], ("in-use", "locked"))
            self.assertTrue((target / "debug/.fingerprint").is_dir())
        finally:
            (root / "release-build").touch()
            stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(cleanup.prune([], self.common, max_bytes=0)["entries"][0]["action"], "cleaned")

    def test_external_symlink_and_untagged_nested_output_are_not_candidates(self):
        outside = self.project("outside")
        root = self.root / "inside"
        root.mkdir()
        (root / "target").symlink_to(outside / "target", target_is_directory=True)
        self.assertEqual(cleanup.prune([root], self.common, max_bytes=0)["entries"], [])
        (root / "target").unlink()
        (root / "target").mkdir()
        (outside / "target/CACHEDIR.TAG").unlink()
        (outside / "target").rename(root / "target/unknown")
        self.assertEqual(cleanup.prune([root], self.common, max_bytes=0)["entries"], [])
        (root / "target/unknown").rename(outside / "target")
        self.runs(outside)

    def test_untagged_cargo_profile_is_not_cleaned(self):
        root = self.project("untagged")
        (root / "target/CACHEDIR.TAG").unlink()
        self.assertEqual(cleanup.prune([root], self.common, max_bytes=0)["entries"], [])
        self.build(root)
        self.runs(root)

    def test_nested_worktree_activity_does_not_pin_main_builds(self):
        main = self.project("main")
        nested = main / ".git/worktrees/active"
        nested.mkdir(parents=True)
        process = subprocess.Popen([sys.executable, "-c", "input()"], cwd=nested,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            report = cleanup.prune([main, nested], self.common, max_bytes=0)
            self.assertEqual(report["entries"][0]["action"], "cleaned")
        finally:
            process.communicate(b"\n", timeout=5)

    def test_cli_discovers_git_worktrees_and_preserves_source(self):
        root = self.project("source with spaces")
        subprocess.run(["git", "init", "-q", str(root)], check=True, env=self.env, timeout=5)
        source = (root / "src/main.rs").read_bytes()
        self.age(root / "target/debug", 30)
        for flags, action in [(["--dry-run"], "would-clean"), ([], "cleaned")]:
            # The CLI scans open files twice; each scan has its own 60-second limit.
            result = subprocess.run([sys.executable, "-B", str(SCRIPT), *flags], cwd=root, env=self.env,
                                    capture_output=True, text=True, timeout=180)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["entries"][0]["action"], action)
        self.assertFalse(self.executable(root).exists())
        self.assertEqual((root / "src/main.rs").read_bytes(), source)

    def test_inspection_failure_does_not_delete_builds(self):
        root = self.project("inspection-failure")
        bin_dir = self.root / "unavailable-tools"
        bin_dir.mkdir()
        lsof = bin_dir / "lsof"
        lsof.write_text("#!/bin/sh\necho 'inspection unavailable' >&2\nexit 1\n")
        lsof.chmod(0o700)
        subprocess.run(["git", "init", "-q", str(root)], check=True, env=self.env, timeout=5)
        self.age(root / "target/debug", 30)
        result = subprocess.run([sys.executable, "-B", str(SCRIPT)], cwd=root,
                                env={**self.env, "PATH": str(bin_dir) + os.pathsep + self.env["PATH"]},
                                capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.runs(root)


if __name__ == "__main__":
    unittest.main()
