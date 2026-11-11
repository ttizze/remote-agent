//! A transport-free executable model of the proposed sync contract, not a test
//! of the current production protocol. Host state is the independent oracle.
//! The ordered stream can stop at any message; a new subscription fences old
//! messages and resumes only from state the client actually applied.
mod streaming;

use proptest::prelude::*;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    version: u64,
    value: u8,
}

#[derive(Clone, Debug)]
enum Message {
    Snapshot {
        cursor: u64,
        floor: u64,
        rows: BTreeMap<u64, Row>,
    },
    Changes {
        cursor: u64,
        rows: BTreeMap<u64, Row>,
        deleted: Vec<u64>,
    },
    Extend {
        floor: u64,
        rows: BTreeMap<u64, Row>,
    },
}

#[derive(Default)]
struct Host {
    sequence: u64,
    next_id: u64,
    rows: BTreeMap<u64, Row>,
    deleted: BTreeMap<u64, u64>,
    retained_after: u64,
    floor: u64,
    stream: VecDeque<Message>,
}

#[derive(Default)]
struct Client {
    cursor: u64,
    floor: u64,
    rows: BTreeMap<u64, Row>,
}

impl Host {
    fn change(&mut self, kind: u8, target: u64, value: u8) {
        self.sequence += 1;
        let mut rows = BTreeMap::new();
        let mut deleted = Vec::new();
        match kind {
            0 => {
                let row = Row {
                    version: self.sequence,
                    value,
                };
                self.rows.insert(self.next_id, row.clone());
                if self.next_id >= self.floor {
                    rows.insert(self.next_id, row);
                }
                self.next_id += 1;
            }
            1 => {
                if let Some(row) = self.rows.get_mut(&target) {
                    *row = Row {
                        version: self.sequence,
                        value,
                    };
                    if target >= self.floor {
                        rows.insert(target, row.clone());
                    }
                }
            }
            _ => {
                if self.rows.remove(&target).is_some() {
                    self.deleted.insert(target, self.sequence);
                    if target >= self.floor {
                        deleted.push(target);
                    }
                }
            }
        }
        self.stream.push_back(Message::Changes {
            cursor: self.sequence,
            rows,
            deleted,
        });
    }

    fn range(&self, floor: u64) -> BTreeMap<u64, Row> {
        self.rows
            .range(floor..)
            .map(|(id, row)| (*id, row.clone()))
            .collect()
    }

    fn reconnect(&mut self, cursor: u64, floor: u64) {
        self.stream.clear(); // The old subscription is fenced at transport entry.
        self.floor = floor;
        let message = if cursor < self.retained_after || cursor > self.sequence {
            Message::Snapshot {
                cursor: self.sequence,
                floor,
                rows: self.range(floor),
            }
        } else {
            Message::Changes {
                cursor: self.sequence,
                rows: self
                    .rows
                    .range(floor..)
                    .filter(|(_, row)| row.version > cursor)
                    .map(|(id, row)| (*id, row.clone()))
                    .collect(),
                deleted: self
                    .deleted
                    .iter()
                    .filter(|(_, seq)| **seq > cursor)
                    .map(|(id, _)| *id)
                    .collect(),
            }
        };
        self.stream.push_back(message);
    }

    fn extend(&mut self, floor: u64) {
        let floor = floor.min(self.floor);
        let rows = self
            .rows
            .range(floor..self.floor)
            .map(|(id, row)| (*id, row.clone()))
            .collect();
        self.stream.push_back(Message::Extend { floor, rows });
        self.floor = floor;
    }

    fn expire_deletions(&mut self) {
        self.deleted.clear();
        self.retained_after = self.sequence;
    }
}

impl Client {
    fn upsert(&mut self, rows: BTreeMap<u64, Row>) {
        for (id, row) in rows {
            if self
                .rows
                .get(&id)
                .is_none_or(|old| old.version < row.version)
            {
                self.rows.insert(id, row);
            }
        }
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::Snapshot {
                cursor,
                floor,
                rows,
            } => {
                self.rows = rows;
                self.floor = floor;
                self.cursor = cursor;
            }
            Message::Changes {
                cursor,
                rows,
                deleted,
            } => {
                if cursor <= self.cursor {
                    return;
                }
                self.upsert(rows);
                for id in deleted {
                    self.rows.remove(&id);
                }
                self.cursor = cursor;
            }
            Message::Extend { floor, rows } => {
                self.upsert(rows);
                self.floor = floor;
            }
        }
    }
}

fn deliver(host: &mut Host, client: &mut Client) {
    while let Some(message) = host.stream.pop_front() {
        client.apply(message);
    }
}

proptest! {
    #[test]
    fn ordered_stream_and_reconnect_converge(
        actions in prop::collection::vec((0u8..9, any::<u8>(), any::<u8>()), 0..200),
        initial_floor in 0u64..20,
    ) {
        let mut host = Host { floor: initial_floor, ..Default::default() };
        let mut client = Client { floor: initial_floor, ..Default::default() };
        for (kind, target, value) in actions {
            let target = u64::from(target) % host.next_id.max(1);
            match kind {
                0..=2 => host.change(kind, target, value),
                3 => host.expire_deletions(),
                4 => host.extend(target),
                5 => { if let Some(message) = host.stream.pop_front() { client.apply(message); } }
                6 => host.reconnect(client.cursor, client.floor),
                _ => {
                    deliver(&mut host, &mut client);
                    prop_assert_eq!(&client.rows, &host.range(client.floor));
                    prop_assert_eq!(client.floor, host.floor);
                }
            }
        }
        host.reconnect(client.cursor, client.floor);
        deliver(&mut host, &mut client);
        prop_assert_eq!(client.rows, host.range(client.floor));
    }
}

#[test]
fn expired_deletion_removes_cached_row_on_snapshot() {
    let mut host = Host::default();
    let mut client = Client::default();
    host.change(0, 0, 1);
    deliver(&mut host, &mut client);
    host.change(2, 0, 0);
    host.expire_deletions();
    host.reconnect(client.cursor, client.floor);
    deliver(&mut host, &mut client);
    assert!(client.rows.is_empty());
}

#[test]
fn expansion_updates_host_subscription_before_later_writes() {
    let mut host = Host {
        floor: 5,
        ..Default::default()
    };
    let mut client = Client {
        floor: 5,
        ..Default::default()
    };
    for value in 0..10 {
        host.change(0, 0, value);
    }
    host.extend(2);
    host.change(1, 3, 99);
    host.change(2, 4, 0);
    deliver(&mut host, &mut client);
    assert_eq!(client.rows, host.range(2));
}

#[test]
fn disconnect_before_expansion_delivery_preserves_the_old_scope() {
    let mut host = Host {
        floor: 5,
        ..Default::default()
    };
    let mut client = Client {
        floor: 5,
        ..Default::default()
    };
    for value in 0..10 {
        host.change(0, 0, value);
    }
    deliver(&mut host, &mut client);
    host.extend(2);
    host.change(1, 3, 99);
    host.reconnect(client.cursor, client.floor);
    deliver(&mut host, &mut client);
    assert_eq!(client.floor, 5);
    assert_eq!(client.rows, host.range(5));
}
