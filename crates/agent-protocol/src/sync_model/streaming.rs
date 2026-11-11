//! A field uses byte offsets. UTF-8 boundaries must additionally be checked by
//! the eventual string implementation. This model uses bytes to isolate order,
//! checkpoint promotion, missing deltas, and recovery from body omission.
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Message {
    Delta {
        base: u64,
        offset: usize,
        bytes: Vec<u8>,
    },
    Saved {
        base: u64,
        version: u64,
        length: usize,
    },
    Resend {
        version: u64,
        saved: Vec<u8>,
        tail: Vec<u8>,
    },
}

#[derive(Default)]
struct Host {
    version: u64,
    saved: Vec<u8>,
    tail: Vec<u8>,
}

#[derive(Default)]
struct Client {
    version: u64,
    saved_length: usize,
    // None means that a resend is required. Keeping an incomplete prefix after
    // a gap would let the UI silently remain stuck on an old answer.
    text: Option<Vec<u8>>,
}

impl Host {
    fn append(&mut self, byte: u8) -> Message {
        let offset = self.saved.len() + self.tail.len();
        self.tail.push(byte);
        Message::Delta {
            base: self.version,
            offset,
            bytes: vec![byte],
        }
    }

    fn save(&mut self) -> Message {
        let base = self.version;
        self.version += 1;
        self.saved.append(&mut self.tail);
        Message::Saved {
            base,
            version: self.version,
            length: self.saved.len(),
        }
    }

    fn resend(&self) -> Message {
        Message::Resend {
            version: self.version,
            saved: self.saved.clone(),
            tail: self.tail.clone(),
        }
    }

    fn text(&self) -> Vec<u8> {
        self.saved.iter().chain(&self.tail).copied().collect()
    }
}

impl Client {
    fn apply(&mut self, message: Message) {
        match message {
            Message::Delta { base, .. } if base < self.version => {}
            Message::Delta {
                base,
                offset,
                bytes,
            } => match &mut self.text {
                Some(text) if base == self.version && offset == text.len() => text.extend(bytes),
                _ => self.text = None,
            },
            Message::Saved {
                base,
                version,
                length,
            } => {
                if base != self.version
                    || length < self.saved_length
                    || self.text.as_ref().is_none_or(|text| text.len() < length)
                {
                    self.text = None;
                }
                self.version = version;
                self.saved_length = length;
            }
            Message::Resend {
                version,
                mut saved,
                tail,
            } => {
                self.version = version;
                self.saved_length = saved.len();
                saved.extend(tail);
                self.text = Some(saved);
            }
        }
    }
}

proptest! {
    #[test]
    fn lost_deltas_and_omitted_bodies_recover(
        actions in prop::collection::vec((0u8..7, any::<u8>()), 0..200),
    ) {
        let mut host = Host::default();
        let mut client = Client::default();
        for (action, byte) in actions {
            match action {
                0 => {
                    let complete = client.text.as_ref() == Some(&host.text());
                    client.apply(host.append(byte));
                    if complete { prop_assert_eq!(client.text.as_ref(), Some(&host.text())); }
                }
                1 => { host.append(byte); } // Ephemeral delivery is lost.
                2 => client.apply(host.save()),
                3 => client.text = None, // Collapsed body or reconnect without cached text.
                4 => {
                    // Non-append edits get a new persisted version and replace
                    // the body. A delta from the old text must not be appended.
                    let old = host.append(byte);
                    host.saved = vec![byte.wrapping_add(1)];
                    host.tail.clear();
                    host.version += 1;
                    client.apply(host.resend());
                    client.apply(old);
                }
                _ => client.apply(host.resend()),
            }
            if let Some(text) = &client.text {
                prop_assert!(host.text().starts_with(text));
            }
        }
        // A visible omitted field requests a resend, ordered before the next delta.
        client.apply(host.resend());
        prop_assert_eq!(client.text.as_ref(), Some(&host.text()));
        client.apply(host.append(42));
        prop_assert_eq!(client.text.as_ref(), Some(&host.text()));
    }
}

#[test]
fn checkpoint_promotes_received_bytes_without_resending_body() {
    let mut host = Host::default();
    let mut client = Client::default();
    client.apply(host.resend());
    client.apply(host.append(1));
    client.apply(host.append(2));
    client.apply(host.save());
    assert_eq!(client.saved_length, 2);
    assert_eq!(client.text, Some(vec![1, 2]));
    client.apply(host.append(3));
    assert_eq!(client.text, Some(vec![1, 2, 3]));
}

#[test]
fn resend_restores_both_saved_and_unsaved_bytes_after_a_gap() {
    let mut host = Host::default();
    let mut client = Client::default();
    client.apply(host.resend());
    host.append(1);
    client.apply(host.append(2));
    assert!(client.text.is_none());
    client.apply(host.save());
    host.append(3);
    client.apply(host.resend());
    client.apply(host.append(4));
    assert_eq!(client.text, Some(vec![1, 2, 3, 4]));
}
