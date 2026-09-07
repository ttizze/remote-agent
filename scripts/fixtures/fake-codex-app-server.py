#!/usr/bin/env python3
"""Deterministic Codex App Server fixture for the simulator conversation-start E2E."""

import base64
import json
import os
from pathlib import Path
import sys
import time
import threading

OUTPUT_LOCK = threading.Lock()
PENDING = {}
CONTROLS = {}
DELAYED_INPUT_TURNS = set()


SUPPORTED_METHODS = [
    "initialize",
    "model/list",
    "thread/list",
    "thread/start",
    "thread/read",
    "thread/turns/list",
    "thread/items/list",
    "thread/resume",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
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
    with OUTPUT_LOCK:
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def notify(method: str, params: object) -> None:
    with OUTPUT_LOCK:
        sys.stdout.write(json.dumps({"method": method, "params": params}, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def request(request_id: str, method: str, params: object) -> None:
    with OUTPUT_LOCK:
        sys.stdout.write(json.dumps({"id": request_id, "method": method, "params": params}, separators=(",", ":")) + "\n")
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


def persisted_thread(thread):
    if thread["id"] == "fixture-long-history":
        # Uneven, expanded interrupted/failed turns expose lazy-scroll geometry
        # failures that many short, completed turns cannot reproduce.
        sizes = [286, 6, 325, 393, 859, 424, 21, 609, 641, 154]
        inputs = [4, 1, 6, 8, 1, 2, 1, 4, 1, 2]
        statuses = ["failed"] + ["completed"] * 6 + ["interrupted", "completed", "interrupted"]
        turns = []
        for number, (size, user_count, status) in enumerate(zip(sizes, inputs, statuses)):
            boundaries = {index * (size - 1) // user_count for index in range(user_count)}
            items = []
            for index in range(size):
                identifier = f"long-{number}-{index}"
                if index in boundaries:
                    item = {"id": identifier, "type": "userMessage", "content": [
                        {"type": "text", "text": f"Review section {number}, input {index}."},
                    ]}
                elif index == size - 1 or index % 5 == 0:
                    item = {"id": identifier, "type": "agentMessage", "text": f"Section {number}, progress {index}. " * 4,
                            "phase": "final_answer" if status == "completed" and index == size - 1 else "commentary"}
                else:
                    item = {"id": identifier, "type": "commandExecution", "command": f"inspect section-{number}/file-{index}.txt",
                            "status": "completed", "aggregatedOutput": "Inspection complete.\n" * 12, "exitCode": 0}
                items.append(item)
            turns.append({"id": f"long-turn-{number}", "status": status, "items": items})
        turns[-1]["items"][-1].update(id="long-latest-message", text="Latest interrupted conversation message is visible.")
        thread = dict(thread, turns=turns)
    elif str(thread["cwd"]).endswith("large-history"):
        thread = dict(thread, turns=[{"id":"large-turn", "status":"completed", "items":[
            {"id":"large-user", "type":"userMessage", "content":[{"type":"text", "text":"Read the whole output"}]},
            {"id":"large-command", "type":"commandExecution", "command":"cat output.txt", "status":"completed", "aggregatedOutput":"output line\n" * 700000 + "END_OF_LARGE_OUTPUT"},
            {"id":"large-final", "type":"agentMessage", "phase":"final_answer", "text":"Large history is complete"}
        ]}])
    elif thread.get("name") == "History conversation":
        # A large body exists only in persisted history, avoiding live-event
        # preloading so the Simulator must exercise lazy detail retrieval.
        import copy
        thread = copy.deepcopy(thread)
        for turn in thread["turns"]:
            for item in turn["items"]:
                if item["type"] == "commandExecution":
                    item["aggregatedOutput"] = "DEFERRED_DETAIL_FULL_TEXT\n" + "fixture output\n" * 500
    return thread


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


def wait_for_input_release(stop: threading.Event) -> bool:
    deadline = time.monotonic() + 30
    path = Path(os.environ["CODEX_HOME"]) / "release-inputs"
    while time.monotonic() < deadline and not stop.is_set():
        if path.exists():
            return True
        stop.wait(0.02)
    return False


def deliver_deferred_input(thread_id, turn, item, stop):
    if wait_for_input_release(stop):
        stream_item(thread_id, turn, item)
        stream_item(thread_id, turn, {
            "id": "fixture-steer-recorded-" + item["id"], "type": "agentMessage",
            "phase": "commentary", "text": "Additional input recorded",
        })


def run_turn_scenario(thread: dict[str, object], turn: dict[str, object], inputs: list, suffix: str, stop: threading.Event, client_id: str | None) -> None:
    prompt = next((item["text"] for item in inputs if item.get("type") == "text"), "")
    thread_id = str(thread["id"])
    turn_id = str(turn["id"])
    scenario = next(
        (name for name in ("retry", "failed", "interrupted", "request", "approval", "items", "history", "duplicate", "followups")
         if f"[{name}]" in prompt.lower()),
        "success",
    )
    if scenario == "history":
        thread["name"] = "History conversation"

    image_path = None
    image_url = None
    if "[images]" in prompt:
        image_path = Path(os.environ["CODEX_HOME"]) / "fixture image.png"
        image_bytes = (Path(__file__).resolve().parents[2] / "apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png").read_bytes()
        image_path.write_bytes(image_bytes)
        image_url = "data:image/png;base64," + base64.b64encode(image_bytes).decode("ascii")
        inputs = [*inputs, {"type": "localImage", "path": str(image_path)}]

    notify("turn/started", {"threadId": thread_id, "turn": turn})
    if "[delayed-input]" in prompt and not wait_for_input_release(stop):
        return
    base_items = [
        {"id": f"fixture-user-{suffix}", "clientId": client_id, "type": "userMessage", "text": prompt, "content": inputs},
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
    elif scenario in ("request", "approval"):
        request_id = f"fixture-request-{suffix}"
        answer = threading.Event()
        PENDING[request_id] = {"event": answer, "result": None}
        params = {"threadId": thread_id, "turnId": turn_id, "itemId": base_items[-1]["id"]}
        if scenario == "request":
            params["questions"] = [{"id":"continue", "header":"継続", "question":"このまま続けますか？", "options":[{"label":"続ける","description":"作業を進めます"},{"label":"見直す","description":"方針を見直します"}], "isOther":True, "isSecret":False}]
            method = "item/tool/requestUserInput"
        else:
            params.update({"command":"echo fixture", "cwd":thread["cwd"], "reason":"結合テストの承認確認"})
            method = "item/commandExecution/requestApproval"
        request(request_id, method, params)
        while not stop.is_set() and not answer.wait(0.02):
            pass
        response = PENDING.pop(request_id, {}).get("result")
        notify("serverRequest/resolved", {"threadId":thread_id,"requestId":request_id})
        if stop.is_set() or (scenario == "approval" and (response or {}).get("decision") not in ("accept", "acceptForSession")):
            finish_turn(thread, turn, "interrupted")
            return
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

    if stop.wait(float(os.environ.get("BEX_FAKE_STREAM_DELAY_SECONDS", "2"))):
        finish_turn(thread, turn, "interrupted")
        return
    completed_command = dict(base_items[-1], aggregatedOutput="passed", status="completed", exitCode=0)
    turn["items"][3] = completed_command
    complete_item(thread_id, turn, completed_command)
    if "[groups]" in prompt:
        stream_item(thread_id, turn, {
            "id": f"fixture-progress-{suffix}", "type": "agentMessage",
            "text": "最初の確認が終わりました。次のコマンドを確認します。", "phase": "commentary",
        })
        stream_item(thread_id, turn, {
            "id": f"fixture-next-command-{suffix}", "type": "commandExecution",
            "command": "pwd", "aggregatedOutput": "GROUP_DETAIL_OUTPUT", "status": "completed", "exitCode": 0,
        })
        if stop.wait(float(os.environ.get("BEX_FAKE_STREAM_DELAY_SECONDS", "2"))):
            finish_turn(thread, turn, "interrupted")
            return

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

    if scenario == "followups":
        for item in [
            {"id": "history-answer-1", "type": "agentMessage", "text": "Earlier answer remains visible."},
            {"id": "history-followup-1", "type": "userMessage", "content": [{"type": "text", "text": "Next question"}]},
            {"id": "history-answer-2", "type": "agentMessage", "phase": "commentary", "text": "Reply before the next instruction."},
            {"id": "history-followup-2", "type": "userMessage", "content": [{"type": "text", "text": "One more question"}]},
        ]:
            stream_item(thread_id, turn, item)
            complete_item(thread_id, turn, item)
    final_item = {
        "id": f"fixture-final-{suffix}",
        "type": "agentMessage",
        "text": "",
        "phase": "final_answer",
    }
    response_text = (
        "ローカル relay 構成で Mac・iPhone アプリの実装と検証を完了しました。\n\n"
        "- **Mac アプリ**：会話、リモート操作、添付・保存・差分を確認。\n"
        "- iPhone Simulator：**10/10 成功、スキップ 0**。\n"
        "- SwiftUI の会話表示と入力欄を更新しました。\n\n"
        "変更したファイルは、下の差分から確認できます。"
        if scenario == "history" else "シミュレータで完了しました。"
    )
    if image_path is not None:
        response_text = f"Hostの画像です。\n\n![Hostから読み込んだ画像](<{image_path}>)\n\nインライン画像です。\n\n![インライン画像]({image_url})"
    stream_item(thread_id, turn, final_item)
    if "[long-markdown]" in prompt:
        chunks = ["```text\n"] + ["Markdown stream fixture " * 80 + "\n" for _ in range(36)] + ["\n```\n\n**MARKDOWN_STREAM_COMPLETE**"]
        for chunk in chunks:
            if stop.wait(0.25):
                return
            final_item["text"] += chunk
            notify("item/agentMessage/delta", {
                "threadId": thread_id, "turnId": turn_id,
                "itemId": final_item["id"], "delta": chunk,
            })
        complete_item(thread_id, turn, final_item)
        finish_turn(thread, turn, "completed")
        return
    notify(
        "item/agentMessage/delta",
        {
            "threadId": thread_id,
            "turnId": turn_id,
            "itemId": final_item["id"],
            "delta": response_text,
        },
    )
    final_item["text"] = response_text
    complete_item(thread_id, turn, final_item)
    finish_turn(thread, turn, "completed")
    if scenario == "duplicate":
        thread["turns"].append({
            "id": turn_id, "status": "completed", "items": [
                {"id": "duplicate-history-old", "type": "agentMessage", "phase": "final_answer",
                 "text": "Older AI response must remain visible."},
            ],
        })


def run_server() -> int:
    threads: dict[str, dict[str, object]] = {}
    threads_before_list_fixture = None
    list_fixture_contents = None
    expected_cwd = os.environ.get("BEX_FAKE_CODEX_EXPECTED_CWD")
    next_thread_number = 0

    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if method is None and message.get("id") in PENDING:
            pending = PENDING[message["id"]]
            pending["result"] = message.get("result")
            pending["event"].set()
            continue
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
        elif method == "model/list":
            respond(request_id, result={"data": [{
                "id": "fixture-model", "model": "fixture-model", "displayName": "Fixture Model",
                "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [
                    {"reasoningEffort": "medium", "description": "Balanced"},
                    {"reasoningEffort": "high", "description": "Detailed"},
                ], "isDefault": True, "hidden": False, "description": "Isolated test model",
            }], "nextCursor": None})
        elif method == "thread/list":
            fixture = Path(os.environ.get("CODEX_HOME", "/tmp")) / "list-fixture.json"
            contents = fixture.read_text() if fixture.exists() else None
            if contents is not None and contents != list_fixture_contents:
                if threads_before_list_fixture is None:
                    threads_before_list_fixture = threads
                list_fixture_contents = contents
                threads = {}
                for summary in json.loads(contents):
                    thread = thread_value(summary["id"], summary["cwd"])
                    thread.update(summary)
                    thread["turns"] = [{"id": f"turn-{thread['id']}", "status": "completed", "items": [
                        {"id": f"answer-{thread['id']}", "type": "agentMessage", "phase": "final_answer",
                         "text": f"History for {thread['name']}"},
                    ]}]
                    threads[thread["id"]] = thread
            elif not fixture.exists() and threads_before_list_fixture is not None:
                threads = threads_before_list_fixture
                threads_before_list_fixture = None
                list_fixture_contents = None
            if fixture.exists() and params.get("useStateDbOnly") is not True:
                respond(request_id, error={"code": -32602, "message": "Title lists must not scan rollout history"})
                continue
            candidates = threads.values()
            if params.get("searchTerm"):
                term = params["searchTerm"].lower()
                candidates = [thread for thread in candidates if term in (thread.get("name") or thread.get("preview", "")).lower()]
            ordered = sorted(candidates, key=lambda thread: thread["updatedAt"], reverse=True)
            offset = int(params.get("cursor") or 0)
            end = offset + min(int(params.get("limit") or 64), 100)
            page = [dict(thread, turns=[]) for thread in ordered[offset:end]]
            trace(method, useStateDbOnly=params.get("useStateDbOnly") is True, count=len(page))
            respond(request_id, result={"data": page,
                                       "nextCursor": str(end) if end < len(ordered) else None})
        elif method == "thread/start":
            has_project_id = "projectId" in params
            cwd = params.get("cwd", os.getcwd())
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
            thread["model"] = params.get("model")
            thread["createdAt"] = next_thread_number
            thread["updatedAt"] = next_thread_number
            threads[thread["id"]] = thread
            respond(request_id, result={"thread": thread})
        elif method == "turn/start":
            thread_id = params.get("threadId")
            inputs = params.get("input")
            has_text_input = (
                isinstance(inputs, list)
                and len(inputs) >= 1
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
            if "[model]" in inputs[0]["text"] and (
                params.get("model") != "fixture-model" or params.get("effort") != "high"
                or threads[thread_id].get("model") != "fixture-model"
            ):
                respond(request_id, error={"code": -32602, "message": "selected model and effort did not reach the server"})
                continue
            if any(turn["status"] == "inProgress" for turn in threads[thread_id]["turns"]):
                respond(request_id,error={"code":-32600,"message":"turn already active"})
                continue
            suffix = f"{str(thread_id).rsplit('-',1)[-1]}-{len(threads[thread_id]['turns'])+1}"
            turn_id = f"fixture-turn-{suffix}"
            turn = {
                "id": turn_id,
                "status": "inProgress",
                "items": [],
                "startedAt": 1,
            }
            threads[thread_id]["turns"].append(turn)
            threads[thread_id]["status"] = {"type": "active", "activeFlags": []}
            respond(request_id, result={"turn": {"id": turn_id}})
            stop = threading.Event()
            CONTROLS[turn_id] = stop
            if any(tag in inputs[0]["text"] for tag in ("[delayed-input]", "[deferred-steer]")):
                DELAYED_INPUT_TURNS.add(turn_id)
                (Path(os.environ["CODEX_HOME"]) / "release-inputs").unlink(missing_ok=True)
            threading.Thread(target=run_turn_scenario,args=(threads[thread_id],turn,inputs,suffix,stop,params.get("clientUserMessageId")),daemon=True).start()
        elif method in ("turn/interrupt", "turn/steer"):
            thread = threads.get(params.get("threadId"))
            active = next((turn for turn in (thread or {}).get("turns",[]) if turn["status"] == "inProgress"),None)
            expected = params.get("turnId" if method == "turn/interrupt" else "expectedTurnId")
            if active is None or active["id"] != expected:
                respond(request_id,error={"code":-32602,"message":"active turn changed"})
            elif method == "turn/interrupt":
                CONTROLS[active["id"]].set()
                respond(request_id,result={})
            else:
                item={"id":f"steered-{params.get('clientUserMessageId') or len(active['items'])}","clientId":params.get("clientUserMessageId"),"type":"userMessage","content":params.get("input",[])}
                if active["id"] in DELAYED_INPUT_TURNS:
                    threading.Thread(target=deliver_deferred_input, args=(thread["id"],active,item,CONTROLS[active["id"]]), daemon=True).start()
                else:
                    stream_item(thread["id"],active,item)
                respond(request_id,result={"turnId":active["id"]})
        elif method in ("thread/turns/list", "thread/items/list"):
            thread = threads.get(params.get("threadId"))
            if thread is None:
                respond(request_id, error={"code": -32602, "message": "thread not found"})
                continue
            thread = persisted_thread(thread)
            if method == "thread/turns/list":
                values = [dict(turn, items=[]) if params.get("itemsView") == "notLoaded" else turn
                          for turn in thread["turns"]]
            else:
                values = [{"turnId": turn["id"], "item": item} for turn in thread["turns"]
                          if turn["id"] == params.get("turnId") for item in turn["items"]]
            if params.get("sortDirection") == "desc":
                values.reverse()
            offset = int(params.get("cursor") or 0)
            end = offset + int(params.get("limit") or 10)
            respond(request_id, result={"data": values[offset:end], "nextCursor": str(end) if end < len(values) else None,
                                       "backwardsCursor": None})
        elif method in ("thread/read", "thread/resume"):
            external = Path(os.environ.get("CODEX_HOME", "/tmp")) / "background-reply"
            if external.exists() and threads:
                latest = max(threads.values(), key=lambda thread: thread["createdAt"])
                latest["turns"].append({
                    "id": "fixture-external-turn", "status": "completed",
                    "items": [{"id": "fixture-external-final", "type": "agentMessage",
                               "phase": "final_answer", "text": external.read_text(encoding="utf-8")}],
                })
                external.unlink()
            thread = threads.get(params.get("threadId"))
            if thread is None:
                respond(request_id, error={"code": -32602, "message": "thread not found"})
            elif method == "thread/read" and params.get("includeTurns") and thread.get("historyMode") == "paginated":
                respond(request_id, error={"code": -32603, "message": "Full history hydration is unavailable; use pagination"})
            elif method == "thread/read" and params.get("includeTurns") and thread["turns"] and all(
                turn["status"] == "inProgress" and not any(item["type"] == "agentMessage" for item in turn["items"])
                for turn in thread["turns"]
            ):
                respond(request_id, error={"code": -32603, "message": "rollout is empty"})
            elif method == "thread/resume" and not thread["turns"]:
                respond(request_id, error={"code": -32600, "message": "no rollout found for thread id"})
            else:
                if params.get("includeTurns"):
                    thread = persisted_thread(thread)
                respond(request_id, result={"thread": dict(thread, turns=[]) if method == "thread/read" and not params.get("includeTurns") else thread})
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
