//! Normalize the pinned transport dependencies' health events; never retain their payloads.
use regex::Regex;
use std::sync::LazyLock;
use tracing::{Level, Metadata, field::Visit};

#[derive(PartialEq, Eq)]
pub(super) struct Event {
    pub operation: &'static str,
    pub message: String,
    pub error_code: Option<i32>,
}

pub(super) fn enabled(metadata: &Metadata<'_>) -> bool {
    match metadata.target() {
        "iroh::_events::link_change" => *metadata.level() == Level::DEBUG,
        "iroh::socket::transports::ip" => *metadata.level() == Level::TRACE,
        "iroh::socket::transports" => *metadata.level() <= Level::WARN,
        "netwatch::udp" => matches!(*metadata.level(), Level::WARN | Level::DEBUG),
        "noq::endpoint" | "noq::connection" => *metadata.level() == Level::ERROR,
        _ => false,
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    major: Option<bool>,
}

impl Visit for Fields {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "is_major" {
            self.major = Some(value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.into();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

pub(super) fn decode(event: &tracing::Event<'_>) -> Option<Event> {
    let mut fields = Fields::default();
    event.record(&mut fields);
    let target = event.metadata().target();
    let operation = match target {
        "iroh::_events::link_change" => {
            return Some(Event {
                operation: "network.change",
                message: format!("major={}", fields.major?),
                error_code: None,
            });
        }
        "iroh::socket::transports" if fields.message.starts_with("failed to rebind ") => {
            "network.socket.rebind_failed"
        }
        "netwatch::udp" if fields.message.starts_with("failed to rebind UDP socket: ") => {
            "network.socket.rebind_failed"
        }
        "iroh::socket::transports"
            if fields.message
                == "All transports failed to receive. QUIC endpoint will be shutdown." =>
        {
            return Some(Event {
                operation: "network.endpoint.receive_failed",
                message: "All transports failed; QUIC endpoint will shut down".into(),
                error_code: None,
            });
        }
        _ if (target == "netwatch::udp" && fields.message == "UDP socket rebound")
            || (target == "iroh::socket::transports::ip"
                && fields.message.starts_with("rebound from ")) =>
        {
            return Some(Event {
                operation: "network.socket.rebound",
                message: "UDP socket rebound".into(),
                error_code: None,
            });
        }
        "netwatch::udp" if fields.message == "socket closed" => "network.socket.closed",
        "noq::endpoint" if fields.message.starts_with("I/O error: ") => "network.endpoint.io_error",
        "noq::connection" if fields.message.starts_with("I/O error: ") => {
            "network.connection.io_error"
        }
        _ => return None,
    };
    // ErrorKind names and numeric OS codes are useful without persisting debug
    // messages, peer addresses, interface details, URLs, or dependency fields.
    static OS_CODE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:\bcode: |\bos error )(-?\d+)\b").unwrap());
    let error_code = OS_CODE
        .captures(&fields.message)
        .and_then(|captures| captures[1].parse::<i32>().ok());
    let cause = if fields.message.contains("socket closed") {
        "SocketClosed"
    } else {
        [
            "AddrInUse",
            "AddrNotAvailable",
            "PermissionDenied",
            "NotConnected",
            "BrokenPipe",
            "NetworkDown",
            "NetworkUnreachable",
            "HostUnreachable",
            "OutOfMemory",
            "WouldBlock",
            "TimedOut",
            "Interrupted",
        ]
        .into_iter()
        .find(|kind| fields.message.contains(&format!("kind: {kind},")))
        .unwrap_or("Unclassified")
    };
    Some(Event {
        operation,
        message: format!("source={target} cause={cause}"),
        error_code,
    })
}
