#!/usr/bin/env python3
"""Deterministic Codex App Server fixture for the simulator conversation-start E2E."""

import json
import os
from pathlib import Path
import sys
import time


SUPPORTED_METHODS = [
    "initialize",
    "thread/list",
    "thread/start",
    "thread/read",
    "turn/start",
]


def generate_schema(arguments: list[str]) -> int:
    try:
        output = Path(arguments[arguments.index("--out") + 1])
    except (ValueError, IndexError):
        return 2
    output.mkdir(parents=True, exist_ok=True)
    schema = {
        "oneOf": [
            {"properties": {"method": {"enum": [method]}}}
            for method in SUPPORTED_METHODS
        ]
    }
    (output / "ClientRequest.json").write_text(json.dumps(schema), encoding="utf-8")
    return 0


def trace(method: str, **facts: object) -> None:
    path = os.environ.get("BEX_FAKE_CODEX_TRACE")
    if not path:
        return
    with open(path, "a", encoding="utf-8") as output:
        output.write(json.dumps({"method": method, **facts}, separators=(",", ":")))
        output.write("\n")


def respond(request_id: object, *, result: object = None, error: object = None) -> None:
    response = {"id": request_id}
    if error is not None:
        response["error"] = error
    else:
        response["result"] = result
    sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notify(method: str, params: object) -> None:
    sys.stdout.write(json.dumps({"method": method, "params": params}, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def request(request_id: str, method: str, params: object) -> None:
    sys.stdout.write(
        json.dumps({"id": request_id, "method": method, "params": params}, separators=(",", ":")) + "\n"
    )
    sys.stdout.flush()


def thread_value(thread_id: str, cwd: str) -> dict[str, object]:
    return {
        "id": thread_id,
        "name": "Simulator conversation",
        "preview": "",
        "cwd": cwd,
        "createdAt": 1,
        "updatedAt": 1,
        "status": {"type": "idle"},
        "turns": [],
    }


def stream_item(thread_id: str, turn: dict[str, object], item: dict[str, object]) -> None:
    turn["items"].append(item)
    notify("item/started", {"threadId": thread_id, "turnId": turn["id"], "item": item})


def complete_item(thread_id: str, turn: dict[str, object], item: dict[str, object]) -> None:
    notify("item/completed", {"threadId": thread_id, "turnId": turn["id"], "item": item})


def finish_turn(thread: dict[str, object], turn: dict[str, object], status: str, error: object = None) -> None:
    turn["status"] = status
    turn["completedAt"] = 4
    turn["durationMs"] = 3000
    if error is not None:
        turn["error"] = error
    thread["status"] = {"type": "idle"}
    notify("turn/completed", {"threadId": thread["id"], "turn": turn})
    notify("thread/status/changed", {"threadId": thread["id"], "status": {"type": "idle"}})


def run_turn_scenario(thread: dict[str, object], turn: dict[str, object], prompt: str, suffix: str) -> None:
    thread_id = str(thread["id"])
    turn_id = str(turn["id"])
    scenario = next(
        (name for name in ("retry", "failed", "interrupted", "request", "items", "history")
         if f"[{name}]" in prompt.lower()),
        "success",
    )
    if scenario == "history":
        thread["name"] = "History conversation"

    time.sleep(0.5)
    notify("turn/started", {"threadId": thread_id, "turn": turn})
    base_items = [
        {"id": f"fixture-user-{suffix}", "type": "userMessage", "text": prompt},
        {
            "id": f"fixture-commentary-{suffix}",
            "type": "agentMessage",
            "text": "シミュレータで確認しています…",
            "phase": "commentary",
        },
        {"id": f"fixture-reasoning-{suffix}", "type": "reasoning", "summary": ["確認中"]},
        {
            "id": f"fixture-command-{suffix}",
            "type": "commandExecution",
            "command": "./gradlew test",
            "cwd": thread["cwd"],
            "aggregatedOutput": "running",
            "status": "inProgress",
        },
    ]
    for item in base_items:
        stream_item(thread_id, turn, item)

    if scenario == "retry":
        notify(
            "error",
            {
                "threadId": thread_id,
                "turnId": turn_id,
                "willRetry": True,
                "error": {
                    "message": "stream disconnected",
                    "additionalDetails": "attempt 2 of 5",
                    "codexErrorInfo": {"responseStreamDisconnected": {"httpStatusCode": 429}},
                },
            },
        )
    elif scenario == "request":
        request(
            f"fixture-request-{suffix}",
            "item/tool/requestUserInput",
            {
                "threadId": thread_id,
                "turnId": turn_id,
                "questions": [{"question": "このまま続けますか？"}],
            },
        )
    elif scenario == "items":
        extra_items = [
            {"id": f"fixture-plan-{suffix}", "type": "plan", "text": "確認計画"},
            {
                "id": f"fixture-mcp-{suffix}", "type": "mcpToolCall", "server": "fixture",
                "tool": "lookup", "status": "inProgress",
            },
            {"id": f"fixture-dynamic-{suffix}", "type": "dynamicToolCall", "tool": "fixture", "status": "inProgress"},
            {"id": f"fixture-collab-{suffix}", "type": "collabAgentToolCall", "tool": "spawn_agent", "status": "inProgress"},
            {"id": f"fixture-subagent-{suffix}", "type": "subAgentActivity", "status": "running"},
            {"id": f"fixture-web-{suffix}", "type": "webSearch", "query": "Codex"},
            {"id": f"fixture-image-{suffix}", "type": "imageView", "path": "/tmp/fixture.png"},
            {"id": f"fixture-sleep-{suffix}", "type": "sleep"},
            {"id": f"fixture-review-in-{suffix}", "type": "enteredReviewMode"},
            {"id": f"fixture-review-out-{suffix}", "type": "exitedReviewMode"},
            {"id": f"fixture-compaction-{suffix}", "type": "contextCompaction"},
        ]
        for item in extra_items:
            stream_item(thread_id, turn, item)

    time.sleep(float(os.environ.get("BEX_FAKE_STREAM_DELAY_SECONDS", "2")))
    completed_command = dict(base_items[-1], aggregatedOutput="passed", status="completed", exitCode=0)
    turn["items"][3] = completed_command
    complete_item(thread_id, turn, completed_command)

    if scenario == "request":
        notify("serverRequest/resolved", {"threadId": thread_id, "requestId": f"fixture-request-{suffix}"})
    if scenario == "failed":
        error = {
            "message": "context is full",
            "codexErrorInfo": "contextWindowExceeded",
        }
        notify("error", {"threadId": thread_id, "turnId": turn_id, "willRetry": False, "error": error})
        finish_turn(thread, turn, "failed", error)
        return
    if scenario == "interrupted":
        finish_turn(thread, turn, "interrupted")
        return

    final_item = {
        "id": f"fixture-final-{suffix}",
        "type": "agentMessage",
        "text": "",
        "phase": "final_answer",
    }
    stream_item(thread_id, turn, final_item)
    notify(
        "item/agentMessage/delta",
        {
            "threadId": thread_id,
            "turnId": turn_id,
            "itemId": final_item["id"],
            "delta": "シミュレータで完了しました。",
        },
    )
    final_item["text"] = "シミュレータで完了しました。"
    complete_item(thread_id, turn, final_item)
    finish_turn(thread, turn, "completed")


def run_server() -> int:
    threads: dict[str, dict[str, object]] = {}
    expected_cwd = os.environ.get("BEX_FAKE_CODEX_EXPECTED_CWD")
    next_thread_number = 0

    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if "id" not in message:
            continue
        request_id = message["id"]
        params = message.get("params") or {}

        if method == "initialize":
            respond(
                request_id,
                result={
                    "userAgent": "remote-agent-simulator-fixture",
                    "platformFamily": "unix",
                    "platformOs": "macos",
                    "codexHome": os.environ.get("CODEX_HOME", "/tmp"),
                },
            )
        elif method == "thread/list":
            respond(request_id, result={"data": list(threads.values()), "nextCursor": None})
        elif method == "thread/start":
            has_project_id = "projectId" in params
            cwd = params.get("cwd")
            cwd_matches = expected_cwd is None or cwd == expected_cwd
            trace(
                method,
                hasProjectId=has_project_id,
                cwdMatchesFixture=cwd_matches,
            )
            if has_project_id:
                respond(
                    request_id,
                    error={"code": -32600, "message": "project not found: desktop-project"},
                )
                continue
            if not isinstance(cwd, str) or not cwd or not cwd_matches:
                respond(request_id, error={"code": -32602, "message": "invalid cwd"})
                continue
            next_thread_number += 1
            thread = thread_value(f"fixture-thread-{next_thread_number}", cwd)
            thread["createdAt"] = next_thread_number
            thread["updatedAt"] = next_thread_number
            threads[thread["id"]] = thread
            respond(request_id, result={"thread": thread})
        elif method == "turn/start":
            thread_id = params.get("threadId")
            inputs = params.get("input")
            has_text_input = (
                isinstance(inputs, list)
                and len(inputs) == 1
                and isinstance(inputs[0], dict)
                and inputs[0].get("type") == "text"
                and isinstance(inputs[0].get("text"), str)
                and bool(inputs[0]["text"].strip())
            )
            trace(
                method,
                threadExists=thread_id in threads,
                hasTextInput=has_text_input,
            )
            if thread_id not in threads or not has_text_input:
                respond(request_id, error={"code": -32602, "message": "invalid turn input"})
                continue
            suffix = str(thread_id).rsplit("-", 1)[-1]
            turn_id = f"fixture-turn-{suffix}"
            turn = {
                "id": turn_id,
                "status": "inProgress",
                "items": [],
                "startedAt": 1,
            }
            threads[thread_id]["turns"] = [turn]
            threads[thread_id]["status"] = {"type": "active", "activeFlags": []}
            respond(request_id, result={"turn": {"id": turn_id}})
            run_turn_scenario(threads[thread_id], turn, inputs[0]["text"], suffix)
        elif method == "thread/read":
            thread = threads.get(params.get("threadId"))
            if thread is None:
                respond(request_id, error={"code": -32602, "message": "thread not found"})
            else:
                respond(request_id, result={"thread": thread})
        else:
            respond(request_id, error={"code": -32601, "message": "method not found"})
    return 0


def main(arguments: list[str]) -> int:
    if arguments[:2] == ["app-server", "generate-json-schema"]:
        return generate_schema(arguments)
    if arguments[:1] == ["app-server"]:
        return run_server()
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
