//! Correlate bounded captures without inventing missing timing observations.
use crate::Result;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

type Fields = Map<String, Value>;

struct Row {
    operation: String,
    pid: u64,
    timestamp_ms: u64,
    revision: Value,
    fields: Fields,
}

fn fields(message: &str) -> Fields {
    message
        .split_whitespace()
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| {
            let parsed = if key != "client_revision"
                && !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
            {
                value.parse::<u64>().ok().map(Value::from)
            } else {
                None
            };
            (
                key.to_owned(),
                parsed.unwrap_or_else(|| Value::String(value.to_owned())),
            )
        })
        .collect()
}

fn number(fields: &Fields, name: &str) -> Option<u64> {
    fields.get(name).and_then(Value::as_u64)
}
fn text<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields.get(name).and_then(Value::as_str)
}
fn required(fields: &Fields, name: &str) -> Result<u64> {
    Ok(number(fields, name).ok_or_else(|| format!("missing capture field {name}"))?)
}

fn load_rows(path: &Path) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let paths = (1..=4)
        .rev()
        .map(|index| {
            let mut path = path.as_os_str().to_owned();
            path.push(format!(".{index}"));
            PathBuf::from(path)
        })
        .chain(std::iter::once(path.to_owned()));
    for path in paths {
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for line in contents.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(operation) = row["operation"].as_str() else {
                continue;
            };
            if ![
                "client.connection",
                "host.connection",
                "host.rpc.performance",
            ]
            .iter()
            .any(|prefix| operation.starts_with(prefix))
            {
                continue;
            }
            rows.push(Row {
                operation: operation.to_owned(),
                pid: row["pid"].as_u64().ok_or("missing capture PID")?,
                timestamp_ms: row["timestampMs"]
                    .as_u64()
                    .ok_or("missing capture timestamp")?,
                revision: row["revision"].clone(),
                fields: fields(row["message"].as_str().ok_or("missing capture message")?),
            });
        }
    }
    rows.sort_by_key(|row| row.timestamp_ms);
    Ok(rows)
}

fn interval(events: &[&Fields], start: &str, end: &str) -> Option<u64> {
    let mut first = None;
    for event in events {
        if text(event, "phase") == Some(start) {
            if first.is_some() {
                return None;
            }
            first = number(event, "at_us");
        } else if text(event, "phase") == Some(end)
            && let (Some(first), Some(last)) = (first, number(event, "at_us"))
            && last >= first
        {
            return Some(last - first);
        }
    }
    None
}

fn analyze(
    rows: &[Row],
    platform: &str,
    attempt_id: Option<u64>,
    trace_id: Option<u64>,
) -> Result<Value> {
    let headers = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.operation == "client.connection.timeline"
                && text(&row.fields, "platform") == Some(platform)
                && trace_id.is_none_or(|trace| number(&row.fields, "trace") == Some(trace))
        })
        .map(|(index, row)| Ok((required(&row.fields, "started_at_ms")?, index, row)))
        .collect::<Result<Vec<_>>>()?;
    let header = headers
        .into_iter()
        .max_by_key(|(started, index, _)| (*started, *index))
        .ok_or_else(|| format!("No {platform} connection capture received"))?
        .2;
    let same_process = rows
        .iter()
        .filter(|row| row.pid == header.pid)
        .collect::<Vec<_>>();
    let mut metadata = header.fields.clone();
    let trace = required(&metadata, "trace")?;
    let mut unique = BTreeMap::new();
    for row in &same_process {
        if row.operation == "client.connection.event" && number(&row.fields, "trace") == Some(trace)
        {
            unique.insert(required(&row.fields, "seq")?, &row.fields);
        }
    }
    let events = unique.into_values().collect::<Vec<_>>();
    if let Some(attempt) = attempt_id {
        metadata.insert("attempt".to_owned(), attempt.into());
        let selected = events.iter().find(|event| {
            text(event, "phase") == Some("ResumeConnection")
                && number(event, "group") == Some(attempt)
        });
        metadata.insert(
            "connection".to_owned(),
            json!(selected.and_then(|event| number(event, "stream"))),
        );
    }
    let attempt_id = required(&metadata, "attempt")?;
    let connection = number(&metadata, "connection");
    let resume = events.iter().find(|event| {
        text(event, "phase") == Some("ResumeStart") && number(event, "group") == Some(attempt_id)
    });
    let start = resume.and_then(|event| number(event, "at_us"));
    let end = events
        .iter()
        .find(|event| {
            text(event, "phase") == Some("ResumeStart")
                && start.is_some_and(|start| number(event, "at_us").is_some_and(|at| at > start))
        })
        .and_then(|event| number(event, "at_us"));
    let in_attempt = |event: &&Fields| {
        start.is_some_and(|start| {
            number(event, "at_us").is_some_and(|at| at >= start && end.is_none_or(|end| at < end))
        })
    };
    let attempt = events
        .iter()
        .copied()
        .filter(in_attempt)
        .collect::<Vec<_>>();
    let ui = events
        .iter()
        .rev()
        .find(|event| {
            text(event, "phase") == Some("UiConnectStart")
                && start.is_some_and(|start| number(event, "at_us").is_some_and(|at| at <= start))
        })
        .and_then(|event| number(event, "at_us"));
    let ui_events = events
        .iter()
        .copied()
        .filter(|event| {
            ui.is_some_and(|ui| {
                number(event, "at_us").is_some_and(|at| at >= ui && end.is_none_or(|end| at < end))
            })
        })
        .collect::<Vec<_>>();
    let summaries = same_process
        .iter()
        .filter(|row| {
            row.operation == "client.connection"
                && number(&row.fields, "attempt") == Some(attempt_id)
        })
        .map(|row| &row.fields)
        .collect::<Vec<_>>();
    let sessions = same_process
        .iter()
        .filter(|row| {
            row.operation == "host.connection.link"
                && number(&row.fields, "trace") == Some(trace)
                && connection.is_some()
                && number(&row.fields, "connection") == connection
        })
        .map(|row| required(&row.fields, "session"))
        .collect::<Result<BTreeSet<_>>>()?;
    let host = same_process
        .iter()
        .filter(|row| {
            row.operation == "host.rpc.performance"
                && number(&row.fields, "session").is_some_and(|session| sessions.contains(&session))
        })
        .map(|row| &row.fields)
        .collect::<Vec<_>>();
    let value_at = |events: &[&Fields], phase| {
        events
            .iter()
            .find(|event| text(event, "phase") == Some(phase))
            .and_then(|event| number(event, "value"))
    };
    let mut requests = Vec::new();
    for sent in &attempt {
        if text(sent, "phase") != Some("RequestSent")
            || connection.is_none()
            || number(sent, "group") != connection
        {
            continue;
        }
        let stream = required(sent, "stream")?;
        let sent_at = required(sent, "at_us")?;
        let app = attempt
            .iter()
            .copied()
            .filter(|event| {
                number(event, "group") == connection && number(event, "stream") == Some(stream)
            })
            .collect::<Vec<_>>();
        let server = host
            .iter()
            .find(|server| number(server, "stream") == Some(stream))
            .copied();
        let phase_at = |phase| {
            app.iter()
                .find(|event| text(event, "phase") == Some(phase))
                .and_then(|event| number(event, "at_us"))
        };
        let first = phase_at("ResponseFirstRead");
        let complete = phase_at("ResponseReceived");
        let wakes = app
            .iter()
            .filter(|event| text(event, "phase") == Some("ReadWake"))
            .filter_map(|wake| {
                let wake = number(wake, "at_us")?;
                app.iter()
                    .find(|event| {
                        text(event, "phase") == Some("ReadPolled")
                            && number(event, "at_us").is_some_and(|at| at >= wake)
                    })
                    .and_then(|poll| number(poll, "at_us"))
                    .map(|poll| poll - wake)
            })
            .collect::<Vec<_>>();
        let host_us = server
            .map(|server| {
                ["decode_us", "queue_us", "handle_encode_us", "write_us"]
                    .into_iter()
                    .map(|key| required(server, key))
                    .try_fold(0u64, |total, value| {
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                            total.checked_add(value?).ok_or("host timing overflow")?,
                        )
                    })
            })
            .transpose()?;
        let round_trip = interval(&app, "RequestOpened", "ResponseReceived");
        let residual = round_trip
            .zip(host_us)
            .map(|(round_trip, host_us)| i128::from(round_trip) - i128::from(host_us));
        let timing_consistent = residual.map(|value| value >= 0);
        let residual = residual.map(serde_json::to_value).transpose()?;
        let first_after_send = first
            .map(|at| serde_json::to_value(i128::from(at) - i128::from(sent_at)))
            .transpose()?;
        let response_after_send = complete
            .map(|at| serde_json::to_value(i128::from(at) - i128::from(sent_at)))
            .transpose()?;
        requests.push(json!({"stream": stream, "method": server.and_then(|server| server.get("method")), "host": server, "request_bytes": sent.get("value"),
            "slot_wait_us": value_at(&app, "RequestSlotWait"), "opened_to_sent_us": interval(&app, "RequestOpened", "RequestSent"), "encode_us": value_at(&app, "RequestEncoded"),
            "first_read_after_send_us": first_after_send, "response_after_send_us": response_after_send, "opened_to_response_us": round_trip,
            "read_to_complete_us": interval(&app, "ResponseFirstRead", "ResponseReceived"), "decode_us": value_at(&app, "ResponseDecoded"), "max_observed_wake_to_poll_us": wakes.into_iter().max(),
            "host_local_us": host_us, "outside_host_interval_us": residual, "timing_consistent": timing_consistent}));
    }
    let mut relays = Vec::new();
    for dial in attempt
        .iter()
        .filter(|event| text(event, "phase") == Some("RelayDialStart"))
    {
        let group = required(dial, "group")?;
        let selected = attempt
            .iter()
            .copied()
            .filter(|event| number(event, "group") == Some(group))
            .collect::<Vec<_>>();
        let tcp = selected.iter().filter(|event| text(event, "phase") == Some("RelayTcpStart")).map(|began| {
            let stream = required(began, "stream")?;
            let candidate = selected.iter().copied().filter(|event| number(event, "stream") == Some(stream)).collect::<Vec<_>>();
            Ok(json!({"attempt": stream, "tcp_us": interval(&candidate, "RelayTcpStart", "RelayTcpReady")}))
        }).collect::<Result<Vec<_>>>()?;
        relays.push(json!({"dial": group, "total_us": interval(&selected, "RelayDialStart", "RelayReady"), "pre_tcp_us": interval(&selected, "RelayDialStart", "RelayTcpStart"), "tcp": tcp,
            "tls_us": interval(&selected, "RelayTlsStart", "RelayTlsReady"), "post_tls_to_auth_us": interval(&selected, "RelayTlsReady", "RelayAuthStart"), "auth_us": interval(&selected, "RelayAuthStart", "RelayAuthReady")}));
    }
    let traces = host
        .iter()
        .filter_map(|record| number(record, "trace"))
        .collect::<BTreeSet<_>>();
    let mut host_events = BTreeMap::new();
    for row in &same_process {
        if row.operation == "host.connection.event"
            && number(&row.fields, "trace").is_some_and(|trace| traces.contains(&trace))
        {
            host_events.insert(
                (
                    required(&row.fields, "trace")?,
                    required(&row.fields, "seq")?,
                ),
                &row.fields,
            );
        }
    }
    let host_events = host_events.into_values().collect::<Vec<_>>();
    let host_headers = same_process
        .iter()
        .filter(|row| {
            row.operation == "host.connection.timeline"
                && number(&row.fields, "trace").is_some_and(|trace| traces.contains(&trace))
        })
        .map(|row| &row.fields)
        .collect::<Vec<_>>();
    let pulse = |events: &[&Fields]| {
        events
            .iter()
            .filter(|event| text(event, "phase") == Some("RuntimePulse"))
            .filter_map(|event| number(event, "value"))
            .max()
    };
    let attempts = events.iter().filter(|event| text(event, "phase") == Some("ResumeStart")).map(|event| {
        let group = number(event, "group");
        let result = events.iter().find(|result| number(result, "group") == group && matches!(text(result, "phase"), Some("ResumeReady" | "ResumeFailed" | "ResumeCancelled"))).and_then(|result| text(result, "phase"));
        let elapsed = events.iter().find(|result| number(result, "group") == group && matches!(text(result, "phase"), Some("ResumeReady" | "ResumeFailed"))).and_then(|result| number(result, "value"));
        json!({"id": group, "started_at_us": event.get("at_us"), "result": result, "core_elapsed_us": elapsed})
    }).collect::<Vec<_>>();
    Ok(
        json!({"metadata": metadata, "host_revision": header.revision, "summary": summaries.last(), "attempts": attempts,
        "resume_start_observed": resume.is_some(), "client_dropped_total": header.fields.get("dropped"), "host_captures": host_headers, "requests": requests, "relays": relays,
        "network_report_us": interval(&attempt, "NetworkReportStart", "NetworkReportReady"), "ui_connect_us": interval(&ui_events, "UiConnectStart", "UiConnectReady"), "ui_to_list_state_us": interval(&ui_events, "UiConnectStart", "ListViewUpdated"),
        "max_client_runtime_gap_us": pulse(&attempt), "max_host_capture_runtime_gap_us": pulse(&host_events), "client_events": attempt, "host_events": host_events,
        "unobserved": ["one-way network time", "relay internal queues", "TCP retransmits inside relay transport", "GPU presentation"]}),
    )
}

pub fn run(arguments: &[String]) -> Result<()> {
    let mut log = None;
    let mut platform = "Ios";
    let mut attempt = None;
    let mut trace = None;
    let mut arguments = arguments.iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or("connection-diagnostics expects flag/value pairs")?;
        match flag.as_str() {
            "--log" => log = Some(PathBuf::from(value)),
            "--platform" => platform = value,
            "--attempt" => attempt = Some(value.parse()?),
            "--trace" => trace = Some(value.parse()?),
            _ => return Err(format!("unknown diagnostic option {flag}").into()),
        }
    }
    let log = log
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library/Application Support/app.bex.BEX-Dev/logs/host.jsonl")
            })
        })
        .ok_or("Pass --log or set HOME")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&analyze(&load_rows(&log)?, platform, attempt, trace)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn row(operation: &str, message: &str, pid: u64) -> Row {
        Row {
            operation: operation.into(),
            pid,
            timestamp_ms: 1,
            revision: "fixture".into(),
            fields: fields(message),
        }
    }

    #[test]
    fn recovered_old_capture_cannot_replace_latest_or_invent_observations() {
        let rows = vec![
            row(
                "client.connection.timeline",
                "trace=2 attempt=20 connection=3 dropped=0 platform=Ios started_at_ms=200 recovered=false",
                1,
            ),
            row(
                "client.connection.timeline",
                "trace=1 attempt=10 connection=0 dropped=0 platform=Ios started_at_ms=100 recovered=true",
                1,
            ),
            row(
                "client.connection.event",
                "trace=1 seq=1 phase=ResumeStart group=10 stream=0 at_us=5 value=0",
                1,
            ),
            row(
                "client.connection.event",
                "trace=1 seq=2 phase=ResumeFailed group=10 stream=0 at_us=2005 value=2000",
                1,
            ),
        ];
        assert_eq!(
            analyze(&rows, "Ios", None, None).unwrap()["metadata"]["trace"],
            2
        );
        let recovered = analyze(&rows, "Ios", None, Some(1)).unwrap();
        assert_eq!(recovered["attempts"][0]["result"], "ResumeFailed");
        assert_eq!(recovered["attempts"][0]["core_elapsed_us"], 2000);
        assert!(recovered["summary"].is_null());
        assert_eq!(recovered["host_events"], json!([]));
    }

    #[test]
    fn correlation_requires_matching_process_connection_and_stream() {
        let mut rows = vec![
            row(
                "client.connection.timeline",
                "trace=1 attempt=2 connection=3 dropped=0 platform=Ios client_revision=123 started_at_ms=1 recovered=false",
                1,
            ),
            row("host.connection.link", "trace=1 connection=3 session=4", 1),
            row(
                "host.rpc.performance",
                "session=4 stream=8 method=scope decode_us=2 queue_us=3 handle_encode_us=4 write_us=1",
                1,
            ),
            row(
                "host.rpc.performance",
                "session=4 stream=8 method=wrong decode_us=200 queue_us=300 handle_encode_us=400 write_us=1",
                2,
            ),
        ];
        for (seq, (phase, group, stream, at)) in [
            ("ResumeStart", 2, 0, 10),
            ("RequestOpened", 3, 8, 18),
            ("RequestSent", 3, 8, 20),
            ("ResponseFirstRead", 3, 8, 110),
            ("ResponseReceived", 3, 8, 120),
            ("RequestSent", 9, 8, 130),
        ]
        .into_iter()
        .enumerate()
        {
            rows.push(row("client.connection.event", &format!("trace=1 seq={seq} phase={phase} group={group} stream={stream} at_us={at} value=10"), 1));
        }
        let result = analyze(&rows, "Ios", None, None).unwrap();
        assert_eq!(result["requests"].as_array().unwrap().len(), 1);
        let request = &result["requests"][0];
        assert_eq!(request["method"], "scope");
        assert_eq!(request["first_read_after_send_us"], 90);
        assert_eq!(request["outside_host_interval_us"], 92);
        assert!(request["max_observed_wake_to_poll_us"].is_null());
        assert!(result["ui_to_list_state_us"].is_null());
        assert_eq!(result["metadata"]["client_revision"], "123");
    }

    #[test]
    fn missing_start_and_reply_are_not_zero_latency() {
        let mut rows = vec![
            row(
                "client.connection.timeline",
                "trace=1 attempt=2 connection=3 dropped=100 platform=Ios client_revision=fixture started_at_ms=1 recovered=false",
                1,
            ),
            row(
                "client.connection.event",
                "trace=1 seq=101 phase=RequestSent group=3 stream=8 at_us=10 value=5",
                1,
            ),
        ];
        let result = analyze(&rows, "Ios", None, None).unwrap();
        assert_eq!(result["resume_start_observed"], false);
        assert_eq!(result["client_dropped_total"], 100);
        assert_eq!(result["requests"], json!([]));
        assert!(result["max_client_runtime_gap_us"].is_null());
        rows.push(row(
            "client.connection.event",
            "trace=1 seq=100 phase=ResumeStart group=2 stream=0 at_us=1 value=0",
            1,
        ));
        let result = analyze(&rows, "Ios", None, None).unwrap();
        for key in [
            "host_local_us",
            "response_after_send_us",
            "outside_host_interval_us",
        ] {
            assert!(result["requests"][0][key].is_null());
        }
    }

    proptest! {
        #[test]
        fn interval_is_translation_invariant_and_rejects_restarted_pairs(start in 0u64..100000, elapsed in 0u64..100000, offset in 0u64..100000) {
            let first = fields(&format!("phase=start at_us={start}")); let last = fields(&format!("phase=end at_us={}", start + elapsed));
            let shifted_first = fields(&format!("phase=start at_us={}", start + offset)); let shifted_last = fields(&format!("phase=end at_us={}", start + elapsed + offset));
            prop_assert_eq!(interval(&[&first, &last], "start", "end"), interval(&[&shifted_first, &shifted_last], "start", "end"));
            prop_assert_eq!(interval(&[&first, &last], "start", "end"), Some(elapsed));
            prop_assert_eq!(interval(&[&first, &shifted_first, &shifted_last], "start", "end"), None);
        }
    }

    #[test]
    fn rotated_logs_are_ordered_and_duplicate_events_use_latest_observation() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("host.jsonl");
        let event = |timestamp, message| json!({"operation": "client.connection.event", "pid": 1, "timestampMs": timestamp, "message": message});
        fs::write(
            directory.path().join("host.jsonl.4"),
            format!(
                "{}\ninvalid json\n{{\"operation\":\"unrelated\"}}\n",
                event(2, "trace=1 seq=1 phase=ResumeStart group=2 at_us=10")
            ),
        )
        .unwrap();
        fs::write(&log, format!("{}\n{}\n", json!({"operation": "client.connection.timeline", "pid": 1, "timestampMs": 1, "revision": "fixture", "message": "trace=1 attempt=2 connection=3 platform=Ios started_at_ms=1 dropped=0"}), event(3, "trace=1 seq=1 phase=ResumeStart group=2 at_us=20"))).unwrap();
        let rows = load_rows(&log).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.timestamp_ms).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let result = analyze(&rows, "Ios", None, None).unwrap();
        assert_eq!(result["client_events"].as_array().unwrap().len(), 1);
        assert_eq!(result["client_events"][0]["at_us"], 20);
    }

    #[test]
    fn earlier_attempt_has_its_own_connection_ui_window_and_observed_wake_delays() {
        let mut rows = vec![
            row(
                "client.connection.timeline",
                "trace=1 attempt=3 connection=9 dropped=0 platform=Ios started_at_ms=1",
                1,
            ),
            row("host.connection.link", "trace=1 connection=4 session=5", 1),
            row(
                "host.rpc.performance",
                "session=5 stream=8 trace=6 decode_us=40 queue_us=30 handle_encode_us=20 write_us=10",
                1,
            ),
            row("host.connection.timeline", "trace=6 dropped=1", 1),
            row(
                "host.connection.event",
                "trace=6 seq=1 phase=RuntimePulse value=7",
                1,
            ),
            row(
                "host.connection.event",
                "trace=99 seq=2 phase=RuntimePulse value=900",
                1,
            ),
        ];
        for (seq, (phase, group, stream, at, value)) in [
            ("UiConnectStart", 0, 0, 5, 0),
            ("ResumeStart", 2, 0, 10, 0),
            ("ResumeConnection", 2, 4, 11, 0),
            ("RequestOpened", 4, 9, 11, 0),
            ("RequestOpened", 4, 8, 12, 0),
            ("RequestSent", 4, 8, 13, 40),
            ("ResponseReceived", 9, 8, 14, 0),
            ("ReadWake", 4, 8, 15, 0),
            ("ReadPolled", 4, 8, 18, 0),
            ("ResponseFirstRead", 4, 8, 19, 0),
            ("ResponseReceived", 4, 8, 22, 0),
            ("RuntimePulse", 0, 0, 23, 6),
            ("UiConnectReady", 0, 0, 24, 0),
            ("ResumeReady", 2, 0, 25, 15),
            ("ListViewUpdated", 0, 0, 26, 0),
            ("ResumeStart", 3, 0, 30, 0),
            ("RuntimePulse", 0, 0, 31, 99),
        ]
        .into_iter()
        .enumerate()
        {
            rows.push(row("client.connection.event", &format!("trace=1 seq={seq} phase={phase} group={group} stream={stream} at_us={at} value={value}"), 1));
        }
        let result = analyze(&rows, "Ios", Some(2), None).unwrap();
        assert!(
            result["client_events"]
                .as_array()
                .unwrap()
                .iter()
                .all(|event| event["at_us"].as_u64().unwrap() < 30)
        );
        assert_eq!(result["metadata"]["connection"], 4);
        assert_eq!(result["ui_connect_us"], 19);
        assert_eq!(result["ui_to_list_state_us"], 21);
        assert_eq!(result["requests"][0]["max_observed_wake_to_poll_us"], 3);
        assert_eq!(result["requests"][0]["outside_host_interval_us"], -90);
        assert_eq!(result["requests"][0]["timing_consistent"], false);
        assert_eq!(result["max_client_runtime_gap_us"], 6);
        assert_eq!(result["max_host_capture_runtime_gap_us"], 7);
        assert_eq!(result["host_captures"].as_array().unwrap().len(), 1);
        assert_eq!(result["attempts"][0]["result"], "ResumeReady");
        assert_eq!(result["attempts"][0]["core_elapsed_us"], 15);
        let missing = analyze(&rows, "Ios", Some(99), None).unwrap();
        assert!(missing["metadata"]["connection"].is_null());
        assert_eq!(missing["requests"], json!([]));
    }
}
