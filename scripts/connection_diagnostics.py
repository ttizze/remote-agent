#!/usr/bin/env python3
"""Summarize bounded connection captures from the Host; missing values stay missing."""
import argparse
import json
from pathlib import Path


def fields(row):
    return {key: int(value) if value.isdecimal() and key != "client_revision" else value
            for key, value in (part.split("=", 1) for part in row["message"].split() if "=" in part)}


def load_rows(path):
    rows = []
    for file in [*(path.parent / f"{path.name}.{n}" for n in range(4, 0, -1)), path]:
        if file.exists():
            for line in file.read_text().splitlines():
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if row.get("operation", "").startswith(("client.connection", "host.connection", "host.rpc.performance")):
                    rows.append(row)
    return sorted(rows, key=lambda row: row["timestampMs"])


def interval(events, start, end):
    first = None
    for event in events:
        if event["phase"] == start:
            if first is not None:
                return None  # A restart without an end makes this pairing ambiguous.
            first = event
        elif event["phase"] == end and first and event["at_us"] >= first["at_us"]:
            return event["at_us"] - first["at_us"]
    return None


def analyze(rows, platform="Ios", attempt_id=None):
    headers = [r for r in rows if r["operation"] == "client.connection.timeline" and fields(r).get("platform") == platform]
    if not headers:
        raise ValueError(f"No {platform} connection capture received")
    header = headers[-1]
    metadata = fields(header)
    same_process = [r for r in rows if r["pid"] == header["pid"]]
    events = {fields(r)["seq"]: fields(r) for r in same_process
              if r["operation"] == "client.connection.event" and fields(r)["trace"] == metadata["trace"]}
    events = sorted(events.values(), key=lambda e: e["seq"])
    if attempt_id is not None:
        metadata["attempt"] = attempt_id
        selected = next((e for e in events if e["phase"] == "ResumeConnection" and e["group"] == attempt_id), None)
        metadata["connection"] = selected["stream"] if selected else None
    resume = next((e for e in events if e["phase"] == "ResumeStart" and e["group"] == metadata["attempt"]), None)
    start = resume["at_us"] if resume else None
    end = next((e["at_us"] for e in events if e["phase"] == "ResumeStart" and start is not None and e["at_us"] > start), None)
    attempt = [e for e in events if start is not None and e["at_us"] >= start and (end is None or e["at_us"] < end)]
    ui = [e for e in events if e["phase"] == "UiConnectStart" and start is not None and e["at_us"] <= start]
    ui_events = [e for e in events if ui and e["at_us"] >= ui[-1]["at_us"] and (end is None or e["at_us"] < end)]
    summaries = [fields(r) for r in same_process if r["operation"] == "client.connection" and fields(r).get("attempt") == metadata["attempt"]]
    links = [fields(r) for r in same_process if r["operation"] == "host.connection.link"
             and fields(r).get("trace") == metadata["trace"] and fields(r).get("connection") == metadata["connection"]]
    sessions = {r["session"] for r in links}
    host = [fields(r) for r in same_process if r["operation"] == "host.rpc.performance" and fields(r)["session"] in sessions]
    requests = []
    for sent in attempt:
        if sent["phase"] != "RequestSent" or sent["group"] != metadata["connection"]:
            continue
        stream = sent["stream"]
        app = [e for e in attempt if e["group"] == metadata["connection"] and e["stream"] == stream]
        server = next((r for r in host if r["stream"] == stream), None)
        first = next((e for e in app if e["phase"] == "ResponseFirstRead"), None)
        complete = next((e for e in app if e["phase"] == "ResponseReceived"), None)
        wakes = [e for e in app if e["phase"] == "ReadWake"]
        wake_delays = []
        for wake in wakes:
            poll = next((e for e in app if e["phase"] == "ReadPolled" and e["at_us"] >= wake["at_us"]), None)
            if poll:
                wake_delays.append(poll["at_us"] - wake["at_us"])
        host_us = sum(server[key] for key in ("decode_us", "queue_us", "handle_encode_us", "write_us")) if server else None
        response_us = complete["at_us"] - sent["at_us"] if complete else None
        # Includes client work, network and unobserved scheduling; never label this a pure RTT.
        round_trip_us = interval(app, "RequestOpened", "ResponseReceived")
        residual = round_trip_us - host_us if round_trip_us is not None and host_us is not None else None
        requests.append({"stream": stream, "method": server.get("method") if server else None,
                         "host": server, "request_bytes": sent["value"],
                         "slot_wait_us": next((e["value"] for e in app if e["phase"] == "RequestSlotWait"), None),
                         "opened_to_sent_us": interval(app, "RequestOpened", "RequestSent"),
                         "encode_us": next((e["value"] for e in app if e["phase"] == "RequestEncoded"), None),
                         "first_read_after_send_us": first["at_us"] - sent["at_us"] if first else None,
                         "response_after_send_us": response_us, "opened_to_response_us": round_trip_us,
                         "read_to_complete_us": interval(app, "ResponseFirstRead", "ResponseReceived"),
                         "decode_us": next((e["value"] for e in app if e["phase"] == "ResponseDecoded"), None),
                         "max_observed_wake_to_poll_us": max(wake_delays) if wake_delays else None,
                         "host_local_us": host_us, "outside_host_interval_us": residual,
                         "timing_consistent": residual >= 0 if residual is not None else None})
    relays = []
    for dial in (e for e in attempt if e["phase"] == "RelayDialStart"):
        selected = [e for e in attempt if e["group"] == dial["group"]]
        tcp = []
        for began in (e for e in selected if e["phase"] == "RelayTcpStart"):
            candidate = [e for e in selected if e["stream"] == began["stream"]]
            tcp.append({"attempt": began["stream"], "tcp_us": interval(candidate, "RelayTcpStart", "RelayTcpReady")})
        relays.append({"dial": dial["group"], "total_us": interval(selected, "RelayDialStart", "RelayReady"),
                       "pre_tcp_us": interval(selected, "RelayDialStart", "RelayTcpStart"), "tcp": tcp,
                       "tls_us": interval(selected, "RelayTlsStart", "RelayTlsReady"),
                       "post_tls_to_auth_us": interval(selected, "RelayTlsReady", "RelayAuthStart"),
                       "auth_us": interval(selected, "RelayAuthStart", "RelayAuthReady")})
    host_traces = {r["trace"] for r in host if "trace" in r}
    host_events = {(fields(r)["trace"], fields(r)["seq"]): fields(r) for r in same_process
                   if r["operation"] == "host.connection.event" and fields(r)["trace"] in host_traces}
    host_events = sorted(host_events.values(), key=lambda e: (e["trace"], e["seq"]))
    host_headers = [fields(r) for r in same_process if r["operation"] == "host.connection.timeline" and fields(r)["trace"] in host_traces]
    pulse = lambda values: max((e["value"] for e in values if e["phase"] == "RuntimePulse"), default=None)
    return {"metadata": metadata, "host_revision": header["revision"], "summary": summaries[-1] if summaries else None,
            "attempts": [{"id": e["group"], "started_at_us": e["at_us"],
                          "result": next((r["phase"] for r in events if r["group"] == e["group"] and r["phase"] in ("ResumeReady", "ResumeFailed", "ResumeCancelled")), None),
                          "core_elapsed_us": next((r["value"] for r in events if r["group"] == e["group"] and r["phase"] in ("ResumeReady", "ResumeFailed")), None)}
                         for e in events if e["phase"] == "ResumeStart"],
            "resume_start_observed": resume is not None, "client_dropped_total": metadata["dropped"],
            "host_captures": host_headers, "requests": requests, "relays": relays,
            "network_report_us": interval(attempt, "NetworkReportStart", "NetworkReportReady"),
            "ui_connect_us": interval(ui_events, "UiConnectStart", "UiConnectReady"),
            "ui_to_list_state_us": interval(ui_events, "UiConnectStart", "ListViewUpdated"),
            "max_client_runtime_gap_us": pulse(attempt), "max_host_capture_runtime_gap_us": pulse(host_events),
            "client_events": attempt, "host_events": host_events,
            "unobserved": ["one-way network time", "relay internal queues", "TCP retransmits inside relay transport", "GPU presentation"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", type=Path, default=Path.home()/"Library/Application Support/app.bex.BEX-Dev/logs/host.jsonl")
    parser.add_argument("--platform", default="Ios")
    parser.add_argument("--attempt", type=int, help="Inspect an earlier attempt retained in the same capture")
    args = parser.parse_args()
    print(json.dumps(analyze(load_rows(args.log), args.platform, args.attempt), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
