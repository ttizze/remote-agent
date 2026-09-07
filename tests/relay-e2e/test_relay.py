#!/usr/bin/env python3
"""Exercise the Phoenix relay with two real WebSocket clients.

The test deliberately uses only Python's standard library.  This keeps the
full-stack check independent of a WebSocket client package while still
testing the HTTP upgrade, masking, Phoenix channel envelopes, and the actual
Phoenix application process.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlencode


ROOT = Path(__file__).resolve().parents[2]
SERVER_DIR = ROOT / "apps" / "server"
SERVER_START_TIMEOUT = 30.0
MESSAGE_TIMEOUT = 5.0
HOST_START_TIMEOUT = 15.0
MAX_HTTP_HEADERS = 64 * 1024
MAX_FRAME_PAYLOAD = 4 * 1024 * 1024
HOST_DAEMON = ROOT / "target" / "debug" / "host-daemon"
FAKE_CODEX = ROOT / "scripts" / "fixtures" / "fake-codex-app-server.py"


class RelayTestError(RuntimeError):
    """A deterministic relay test failure with safe, non-secret details."""


class WebSocketClosed(RelayTestError):
    """The peer closed the WebSocket before the expected message arrived."""


class WebSocket:
    """The small RFC 6455 client needed by this test.

    Phoenix sends server frames without a mask and expects client frames to be
    masked.  The implementation handles text, ping/pong, and close frames;
    other frame types are rejected because they are outside this test's wire
    contract.
    """

    def __init__(self, host: str, port: int, path: str, timeout: float) -> None:
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._socket.settimeout(timeout)
        self._buffer = bytearray()
        self._closed = False
        self._handshake(path)

    def __enter__(self) -> "WebSocket":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        finally:
            self._socket.close()

    def send_json(self, value: Any) -> None:
        payload = json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        self._send_frame(0x1, payload)

    def receive_json(self) -> Any:
        while True:
            opcode, payload = self._receive_frame()
            if opcode == 0x1:
                try:
                    return json.loads(payload.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise RelayTestError("relay sent invalid JSON text") from error
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            if opcode == 0x8:
                raise WebSocketClosed("relay closed the WebSocket")
            raise RelayTestError("relay sent an unsupported WebSocket frame")

    def receive_until(
        self, predicate: Callable[[Any], bool], description: str, timeout: float
    ) -> Any:
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RelayTestError(f"timed out waiting for {description}")
            self._socket.settimeout(remaining)
            try:
                value = self.receive_json()
            except socket.timeout as error:
                raise RelayTestError(f"timed out waiting for {description}") from error
            if predicate(value):
                return value

    def _handshake(self, path: str) -> None:
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        request = (
            f"GET {path} HTTP/1.1\r\n"
            "Host: 127.0.0.1\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        ).encode("ascii")
        self._socket.sendall(request)

        while b"\r\n\r\n" not in self._buffer:
            chunk = self._socket.recv(4096)
            if not chunk:
                raise WebSocketClosed("relay closed during the WebSocket handshake")
            self._buffer.extend(chunk)
            if len(self._buffer) > MAX_HTTP_HEADERS:
                raise RelayTestError("relay WebSocket handshake headers are too large")

        header_bytes, remainder = self._buffer.split(b"\r\n\r\n", 1)
        self._buffer = bytearray(remainder)
        lines = header_bytes.decode("latin-1").split("\r\n")
        if not lines or not lines[0].startswith("HTTP/1.1 101 "):
            raise RelayTestError("relay did not accept the WebSocket upgrade")

        headers: dict[str, str] = {}
        for line in lines[1:]:
            name, separator, value = line.partition(":")
            if separator:
                headers[name.strip().lower()] = value.strip()

        expected_accept = base64.b64encode(
            hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
            ).digest()
        ).decode("ascii")
        if headers.get("sec-websocket-accept") != expected_accept:
            raise RelayTestError("relay returned an invalid WebSocket handshake")

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        if len(payload) > MAX_FRAME_PAYLOAD:
            raise RelayTestError("test WebSocket frame is too large")
        mask = secrets.token_bytes(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        length = len(payload)
        if length < 126:
            header = bytes((0x80 | opcode, 0x80 | length))
        elif length <= 0xFFFF:
            header = bytes((0x80 | opcode, 0x80 | 126)) + struct.pack(">H", length)
        else:
            header = bytes((0x80 | opcode, 0x80 | 127)) + struct.pack(">Q", length)
        self._socket.sendall(header + mask + masked)

    def _receive_frame(self) -> tuple[int, bytes]:
        first, second = self._read_exact(2)
        if not first & 0x80:
            raise RelayTestError("fragmented relay WebSocket frames are unsupported")
        if first & 0x70:
            raise RelayTestError("relay set a reserved WebSocket frame bit")

        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._read_exact(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._read_exact(8))[0]
        if length > MAX_FRAME_PAYLOAD:
            raise RelayTestError("relay WebSocket frame is too large")

        masked = bool(second & 0x80)
        mask = self._read_exact(4) if masked else b""
        payload = self._read_exact(length)
        if masked:
            payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        return opcode, payload

    def _read_exact(self, length: int) -> bytes:
        while len(self._buffer) < length:
            chunk = self._socket.recv(max(4096, length - len(self._buffer)))
            if not chunk:
                raise WebSocketClosed("relay closed the WebSocket")
            self._buffer.extend(chunk)
        value = bytes(self._buffer[:length])
        del self._buffer[:length]
        return value


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def _server_command() -> list[str]:
    mix = shutil.which("mix")
    if mix:
        return [mix, "run", "--no-halt"]
    nix = shutil.which("nix")
    if nix:
        return [
            nix,
            "shell",
            "nixpkgs#beamPackages.elixir_1_20",
            "-c",
            "mix",
            "run",
            "--no-halt",
        ]
    raise RelayTestError("neither mix nor nix is available to start apps/server")


def _start_server(
    port: int, token: str
) -> tuple[subprocess.Popen[bytes], tempfile.TemporaryFile[bytes]]:
    output = tempfile.TemporaryFile(mode="w+b")
    environment = os.environ.copy()
    environment["REMOTE_AGENT_RELAY_TOKEN"] = token
    environment["PHX_SERVER"] = "true"
    environment["PORT"] = str(port)
    process = subprocess.Popen(
        _server_command(),
        cwd=SERVER_DIR,
        env=environment,
        stdout=output,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    deadline = time.monotonic() + SERVER_START_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            output.close()
            raise RelayTestError("Phoenix relay server exited before it was ready")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return process, output
        except OSError:
            time.sleep(0.05)

    _stop_server(process, output)
    raise RelayTestError("timed out waiting for Phoenix relay server")


def _start_host(
    port: int,
    token: str,
    runner_id: str,
    codex_home: str,
    trace_path: str,
) -> tuple[subprocess.Popen[bytes], tempfile.TemporaryFile[bytes]]:
    if not HOST_DAEMON.is_file() or not os.access(HOST_DAEMON, os.X_OK):
        raise RelayTestError(
            "target/debug/host-daemon is unavailable; build it with "
            "nix develop . --command cargo build -p host-daemon"
        )
    if not FAKE_CODEX.is_file() or not os.access(FAKE_CODEX, os.X_OK):
        raise RelayTestError("the fake Codex App Server fixture is unavailable")

    output = tempfile.TemporaryFile(mode="w+b")
    environment = os.environ.copy()
    environment["CODEX_HOME"] = codex_home
    environment["BEX_FAKE_CODEX_TRACE"] = trace_path
    environment["REMOTE_AGENT_RELAY_TOKEN"] = token
    command = [
        str(HOST_DAEMON),
        "--relay-url",
        f"ws://127.0.0.1:{port}/socket/websocket",
        "--relay-token",
        token,
        "--runner-id",
        runner_id,
        "--codex",
        str(FAKE_CODEX),
    ]
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        env=environment,
        stdout=output,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    return process, output


def _stop_server(process: subprocess.Popen[bytes], output: tempfile.TemporaryFile[bytes]) -> None:
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    output.close()


def _socket_path(token: str, runner_id: str, role: str) -> str:
    query = urlencode(
        {
            "vsn": "2.0.0",
            "token": token,
            "runner_id": runner_id,
            "role": role,
        }
    )
    return f"/socket/websocket?{query}"


def _join(socket_: WebSocket, topic: str, role: str) -> str:
    join_ref = f"join-{role}"
    socket_.send_json([join_ref, join_ref, topic, "phx_join", {}])
    reply = socket_.receive_until(
        lambda value: isinstance(value, list)
        and len(value) == 5
        and value[0] == join_ref
        and value[2] == topic
        and value[3] == "phx_reply",
        f"{role} channel join reply",
        MESSAGE_TIMEOUT,
    )
    payload = reply[4]
    if not isinstance(payload, dict) or payload.get("status") != "ok":
        raise RelayTestError(f"{role} channel join was rejected")
    return join_ref


def _jsonl_event_for_request(value: Any, topic: str, request_id: str) -> bool:
    if not (
        isinstance(value, list)
        and len(value) == 5
        and value[2] == topic
        and value[3] == "jsonl"
        and isinstance(value[4], dict)
        and isinstance(value[4].get("line"), str)
    ):
        return False
    try:
        line = json.loads(value[4]["line"])
    except json.JSONDecodeError:
        return False
    return isinstance(line, dict) and line.get("id") == request_id


def _push_and_receive(
    mobile: WebSocket,
    join_ref: str,
    topic: str,
    line: str,
    request_id: str,
    description: str,
    timeout: float,
) -> str:
    mobile.send_json([join_ref, f"request-{request_id}", topic, "jsonl", {"line": line}])
    event = mobile.receive_until(
        lambda value: _jsonl_event_for_request(value, topic, request_id),
        description,
        timeout,
    )
    return event[4]["line"]


def _run_raw_relay_round_trip() -> None:
    token = secrets.token_urlsafe(24)
    runner_id = f"relay-e2e-{uuid.uuid4().hex}"
    topic = f"runner:{runner_id}"
    port = _find_free_port()
    server, output = _start_server(port, token)

    mobile: WebSocket | None = None
    runner: WebSocket | None = None
    try:
        mobile = WebSocket(
            "127.0.0.1", port, _socket_path(token, runner_id, "mobile"), MESSAGE_TIMEOUT
        )
        runner = WebSocket(
            "127.0.0.1", port, _socket_path(token, runner_id, "runner"), MESSAGE_TIMEOUT
        )
        _join(mobile, topic, "mobile")
        _join(runner, topic, "runner")

        request_line = (
            '{"id":"relay-e2e-request","method":"thread/start",'
            '"params":{"prompt":"relay round trip"}}'
        )
        request_ref = "mobile-request"
        mobile.send_json(["join-mobile", request_ref, topic, "jsonl", {"line": request_line}])

        forwarded_request = runner.receive_until(
            lambda value: isinstance(value, list)
            and len(value) == 5
            and value[2] == topic
            and value[3] == "jsonl",
            "the mobile JSONL request on the runner socket",
            MESSAGE_TIMEOUT,
        )
        if forwarded_request[4] != {"line": request_line}:
            raise RelayTestError("runner received a JSONL request different from mobile input")

        mobile_reply = mobile.receive_until(
            lambda value: isinstance(value, list)
            and len(value) == 5
            and value[0] == "join-mobile"
            and value[1] == request_ref
            and value[2] == topic
            and value[3] == "phx_reply",
            "the mobile JSONL push acknowledgement",
            MESSAGE_TIMEOUT,
        )
        if not isinstance(mobile_reply[4], dict) or mobile_reply[4].get("status") != "ok":
            raise RelayTestError("Phoenix did not acknowledge the mobile JSONL push")

        response_line = '{"id":"relay-e2e-response","result":{"ok":true}}'
        response_ref = "runner-response"
        runner.send_json(["join-runner", response_ref, topic, "jsonl", {"line": response_line}])

        forwarded_response = mobile.receive_until(
            lambda value: isinstance(value, list)
            and len(value) == 5
            and value[2] == topic
            and value[3] == "jsonl",
            "the runner JSONL response on the mobile socket",
            MESSAGE_TIMEOUT,
        )
        if forwarded_response[4] != {"line": response_line}:
            raise RelayTestError("mobile received a JSONL response different from runner input")

        runner_reply = runner.receive_until(
            lambda value: isinstance(value, list)
            and len(value) == 5
            and value[0] == "join-runner"
            and value[1] == response_ref
            and value[2] == topic
            and value[3] == "phx_reply",
            "the runner JSONL push acknowledgement",
            MESSAGE_TIMEOUT,
        )
        if not isinstance(runner_reply[4], dict) or runner_reply[4].get("status") != "ok":
            raise RelayTestError("Phoenix did not acknowledge the runner JSONL push")
    finally:
        if mobile is not None:
            mobile.close()
        if runner is not None:
            runner.close()
        _stop_server(server, output)


def _run_host_daemon_round_trip() -> None:
    """Prove mobile -> Phoenix -> Rust host -> fake Codex -> mobile."""

    token = secrets.token_urlsafe(24)
    runner_id = f"relay-host-e2e-{uuid.uuid4().hex}"
    topic = f"runner:{runner_id}"
    port = _find_free_port()
    server: subprocess.Popen[bytes] | None = None
    server_output: tempfile.TemporaryFile[bytes] | None = None
    host: subprocess.Popen[bytes] | None = None
    host_output: tempfile.TemporaryFile[bytes] | None = None
    mobile: WebSocket | None = None

    with tempfile.TemporaryDirectory(prefix="remote-agent-relay-e2e-") as codex_home:
        trace_path = str(Path(codex_home) / "fake-codex-trace.jsonl")
        try:
            server, server_output = _start_server(port, token)
            host, host_output = _start_host(
                port, token, runner_id, codex_home, trace_path
            )
            mobile = WebSocket(
                "127.0.0.1",
                port,
                _socket_path(token, runner_id, "mobile"),
                MESSAGE_TIMEOUT,
            )
            join_ref = _join(mobile, topic, "mobile")

            probe_line = (
                '{"id":"relay-host-probe","method":"thread/list","params":{}}'
            )
            probe_deadline = time.monotonic() + HOST_START_TIMEOUT
            probe_response: str | None = None
            while time.monotonic() < probe_deadline:
                if host.poll() is not None:
                    raise RelayTestError(
                        "host-daemon exited before joining the Phoenix relay"
                    )
                remaining = min(1.0, probe_deadline - time.monotonic())
                try:
                    probe_response = _push_and_receive(
                        mobile,
                        join_ref,
                        topic,
                        probe_line,
                        "relay-host-probe",
                        "the host-daemon probe response",
                        remaining,
                    )
                    break
                except RelayTestError:
                    continue
            if probe_response is None:
                raise RelayTestError(
                    "timed out waiting for host-daemon to join and answer"
                )
            if json.loads(probe_response) != {
                "id": "relay-host-probe",
                "result": {"data": [], "nextCursor": None},
            }:
                raise RelayTestError("host-daemon probe response was not exact")

            workspace = "/tmp/remote-agent-relay-e2e-workspace"
            request_line = (
                '{"id":"relay-host-request","method":"thread/start",'
                f'"params":{{"cwd":"{workspace}"}}}}'
            )
            response_line = _push_and_receive(
                mobile,
                join_ref,
                topic,
                request_line,
                "relay-host-request",
                "the fake Codex response through host-daemon",
                MESSAGE_TIMEOUT,
            )
            response = json.loads(response_line)
            thread = response.get("result", {}).get("thread", {})
            if response.get("id") != "relay-host-request" or thread.get("id") != "fixture-thread-1":
                raise RelayTestError("host-daemon returned an unexpected thread response")
            if thread.get("cwd") != workspace:
                raise RelayTestError("host-daemon changed the Codex request cwd")

            with open(trace_path, encoding="utf-8") as trace:
                traces = [json.loads(line) for line in trace if line.strip()]
            if not any(
                entry.get("method") == "thread/start"
                and entry.get("hasProjectId") is False
                and entry.get("cwdMatchesFixture") is True
                for entry in traces
            ):
                raise RelayTestError("fake Codex did not observe the host thread request")
        finally:
            if mobile is not None:
                mobile.close()
            if host is not None and host_output is not None:
                _stop_server(host, host_output)
            if server is not None and server_output is not None:
                _stop_server(server, server_output)


def run() -> None:
    _run_raw_relay_round_trip()
    _run_host_daemon_round_trip()


def main() -> int:
    try:
        run()
    except (OSError, RelayTestError) as error:
        print(f"relay-e2e: FAIL: {error}", file=sys.stderr)
        return 1
    print(
        "relay-e2e: PASS (Phoenix process, mobile WebSocket, raw relay, "
        "host-daemon, fake Codex)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
