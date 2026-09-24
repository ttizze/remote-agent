"""Keep Host's production dependency graph independent of client state and FFI."""
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


def dependencies(manifest):
    data = tomllib.loads(manifest.read_text())
    sections = [data, *data.get("target", {}).values()]
    for section in sections:
        for kind in ("dependencies", "build-dependencies"):
            yield from section.get(kind, {}).items()


def reachable(manifest, visited=None):
    visited = set() if visited is None else visited
    manifest = manifest.resolve()
    if manifest in visited:
        return set()
    visited.add(manifest)
    names = set()
    for name, config in dependencies(manifest):
        names.add(config.get("package", name) if isinstance(config, dict) else name)
        if isinstance(config, dict) and "path" in config:
            names.update(reachable(manifest.parent / config["path"] / "Cargo.toml", visited))
    return names


class CrateBoundariesTest(unittest.TestCase):
    def test_host_and_transport_do_not_depend_on_client_or_ffi(self):
        for crate in ("host-daemon", "agent-transport"):
            with self.subTest(crate=crate):
                names = reachable(ROOT / "crates" / crate / "Cargo.toml")
                self.assertIn("agent-protocol", names)
                self.assertFalse(names & {"agent-core", "agent-ffi", "uniffi", "markdown"})

    def test_protocol_has_no_runtime_or_client_dependencies(self):
        names = reachable(ROOT / "crates/agent-protocol/Cargo.toml")
        self.assertFalse(names & {
            "agent-core", "agent-transport", "host-daemon", "agent-ffi",
            "uniffi", "tokio", "iroh", "markdown",
        })


if __name__ == "__main__":
    unittest.main()
