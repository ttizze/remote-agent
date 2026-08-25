#!/usr/bin/env python3
"""Serve an isolated Host pairing payload to iOS Simulator tests over loopback."""

import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path


PREFIX = "Bex pairing payload: "


def read_payload(path: Path) -> bytes:
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith(PREFIX):
            return line.removeprefix(PREFIX).encode("utf-8")
    raise RuntimeError("Host log does not contain a pairing payload")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host-log", type=Path, required=True)
    parser.add_argument("--listen-port", type=int, required=True)
    arguments = parser.parse_args()
    payload = read_payload(arguments.host_log)

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            if self.path != "/pairing":
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, _format: str, *_arguments: object) -> None:
            return

    server = HTTPServer(("127.0.0.1", arguments.listen_port), Handler)
    try:
        server.serve_forever()
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
