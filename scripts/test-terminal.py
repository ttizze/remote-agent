#!/usr/bin/env python3
"""Test the vendored terminal backend without making it a workspace member."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def remote_packages(lock):
    return {
        (package["name"], package["version"], package["source"], package.get("checksum"))
        for package in lock.get("package", []) if "source" in package
    }


def verify_lock_subset(original, resolved):
    """Cargo may prune the workspace lock, but cannot change any remote package."""
    packages = remote_packages(resolved)
    changed = packages - remote_packages(original)
    if changed:
        descriptions = [f"{name} {version} ({source})" for name, version, source, _ in sorted(changed)]
        raise ValueError("terminal tests changed locked dependencies: " + ", ".join(descriptions))
    return len(packages)


def main():
    env = os.environ.copy()
    # Cargo runs from a temporary workspace. Preserve the caller's target path,
    # including the meaning of a relative CARGO_TARGET_DIR.
    env["CARGO_TARGET_DIR"] = str(Path(env.get("CARGO_TARGET_DIR", ROOT / "target/terminal-tests")).resolve())
    version = subprocess.check_output(["rustc", "-vV"], env=env, text=True)
    host = next(line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: "))
    platform = env.get("CARGO_BUILD_TARGET", host)
    seed = (ROOT / "Cargo.lock").read_bytes()
    original = tomllib.loads(seed.decode("utf-8"))
    with tempfile.TemporaryDirectory(prefix="bex-terminal-tests-") as temporary:
        workspace = Path(temporary)
        for crate in ("alacritty_terminal", "vte"):
            shutil.copytree(ROOT / "vendor" / crate, workspace / crate)
        (workspace / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["alacritty_terminal"]\nresolver = "2"\n'
            '[patch.crates-io]\nvte = { path = "vte" }\n'
            '[profile.dev]\ndebug = "line-tables-only"\n',
            encoding="utf-8",
        )
        lock = workspace / "Cargo.lock"
        lock.write_bytes(seed)
        # The native build populates this target's cache first. Filtering avoids
        # fetching other platforms' packages during an offline native test run.
        subprocess.run(
            ["cargo", "metadata", "--offline", "--format-version", "1", "--filter-platform", platform],
            cwd=workspace, env=env, check=True, stdout=subprocess.DEVNULL,
        )
        count = verify_lock_subset(original, tomllib.loads(lock.read_text(encoding="utf-8")))
        print(f"Verified {count} terminal dependencies against the repository lockfile", flush=True)
        subprocess.run(
            ["cargo", "test", "--offline", "--locked", "-p", "alacritty_terminal", "--lib"],
            cwd=workspace, env=env, check=True,
        )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
    except (OSError, ValueError) as error:
        print(f"terminal tests failed: {error}", file=sys.stderr)
        sys.exit(1)
