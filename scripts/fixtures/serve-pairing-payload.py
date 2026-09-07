#!/usr/bin/env python3
"""Serve isolated UI-test invitations and external-conversation update controls."""

import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import json
import socket


def invitation(state_dir: Path) -> bytes:
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(state_dir / "host.sock"))
        stream = connection.makefile("rwb")
        stream.write(b'{"target":"manager"}\n')
        stream.flush()
        if json.loads(stream.readline()) != {"ready": True}:
            raise RuntimeError("Host manager did not become ready")
        stream.write(b'{"id":1,"method":"host/invite","params":{}}\n')
        stream.flush()
        response = json.loads(stream.readline())
        if "result" not in response:
            raise RuntimeError("Host invitation failed")
        return json.dumps(response["result"], separators=(",", ":")).encode()


def create_task(state_dir: Path) -> bytes:
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(state_dir / "host.sock"))
        stream = connection.makefile("rwb")
        stream.write(b'{"target":"local"}\n')
        stream.flush()
        if json.loads(stream.readline()) != {"ready": True}:
            raise RuntimeError("Local Host did not become ready")
        request = {"id": 1, "method": "host/thread/start", "params": {
            "cwd": str(state_dir.resolve().parent / "project"),
        }}
        stream.write(json.dumps(request).encode() + b"\n")
        stream.flush()
        while True:
            response = json.loads(stream.readline())
            if response.get("id") == 1:
                if "result" not in response:
                    raise RuntimeError("Isolated conversation creation failed")
                return json.dumps({"threadId": response["result"]["thread"]["id"]}).encode()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--listen-port", type=int, default=0)
    arguments = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            if self.path != "/pairing":
                self.send_error(404)
                return
            try:
                payload = invitation(arguments.state_dir)
            except Exception:
                self.send_error(503, "Isolated Host is unavailable")
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self) -> None:
            if self.path == "/long-conversation":
                root = arguments.state_dir.resolve().parent
                (root / "list-fixture.json").write_text(json.dumps([{
                    "id": "fixture-long-history", "cwd": str(root / "project"),
                    "name": "Long interrupted conversation", "createdAt": 10000, "updatedAt": 10000,
                    "status": {"type": "notLoaded"}, "historyMode": "paginated",
                }]))
                self.send_response(204)
                self.end_headers()
                return
            if self.path == "/release-inputs":
                (arguments.state_dir.resolve().parent / "release-inputs").touch()
                self.send_response(204)
                self.end_headers()
                return
            if self.path == "/external-conversation":
                root = arguments.state_dir.resolve().parent
                rollout = root / "external-rollout.jsonl"
                rollout.write_text("initial persisted history\n")
                (root / "list-fixture.json").write_text(json.dumps([{
                    "id": "fixture-external-thread", "cwd": str(root / "project"),
                    "name": "External conversation", "createdAt": 10000, "updatedAt": 10000,
                    "status": {"type": "notLoaded"}, "historyMode": "paginated", "path": str(rollout),
                }]))
                self.send_response(204)
                self.end_headers()
                return
            if self.path in ("/list-fixture", "/title-fixture", "/list-fixture/reset"):
                root = arguments.state_dir.resolve().parent
                projects_path = root / "projects.json"
                backup = root / "projects-before-list-fixture.json"
                fixture = root / "list-fixture.json"
                if self.path.endswith("/reset"):
                    if backup.exists():
                        projects_path.write_bytes(backup.read_bytes())
                        backup.unlink()
                    fixture.unlink(missing_ok=True)
                else:
                    if not backup.exists():
                        backup.write_bytes(projects_path.read_bytes())
                    projects = {}
                    threads = []
                    for number in range(1, 27):
                        identifier = f"pagination-project-{number}"
                        cwd = str(root / identifier)
                        projects[identifier] = {"id": identifier, "name": f"Project {number:02}",
                                                "rootPaths": [cwd], "createdAt": 1, "updatedAt": 1}
                        if self.path == "/title-fixture":
                            for conversation in range(1, 19):
                                threads.append({"id": f"pagination-project-thread-{number}-{conversation}", "cwd": cwd,
                                                "name": f"Project {number:02} conversation {conversation:02}",
                                                "preview": "Unused preview. " * 1000, "updatedAt": number * 100 + conversation})
                        else:
                            threads.append({"id": f"pagination-project-thread-{number}", "cwd": cwd,
                                            "name": f"Project conversation {number:02}", "updatedAt": number})
                    for number in range(1, 41):
                        threads.append({"id": f"pagination-chat-{number}", "cwd": str(root / "unassigned"),
                                        "name": f"Chat {number:02}", "updatedAt": 100 + number})
                    projects_path.write_text(json.dumps({"local-projects": projects,
                                                         "project-order": list(projects)}))
                    fixture.write_text(json.dumps(threads))
                self.send_response(204)
                self.end_headers()
                return
            if self.path == "/background-task":
                try:
                    payload = create_task(arguments.state_dir)
                except Exception:
                    self.send_error(503, "Isolated Host is unavailable")
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return
            if self.path != "/background-reply":
                self.send_error(404)
                return
            # The fresh fixture's Codex store changes without a live notification,
            # as when another application writes the conversation while iOS sleeps.
            (arguments.state_dir.parent / "background-reply").write_text(
                "Latest reply from another client", encoding="utf-8",
            )
            rollout = arguments.state_dir.parent / "external-rollout.jsonl"
            if rollout.exists():
                with rollout.open("a") as output:
                    output.write("external reply persisted\n")
            self.send_response(204)
            self.end_headers()

        def log_message(self, _format: str, *_arguments: object) -> None:
            pass

    server = HTTPServer(("127.0.0.1", arguments.listen_port), Handler)
    print(f"READY {server.server_port}", flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
