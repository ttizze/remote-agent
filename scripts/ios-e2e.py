#!/usr/bin/env python3
"""Build once, then run iOS tests on isolated Simulator/Host pairs."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import tempfile
from threading import Event
import time


@contextmanager
def process(args, stdout=None, stop_signal=signal.SIGTERM):
    child = subprocess.Popen([str(arg) for arg in args], stdout=stdout,
                             stderr=None if stdout == subprocess.PIPE else subprocess.STDOUT,
                             start_new_session=True)
    try:
        yield child
    finally:
        # Include native provider descendants, even if their parent has exited.
        try:
            os.killpg(child.pid, stop_signal)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
        if child.stdout:
            child.stdout.close()


def command(args, cancel, stdout=None, capture=False):
    if cancel.is_set():
        raise InterruptedError("iOS test run cancelled")
    with process(args, subprocess.PIPE if capture else stdout) as child:
        while True:
            try:
                output, _ = child.communicate(timeout=0.2)
                break
            except subprocess.TimeoutExpired:
                if cancel.is_set():
                    raise InterruptedError("iOS test run cancelled")
        if child.returncode:
            raise subprocess.CalledProcessError(child.returncode, args, output)
        return output.decode().strip() if capture else None


def configure_run(source, destination, pairing_url):
    with source.open("rb") as file:
        configuration = plistlib.load(file)

    def configure(value):
        if isinstance(value, dict):
            if "TestBundlePath" in value:
                value.setdefault("EnvironmentVariables", {})["BEX_PAIRING_URL"] = pairing_url
                return 1
            return sum(configure(child) for child in value.values())
        if isinstance(value, list):
            return sum(configure(child) for child in value)
        return 0

    if not configure(configuration):
        raise ValueError("xctestrun contains no test bundle")
    with destination.open("wb") as file:
        plistlib.dump(configuration, file)


def check_summary(summary, expected):
    if (summary.get("passedTests") != expected or summary.get("failedTests") != 0 or
            summary.get("skippedTests") != 0):
        raise ValueError("iOS tests did not pass completely")


def worker(index, tests, target, source, runtime, video, records, without_codex, cancel):
    started = time.monotonic()
    bundle = records / f"worker-{index}.xcresult"
    name = f"Bex isolated E2E {records.name}-{index}"
    products = source.parent
    run = products / f"worker-{index}.xctestrun"
    with tempfile.TemporaryDirectory(prefix="bex-ios-") as temporary, \
            (records / f"worker-{index}.log").open("w") as log:
        root = Path(temporary)
        codex = "--without-codex" if without_codex else target / "debug/bex-codex-fixture"
        with (records / f"host-{index}.log").open("w") as host_log, \
                process([target / "debug/bex-ui-fixture", root / "host", codex,
                         root / "pairing.port", "8000", target / "debug/bex-claude-fixture"],
                        host_log, signal.SIGINT) as host:
            try:
                deadline = time.monotonic() + 30
                while not (root / "pairing.port").is_file():
                    if host.poll() is not None:
                        raise RuntimeError(f"worker {index}: UI fixture exited before publishing its port")
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f"worker {index}: UI fixture timed out")
                    if cancel.wait(0.1):
                        raise InterruptedError("iOS test run cancelled")
                simulator = command(["xcrun", "simctl", "create", name,
                                     "com.apple.CoreSimulator.SimDeviceType.iPhone-17", runtime],
                                    cancel, capture=True)
                command(["xcrun", "simctl", "boot", simulator], cancel, log)
                command(["xcrun", "simctl", "bootstatus", simulator, "-b"], cancel, log)
                command(["xcrun", "simctl", "addmedia", simulator,
                         "apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png", video],
                        cancel, log)
                command(["xcrun", "simctl", "install", simulator,
                         products / "Debug-iphonesimulator/Bex.app"], cancel, log)
                command(["xcrun", "simctl", "privacy", simulator, "grant", "photos-add",
                         "com.ttizze.b-codex"], cancel, log)
                container = Path(command(["xcrun", "simctl", "get_app_container", simulator,
                                          "com.ttizze.b-codex", "data"], cancel, capture=True))
                (container / "Documents").mkdir(exist_ok=True)
                (container / "Documents/attachment-fixture.txt").write_text(
                    "Isolated attachment upload fixture.\n")
                # Keep __TESTROOT__ relative to the shared, locked build products.
                configure_run(source, run,
                              f"http://127.0.0.1:{(root / 'pairing.port').read_text().strip()}/pairing")
                selection = [f"-only-testing:BexUITests/BexLaunchUITests/{test}" for test in tests]
                status = None
                try:
                    command(["xcodebuild", "-xctestrun", run, "-destination",
                             f"platform=iOS Simulator,id={simulator}", "-parallel-testing-enabled", "NO",
                             "-resultBundlePath", bundle, *selection, "test-without-building"], cancel, log)
                except subprocess.CalledProcessError as error:
                    status = error
                summary = json.loads(command(["xcrun", "xcresulttool", "get", "test-results", "summary",
                                              "--path", bundle, "--format", "json"], cancel, capture=True))
                (records / f"worker-{index}.summary.json").write_text(json.dumps(summary, indent=2) + "\n")
                check_summary(summary, len(tests))
                if status:
                    raise status
                result = {"tests": tests, "seconds": round(time.monotonic() - started, 2),
                          "bundle": str(bundle)}
                print(f"worker {index}: {len(tests)} passed; records: {bundle}", flush=True)
                return result
            finally:
                run.unlink(missing_ok=True)
                # A cancelled create may register the device before returning its
                # ID. Recover only this run's unique name, including that case.
                devices = json.loads(subprocess.check_output(
                    ["xcrun", "simctl", "list", "devices", "-j"], timeout=60, stderr=log))["devices"]
                for device in (device for group in devices.values() for device in group if device["name"] == name):
                    subprocess.run(["xcrun", "simctl", "shutdown", device["udid"]], timeout=60,
                                   stdout=log, stderr=subprocess.STDOUT)
                    subprocess.run(["xcrun", "simctl", "delete", device["udid"]], timeout=60,
                                   check=True, stdout=log, stderr=subprocess.STDOUT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--without-codex", action="store_true")
    parser.add_argument("tests", nargs="+")
    args = parser.parse_args()
    if len(set(args.tests)) != len(args.tests) or any(not test.startswith("testSimulator") for test in args.tests):
        parser.error("Select unique isolated Simulator tests")
    workers = int(os.environ.get("BEX_IOS_TEST_WORKERS", "2"))
    if not 1 <= workers <= 4:
        parser.error("BEX_IOS_TEST_WORKERS must be between 1 and 4")
    workers = min(workers, len(args.tests))
    cancel = Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: cancel.set())
    started = time.monotonic()
    target = Path(json.loads(command(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                                     cancel, capture=True))["target_directory"])
    qa = target / "qa"
    qa.mkdir(exist_ok=True)
    records = qa / f"Bex-{time.time_ns()}-{os.getpid()}"
    records.mkdir()
    build = qa / "ios-derived-data"
    # Hold the cache lock through tests: another worktree must not replace the
    # native binaries, app or xctestrun while an isolated Host is using them.
    with (qa / "ios-e2e.lock").open("a") as lock:
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if cancel.wait(0.2):
                    raise InterruptedError("iOS test run cancelled")
        with (records / "build.log").open("w") as log:
            build_started = time.monotonic()
            command(["scripts/build-agent-ios.sh", "simulator"], cancel, log)
            command(["cargo", "build", "--locked", "--package", "bex-process", "--bin", "bex-provider-supervisor",
                     "--package", "host-fixture", "--bin", "bex-ui-fixture", "--bin", "bex-codex-fixture",
                     "--bin", "bex-claude-fixture"], cancel, log)
            for stale in (build / "Build/Products").glob("*.xctestrun"):
                stale.unlink()
            command(["xcodebuild", "-skipPackagePluginValidation", "-project", "apps/mobile/iosApp/Bex.xcodeproj",
                     "-scheme", "Bex", "-destination", "generic/platform=iOS Simulator", "-configuration", "Debug",
                     "-derivedDataPath", build, "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_IDENTITY=-",
                     "CODE_SIGNING_REQUIRED=YES", f"BEX_CARGO_TARGET_DIR={target}", "build-for-testing"], cancel, log)
            build_seconds = round(time.monotonic() - build_started, 2)
            command(["xcrun", "swift", "apps/mobile/iosApp/BexUITests/Fixtures/create-video.swift",
                     records / "attachment-video.mov"], cancel, log)
        runtimes = json.loads(command(["xcrun", "simctl", "list", "runtimes", "-j"], cancel, capture=True))
        available = [r for r in runtimes["runtimes"] if r["isAvailable"] and r["platform"] == "iOS"
                     and r["version"].startswith("26.")]
        if not available:
            raise ValueError("Install a stable iOS 26 Simulator runtime")
        runtime = max(available, key=lambda r: tuple(map(int, r["version"].split("."))))["identifier"]
        runs = list((build / "Build/Products").glob("*.xctestrun"))
        if len(runs) != 1:
            raise ValueError("Expected exactly one xctestrun file")
        print(f"Build: {build_seconds}s; running {len(args.tests)} tests on {workers} isolated pairs; records: {records}",
              flush=True)
        with ThreadPoolExecutor(max_workers=workers) as pool:
            futures = [pool.submit(worker, i + 1, args.tests[i::workers], target, runs[0],
                                   runtime, records / "attachment-video.mov", records, args.without_codex, cancel)
                       for i in range(workers)]
            results = [future.result() for future in futures]
        report = {"buildSeconds": build_seconds, "seconds": round(time.monotonic() - started, 2),
                  "passedTests": len(args.tests), "workers": results}
        (records / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"{len(args.tests)} iOS UI tests passed; 0 failed, 0 skipped; {report['seconds']}s; {records}", flush=True)


if __name__ == "__main__":
    main()
