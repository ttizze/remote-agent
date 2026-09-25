use agent_transport::diagnostics;
use serde_json::Value;
use std::fs;

#[test]
fn transport_failures_survive_without_payloads_or_a_log_storm() {
    let directory = tempfile::tempdir().unwrap();
    diagnostics::initialize(
        directory.path(),
        diagnostics::Component::Host,
        "network-test",
    )
    .unwrap();
    // These are the event shapes emitted by the locked iroh/netwatch/noq
    // versions, including message-less link changes and formatted OS errors.
    tracing::event!(target: "iroh::_events::link_change", tracing::Level::DEBUG,
        is_major = true, state = ?"PRIVATE_INTERFACES");
    let error = std::io::Error::from_raw_os_error(48);
    for _ in 0..1_000 {
        tracing::warn!(target: "iroh::socket::transports", peer = "PRIVATE_PEER",
            "failed to rebind {:?}", error);
        tracing::warn!(target: "netwatch::udp", "socket closed");
    }
    // A different error must not be hidden by the repeat limiter.
    tracing::warn!(target: "iroh::socket::transports", "failed to rebind {:?}",
        std::io::Error::from_raw_os_error(49));
    tracing::error!(target: "noq::endpoint", "I/O error: socket closed");
    tracing::error!(target: "noq::connection", "I/O error: PRIVATE_ERROR (os error 24)");
    tracing::warn!(target: "netwatch::udp", "PRIVATE_UNRELATED_WARNING");
    tracing::error!(target: "iroh::socket::transports", "PRIVATE_UNRELATED_ERROR");
    tracing::warn!(target: "other_dependency", "failed to rebind PRIVATE_DATA");
    tracing::trace!(target: "iroh::socket::transports::ip",
        "rebound from {} to {}", "PRIVATE_OLD_ADDRESS", "PRIVATE_NEW_ADDRESS");
    // Recovery must allow the next failure to be recorded immediately.
    tracing::warn!(target: "netwatch::udp", "socket closed");
    tracing::event!(target: "iroh::_events::link_change", tracing::Level::DEBUG,
        is_major = false, state = ?"PRIVATE_INTERFACES");
    tracing::warn!(target: "netwatch::udp", "socket closed");

    let contents = fs::read_to_string(directory.path().join("logs/host.jsonl")).unwrap();
    assert!(!contents.contains("PRIVATE_"), "{contents}");
    let records: Vec<Value> = contents
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let operations: Vec<_> = records
        .iter()
        .map(|r| r["operation"].as_str().unwrap())
        .collect();
    assert_eq!(
        operations,
        [
            "startup",
            "network.change",
            "network.socket.rebind_failed",
            "network.socket.closed",
            "network.socket.rebind_failed",
            "network.endpoint.io_error",
            "network.connection.io_error",
            "network.socket.rebound",
            "network.socket.closed",
            "network.change",
            "network.socket.closed",
        ]
    );
    assert_eq!(records[2]["errorCode"], 48);
    assert_eq!(records[4]["errorCode"], 49);
    assert!(
        records[4]["message"]
            .as_str()
            .unwrap()
            .ends_with("previous_suppressed=999")
    );
    assert_eq!(records[6]["errorCode"], 24);
    assert_eq!(records[6]["level"], "error");
    assert_eq!(
        records[7]["message"],
        "UDP socket rebound previous_suppressed=999"
    );
    assert_eq!(records[9]["message"], "major=false previous_suppressed=0");

    tracing::warn!(target: "iroh::socket::transports", "All transports failed to receive. QUIC endpoint will be shutdown.");
    let contents = fs::read_to_string(directory.path().join("logs/host.jsonl")).unwrap();
    let last: Value = serde_json::from_str(contents.lines().last().unwrap()).unwrap();
    assert_eq!(last["operation"], "network.endpoint.receive_failed");

    tracing::warn!(target: "iroh::socket::transports::ip",
        "failed to rebind IP transport: {:?}", error);
    let contents = fs::read_to_string(directory.path().join("logs/host.jsonl")).unwrap();
    let last: Value = serde_json::from_str(contents.lines().last().unwrap()).unwrap();
    assert_eq!(last["operation"], "network.socket.rebind_failed");
    assert_eq!(last["errorCode"], 48);
}
