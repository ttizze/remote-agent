//! Independent Host truth with a controllably interrupted delivery queue.
use super::*;
use std::collections::{BTreeMap, VecDeque};

struct Part {
    position: u64,
    version: u64,
    value: u16,
}
struct Group {
    version: u64,
    parts: BTreeMap<u64, Part>,
}
struct Host {
    at: Cursor,
    pruned: u64,
    history: u64,
    next_turn: u64,
    next_item: u64,
    position: u64,
    groups: BTreeMap<u64, Group>,
    removed: Vec<Removal<String>>,
    window: Window,
    pending: VecDeque<Message<String, String>>,
    generation: u64,
}

impl Host {
    fn new() -> Self {
        Self {
            at: cursor(0),
            pruned: 0,
            history: 0,
            next_turn: 0,
            next_item: 0,
            position: 0,
            groups: BTreeMap::new(),
            removed: vec![],
            window: Window { item: 0, turn: 0 },
            pending: VecDeque::new(),
            generation: 1,
        }
    }

    fn rows(&self, floor: Window) -> Vec<Row<String, String>> {
        let mut rows = vec![];
        for (ordinal, group) in self.groups.range(floor.turn..) {
            let parent = format!("turn-{ordinal}");
            rows.push(turn(&parent, group.version, *ordinal));
            for (id, part) in &group.parts {
                if *ordinal > floor.turn || part.position >= floor.item {
                    let mut row = item(
                        &format!("item-{id}"),
                        part.version,
                        part.position,
                        &parent,
                        *ordinal,
                    );
                    row.content = Content::Full(part.value.to_string());
                    rows.push(row);
                }
            }
        }
        rows
    }

    fn write(&mut self, kind: u8, target: u8, value: u16) {
        let from = self.at;
        self.at.sequence += 1;
        let existing = self
            .groups
            .keys()
            .nth(usize::from(target) % self.groups.len().max(1))
            .copied();
        let mut removed = vec![];
        match (kind, existing) {
            (3, Some(ordinal)) => {
                self.groups.remove(&ordinal);
                removed.push(Removal {
                    key: turn(&format!("turn-{ordinal}"), 0, ordinal).key,
                    version: self.at.sequence,
                });
                self.removed.extend(removed.clone());
            }
            (1, Some(ordinal)) if !self.groups[&ordinal].parts.is_empty() => {
                let group = self.groups.get_mut(&ordinal).unwrap();
                let part = group.parts.values_mut().next().unwrap();
                part.version = self.at.sequence;
                part.value = value;
            }
            (_, existing) => {
                let ordinal = if kind == 2 { existing } else { None }.unwrap_or_else(|| {
                    self.next_turn += 1;
                    self.groups.insert(
                        self.next_turn,
                        Group {
                            version: self.at.sequence,
                            parts: BTreeMap::new(),
                        },
                    );
                    self.next_turn
                });
                self.next_item += 1;
                self.position += 1;
                self.groups.get_mut(&ordinal).unwrap().parts.insert(
                    self.next_item,
                    Part {
                        position: self.position,
                        version: self.at.sequence,
                        value,
                    },
                );
            }
        }
        let rows = self
            .rows(self.window)
            .into_iter()
            .filter(|row| row.version == self.at.sequence)
            .collect();
        self.pending.push_back(Message::Changes {
            from,
            through: self.at,
            rows,
            removed,
        });
    }

    fn extend(&mut self, target: u8) {
        let old = self.window;
        self.window.item = self.window.item.min(u64::from(target));
        self.window.turn = self.window.turn.min(u64::from(target));
        self.pending.push_back(Message::Extend {
            cursor: self.at,
            window: self.window,
            rows: self
                .rows(self.window)
                .into_iter()
                .filter(|row| !contains(&scope(), Some(old), &row.key, &row.position))
                .collect(),
        });
    }

    fn replace(&mut self) {
        self.at.sequence += 1;
        self.history = self.at.sequence;
        for group in self.groups.values_mut() {
            group.version = self.at.sequence;
            for part in group.parts.values_mut() {
                part.position += 1000;
                part.version = self.at.sequence;
            }
        }
        self.position += 1000;
        self.snapshot();
    }

    fn snapshot(&mut self) {
        // Simulate a bounded initial window, not all history after a reset.
        self.window = Window {
            item: self.position.saturating_sub(5),
            turn: self.next_turn.saturating_sub(3),
        };
        self.pending.push_back(Message::Snapshot {
            cursor: self.at,
            window: Some(self.window),
            rows: self.rows(self.window),
        });
    }

    fn reconnect(&mut self, client: &mut Replica<String, String>) {
        let stale = self.pending.pop_front();
        self.pending.clear();
        let old_generation = self.generation;
        self.generation += 1;
        *client = client.resubscribe(self.generation).unwrap();
        if let Some(stale) = stale {
            assert_eq!(client.apply(old_generation, stale).unwrap(), *client);
        }
        if can_resume(client.cursor, self.at, self.pruned, self.history) {
            self.window = client.window.unwrap();
            let from = client.cursor.unwrap();
            let rows = self
                .rows(self.window)
                .into_iter()
                .filter(|row| row.version > from.sequence)
                .collect();
            let removed = self
                .removed
                .iter()
                .filter(|row| row.version > from.sequence)
                .cloned()
                .collect();
            self.pending.push_back(Message::Changes {
                from,
                through: self.at,
                rows,
                removed,
            });
        } else {
            self.snapshot();
        }
    }

    fn deliver_one(&mut self, client: &mut Replica<String, String>) {
        if let Some(message) = self.pending.pop_front() {
            *client = client.apply(self.generation, message).unwrap();
        }
    }

    fn deliver_all(&mut self, client: &mut Replica<String, String>) {
        while !self.pending.is_empty() {
            self.deliver_one(client);
        }
    }

    fn restore(&mut self, client: &mut Replica<String, String>) {
        self.groups.clear();
        self.removed.clear();
        self.at.sequence = 0;
        self.pruned = 0;
        self.history = 0;
        // Restoring a previous database state creates a new cursor namespace,
        // even if the restored file contains its original store ID.
        self.at.store = uuid::Uuid::from_u128(self.at.store.as_u128() + 1);
        self.reconnect(client);
    }

    fn act(&mut self, kind: u8, target: u8, value: u16, client: &mut Replica<String, String>) {
        match kind {
            0..=3 => self.write(kind, target, value),
            4 => self.extend(target),
            5 => self.deliver_one(client),
            6 => self.reconnect(client),
            7 => {
                self.removed.clear();
                self.pruned = self.at.sequence;
            }
            8 => self.replace(),
            9 => self.restore(client),
            _ => self.deliver_all(client),
        }
    }
}

proptest! {
    #[test]
    fn windows_reconnect_and_snapshot_fallback_converge(actions in prop::collection::vec((0u8..11, any::<u8>(), any::<u16>()),0..100)) {
        let mut host = Host::new();
        let mut client = empty();
        host.snapshot(); host.deliver_all(&mut client);
        for (kind,target,value) in actions {
            host.act(kind,target,value,&mut client);
            if host.pending.is_empty() {
                prop_assert_eq!(client.window, Some(host.window));
                let truth: BTreeMap<_,_> = host.rows(client.window.unwrap()).into_iter().map(|row| (row.key.clone(),row)).collect();
                prop_assert_eq!(&client.rows,&truth);
                prop_assert_eq!(client.cursor,Some(host.at));
            }
        }
        host.reconnect(&mut client); host.deliver_all(&mut client);
        let truth: BTreeMap<_,_> = host.rows(client.window.unwrap()).into_iter().map(|row| (row.key.clone(),row)).collect();
        prop_assert_eq!(&client.rows,&truth);
        prop_assert_eq!(client.cursor,Some(host.at));
    }
}
