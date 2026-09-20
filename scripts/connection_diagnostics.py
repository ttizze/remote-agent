#!/usr/bin/env python3
"""Read automatically collected Dev traces. Never infer one-way time from wall clocks."""

import argparse
import json
from pathlib import Path


def fields(row):
    result = {}
    for part in row["message"].split():
        if "=" in part:
            key, value = part.split("=", 1)
            result[key] = int(value) if value.isdecimal() and key != "client_revision" else value
    return result


def load_rows(path):
    rows = []
    for file in [*(path.parent / f"{path.name}.{n}" for n in range(4, 0, -1)), path]:
        if not file.exists():
            continue
        for line in file.read_text().splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if row.get("operation", "").startswith(("client.connection", "host.connection", "host.rpc.performance")):
                rows.append(row)
    return sorted(rows, key=lambda row: row["timestampMs"])


def packet_key(event):
    if not event.get("packet_valid") or not event.get("space"):
        return None
    return tuple(event[key] for key in ("group", "space", "path", "packet"))


def unique_packets(events, phase):
    values, duplicate = {}, set()
    for event in events:
        key = packet_key(event)
        if event["phase"] == phase and key is not None:
            if key in values:
                duplicate.add(key)
            values[key] = event
    return {key: value for key, value in values.items() if key not in duplicate}


def match_packets(client, host):
    pairs = []
    for direction, sender, receiver in [("client_to_host", client, host), ("host_to_client", host, client)]:
        sent = unique_packets(sender, "QuicPacketSent")
        received = unique_packets(receiver, "QuicPacketReceived")
        for key in sent.keys() & received.keys():
            pairs.append({"direction": direction, "key": key, "sent_us": sent[key]["source_at_us"], "received_us": received[key]["source_at_us"]})
    return pairs


def clock_bounds(pairs):
    """Host clock minus client clock, bounded by nonnegative delivery delay."""
    upper = [p["received_us"] - p["sent_us"] for p in pairs if p["direction"] == "client_to_host"]
    lower = [p["sent_us"] - p["received_us"] for p in pairs if p["direction"] == "host_to_client"]
    if not upper or not lower:
        return {"status": "insufficient_bidirectional_packets"}
    low, high = max(lower), min(upper)
    if low > high:
        return {"status": "inconsistent_clock_bounds", "lower_us": low, "upper_us": high}
    return {"status": "bounded", "lower_us": low, "upper_us": high, "uncertainty_us": high - low}


def delivery_bounds(pair, bounds):
    if bounds["status"] != "bounded":
        return None
    delta = pair["received_us"] - pair["sent_us"]
    if pair["direction"] == "client_to_host":
        return [delta - bounds["upper_us"], delta - bounds["lower_us"]]
    return [delta + bounds["lower_us"], delta + bounds["upper_us"]]


def relay_summary(events):
    relays = []
    groups = {e["group"] for e in events if e["phase"].startswith("Relay")}
    for group in sorted(groups):
        selected = [e for e in events if e["group"] == group]
        ping = {e["value"]: e for e in selected if e["phase"] == "RelayPingSent"}
        pongs = [e for e in selected if e["phase"] == "RelayPongReceived" and e["value"] in ping]
        metrics = {}
        names = ["rtt_us", "smoothed_rtt_us", "rto_us", "send_buffer_bytes", "tx_bytes", "rx_bytes", "retransmitted_bytes", "out_of_order_bytes", "retransmitted_packets", "congestion_window_bytes", "send_window_bytes"]
        for kind, name in enumerate(names, 1):
            samples = [e for e in selected if e["phase"] == "RelayTcpMetric" and e["kind"] == kind]
            if samples:
                metrics[name] = {"first": samples[0]["value"], "last": samples[-1]["value"], "max": max(e["value"] for e in samples), "samples": len(samples)}
        relays.append({"group": group, "ping_rtt_us": [e["at_us"]-ping[e["value"]]["at_us"] for e in pongs], "tcp": metrics, "socket_probe_errors": [e["value"] for e in selected if e["phase"] == "RelayTcpInfoUnavailable"]})
    return relays


def analyze(rows, platform):
    headers = [r for r in rows if r["operation"] == "client.connection.timeline" and fields(r).get("platform") == platform]
    if not headers:
        raise ValueError(f"No {platform} connection timeline received")
    latest = headers[-1]
    metadata = fields(latest)
    same_process = [r for r in rows if r["pid"] == latest["pid"]]
    client = {fields(r)["seq"]: fields(r) for r in same_process if r["operation"] == "client.connection.event" and fields(r)["trace"] == metadata["trace"]}
    client = sorted(client.values(), key=lambda e: e["seq"])
    links = [fields(r) for r in same_process if r["operation"] == "host.connection.link" and fields(r).get("trace") == metadata["trace"] and fields(r).get("connection") == metadata["connection"]]
    sessions = {r["session"] for r in links}
    rpc = [fields(r) for r in same_process if r["operation"] == "host.rpc.performance" and fields(r).get("session") in sessions]
    host_traces = {r["trace"] for r in rpc if "trace" in r}
    host = {(fields(r)["trace"], fields(r)["seq"]): fields(r) for r in same_process if r["operation"] == "host.connection.event" and fields(r)["trace"] in host_traces}
    host = sorted(host.values(), key=lambda e: e["at_us"])
    start = next((e["at_us"] for e in client if e["phase"] == "ResumeStart" and e["group"] == metadata["attempt"]), 0)
    attempt_client = [e for e in client if e["at_us"] >= start]
    quic_ids = {e["stream"] for e in client if e["phase"] == "QuicTraceLinked" and e["group"] == metadata["connection"]}
    relevant_client = [e for e in attempt_client if e["group"] in quic_ids]
    relevant_host = [e for e in host if e["group"] in quic_ids]
    pairs = match_packets(relevant_client, relevant_host)
    bounds = clock_bounds(pairs) if len(host_traces) == 1 else {"status": "multiple_or_missing_host_clocks"}
    for pair in pairs:
        pair["delivery_interval_us"] = delivery_bounds(pair, bounds)
    requests = []
    for sent in client:
        if sent["phase"] != "RequestSent" or sent["group"] != metadata["connection"] or sent["at_us"] < start:
            continue
        stream = sent["stream"]
        app = [e for e in client if e["group"] == metadata["connection"] and e["stream"] == stream and e["at_us"] >= sent["at_us"]]
        received = [e for e in relevant_client if e["phase"] == "QuicFrameReceived" and e["kind"] == 1 and e["stream"] == stream]
        server = next((r for r in rpc if r["stream"] == stream), {})
        item = {"stream": stream, "method": server.get("method"), "host": server}
        for phase in ["ReadWake", "ReadPolled", "ResponseFirstRead", "ResponseReceived", "ResponseDecoded"]:
            event = next((e for e in app if e["phase"] == phase), None)
            item[phase + "_after_send_us"] = event["at_us"] - sent["at_us"] if event else None
        if received:
            first = min(received, key=lambda e: e["at_us"])
            item["first_quic_frame_after_send_us"] = first["source_at_us"] - sent["at_us"]
            item["quic_observer_delay_us"] = first["at_us"] - first["source_at_us"]
            read = next((e for e in app if e["phase"] == "ResponseFirstRead"), None)
            item["quic_frame_to_first_read_us"] = read["at_us"] - first["source_at_us"] if read else None
            pair = next((p for p in pairs if p["direction"] == "host_to_client" and p["key"] == packet_key(first)), None)
            item["first_response_packet_delivery_us"] = pair["delivery_interval_us"] if pair else None
        requests.append(item)
    pulses = {}
    host_interval = None
    if bounds["status"] == "bounded" and attempt_client:
        host_interval = (start + bounds["lower_us"], attempt_client[-1]["at_us"] + bounds["upper_us"])
    attempt_host = [e for e in host if host_interval and host_interval[0] <= e["at_us"] <= host_interval[1]]
    for side, events in [("client", attempt_client), ("host", attempt_host)]:
        for phase in ["RuntimePulse"]:
            values = [e["value"] for e in events if e["phase"] == phase]
            pulses[f"{side}_{phase}"] = {"count": len(values), "max_gap_us": max(values) if values else None}
    return {"metadata": metadata, "host_revision": latest["revision"], "clock_offset": bounds, "requests": requests, "client_relays": relay_summary(attempt_client), "host_relays": relay_summary(attempt_host), "pulses": pulses, "packets": pairs, "client_events": client, "host_events": host}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", type=Path, default=Path.home()/"Library/Application Support/app.bex.BEX-Dev/logs/host.jsonl")
    parser.add_argument("--platform", default="Ios")
    args = parser.parse_args()
    print(json.dumps(analyze(load_rows(args.log), args.platform), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
