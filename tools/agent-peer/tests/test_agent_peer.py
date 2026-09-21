import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import uuid


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "agent-peer"
loader = importlib.machinery.SourceFileLoader("agent_peer", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
peer = importlib.util.module_from_spec(spec)
loader.exec_module(peer)


class PeerTests(unittest.TestCase):
    def test_claude_denial_is_not_success(self):
        result = peer.decode("claude", json.dumps({
            "type": "result", "result": "Partial answer", "session_id": str(uuid.uuid4()),
            "permission_denials": [{"tool_name": "Read"}]}), 0)
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["text"], "Partial answer")

    def test_codex_requires_turn_completion(self):
        output = json.dumps({"type": "item.completed", "item": {
            "type": "agent_message", "text": "Working…"}})
        self.assertEqual(peer.decode("codex", output, 0)["status"], "error")

    def test_codex_failed_turn_keeps_error(self):
        output = '\n'.join(map(json.dumps, [
            {"type": "turn.failed", "error": {"message": "quota exhausted"}},
            {"type": "item.completed", "item": {"type": "agent_message", "text": "Partial"}}]))
        result = peer.decode("codex", output, 1)
        self.assertEqual(result["errors"], ["quota exhausted"])
        self.assertEqual(result["status"], "error")

    def test_timeout_stops_child(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "late-write"
            with self.assertRaises(subprocess.TimeoutExpired):
                peer.invoke([sys.executable, "-c",
                             "import time,pathlib; time.sleep(3); pathlib.Path('late-write').touch()"],
                            directory, "", 0.05)
            self.assertFalse(marker.exists())

    def test_both_cli_protocols_and_resume_with_literal_stdin(self):
        import os
        for provider in ("claude", "codex"):
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as directory:
                fake = Path(directory) / provider
                fake.write_text('''#!/usr/bin/env python3
import json, pathlib, sys, uuid
args = sys.argv[1:]
if "--resume" in args:
    sid = args[args.index("--resume") + 1]
elif "resume" in args:
    sid = args[args.index("resume") + 1]
else:
    sid = str(uuid.uuid4())
saved = pathlib.Path(sid)
text = sys.stdin.read()
reply = saved.read_text() if saved.exists() else text
saved.write_text(text)
if pathlib.Path(sys.argv[0]).name == "claude":
    print(json.dumps(dict(type="result", session_id=sid, result=reply)))
else:
    for e in [dict(type="thread.started", thread_id=sid),
              dict(type="item.completed", item=dict(type="agent_message", text=reply)),
              dict(type="turn.completed")]:
        print(json.dumps(e))
''')
                fake.chmod(0o755)
                env = dict(os.environ, **{"AGENT_PEER_" + provider.upper() + "_BIN": str(fake)})
                prompt = "Literal $(touch UNEXPECTED) `touch UNEXPECTED`\n日本語"
                first = subprocess.run([sys.executable, str(SCRIPT), "ask", provider,
                                        "--cwd", directory, "-"], input=prompt, text=True,
                                       capture_output=True, env=env, check=True)
                result = json.loads(first.stdout)
                self.assertEqual(result["text"], prompt)
                reply = subprocess.run([sys.executable, str(SCRIPT), "reply", provider,
                                        result["session_id"], "--cwd", directory, "recall"],
                                       text=True, capture_output=True, env=env, check=True)
                resumed = json.loads(reply.stdout)
                self.assertEqual(resumed["session_id"], result["session_id"])
                self.assertEqual(resumed["text"], prompt)
                self.assertFalse((Path(directory) / "UNEXPECTED").exists())


if __name__ == "__main__":
    unittest.main()
