import importlib.util
from pathlib import Path
import plistlib
import signal
import sys
import tempfile
from threading import Event, Thread
import time
import unittest

spec = importlib.util.spec_from_file_location("ios_e2e", Path(__file__).resolve().parents[1] / "ios-e2e.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class IosRunnerTest(unittest.TestCase):
    def test_workers_keep_separate_pairing_and_preserve_build_configuration(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "Bex.xctestrun"
            configuration = {"TestConfigurations": [{"TestTargets": [{
                "TestBundlePath": "__TESTROOT__/BexUITests.xctest",
                "EnvironmentVariables": {"UNCHANGED": "value"},
                "IsUITestBundle": True,
                "OnlyTestIdentifiers": ["BexLaunchUITests"],
            }]}]}
            source.write_bytes(plistlib.dumps(configuration))
            for index in (1, 2):
                destination = root / f"worker-{index}.xctestrun"
                url = f"http://127.0.0.1:{12300 + index}/pairing"
                runner.configure_run(source, destination, url)
                configured = plistlib.loads(destination.read_bytes())
                target = configured["TestConfigurations"][0]["TestTargets"][0]
                self.assertEqual(target["EnvironmentVariables"], {"UNCHANGED": "value", "BEX_PAIRING_URL": url})
                del target["EnvironmentVariables"]["BEX_PAIRING_URL"]
                self.assertEqual(configured, configuration)
            self.assertEqual(plistlib.loads(source.read_bytes()), configuration)
            source.write_bytes(plistlib.dumps({"Metadata": {"FormatVersion": 2}}))
            with self.assertRaises(ValueError):
                runner.configure_run(source, root / "invalid.xctestrun", "http://127.0.0.1/pairing")

    def test_failed_skipped_or_missing_requested_tests_fail_the_run(self):
        runner.check_summary({"passedTests": 2, "failedTests": 0, "skippedTests": 0}, 2)
        for summary in ({"passedTests": 1, "failedTests": 0, "skippedTests": 0},
                        {"passedTests": 2, "failedTests": 1, "skippedTests": 0},
                        {"passedTests": 2, "failedTests": 0, "skippedTests": 1}, {}):
            with self.subTest(summary=summary), self.assertRaises(ValueError):
                runner.check_summary(summary, 2)

    def test_cancellation_stops_provider_descendants(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            # The child represents a provider that outlives its parent unless
            # the runner stops the whole owned process group.
            child = (
                "import pathlib, signal, sys, time; "
                "root=pathlib.Path(sys.argv[1]); "
                "signal.signal(signal.SIGTERM, lambda *_: (root.joinpath('stopped').touch(), sys.exit(0))); "
                "root.joinpath('ready').touch(); time.sleep(60)"
            )
            parent = "import subprocess, sys; subprocess.run([sys.executable, '-c', sys.argv[1], sys.argv[2]])"
            cancel = Event()

            def cancel_when_ready():
                deadline = time.monotonic() + 5
                while not (root / "ready").exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                cancel.set()

            trigger = Thread(target=cancel_when_ready)
            trigger.start()
            try:
                with self.assertRaises(InterruptedError):
                    runner.command([sys.executable, "-c", parent, child, str(root)], cancel, capture=True)
            finally:
                trigger.join()
            self.assertTrue((root / "ready").is_file())
            self.assertTrue((root / "stopped").is_file())

    def test_host_shutdown_leaves_supervisors_alive_to_close_owned_providers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            supervisor = (
                "import pathlib, signal, sys; root=pathlib.Path(sys.argv[1]); "
                "signal.signal(signal.SIGINT, lambda *_: (root.joinpath('interrupted').touch(), sys.exit(1))); "
                "root.joinpath('ready').touch(); sys.stdin.read(); root.joinpath('closed').touch()"
            )
            host = (
                "import signal, subprocess, sys, time\n"
                "child=subprocess.Popen([sys.executable, '-c', sys.argv[1], sys.argv[2]], stdin=subprocess.PIPE)\n"
                "def stop(*_):\n"
                " child.stdin.close(); child.wait(timeout=5); sys.exit(0)\n"
                "signal.signal(signal.SIGINT, stop)\n"
                "time.sleep(60)\n"
            )
            with runner.process([sys.executable, "-c", host, supervisor, str(root)], stop_signal=signal.SIGINT):
                deadline = time.monotonic() + 5
                while not (root / "ready").exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue((root / "ready").is_file())
            self.assertTrue((root / "closed").is_file())
            self.assertFalse((root / "interrupted").exists())


if __name__ == "__main__":
    unittest.main()
