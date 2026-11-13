//! Ephemeral command output uses byte offsets in its own ordered stream.
//! A gap asks the owner for a new tail; it never changes the saved row cursor.
use serde::{Deserialize, Serialize};

const TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub offset: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputUpdate {
    Tail(Output),
    Append { offset: u64, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("command output has a gap; request its current tail")]
    Gap,
    #[error("command output byte offset overflow")]
    Overflow,
}

impl Output {
    pub fn apply(&self, update: OutputUpdate) -> Result<Self, OutputError> {
        let current_end = end(self.offset, &self.text)?;
        let mut next = match update {
            OutputUpdate::Tail(tail) => {
                if end(tail.offset, &tail.text)? < current_end {
                    return Ok(self.clone());
                }
                tail
            }
            OutputUpdate::Append { offset, text } => {
                let incoming = end(offset, &text)?;
                if incoming <= current_end {
                    return Ok(self.clone());
                }
                if offset != current_end {
                    return Err(OutputError::Gap);
                }
                let mut next = self.clone();
                next.text.push_str(&text);
                next
            }
        };
        let mut boundary = next.text.len().saturating_sub(TAIL_BYTES);
        while !next.text.is_char_boundary(boundary) {
            boundary += 1;
        }
        next.offset = next
            .offset
            .checked_add(boundary as u64)
            .ok_or(OutputError::Overflow)?;
        next.text.drain(..boundary);
        Ok(next)
    }
}

fn end(offset: u64, text: &str) -> Result<u64, OutputError> {
    offset
        .checked_add(text.len() as u64)
        .ok_or(OutputError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn late_subscription_gap_and_tail_recovery() {
        let output = Output {
            offset: 0,
            text: String::new(),
        };
        let output = output
            .apply(OutputUpdate::Tail(Output {
                offset: 50,
                text: "before".into(),
            }))
            .unwrap();
        assert_eq!(
            output.apply(OutputUpdate::Append {
                offset: 60,
                text: "gap".into()
            }),
            Err(OutputError::Gap)
        );
        let output = output
            .apply(OutputUpdate::Tail(Output {
                offset: 60,
                text: "current".into(),
            }))
            .unwrap();
        let next = output
            .apply(OutputUpdate::Append {
                offset: 67,
                text: "!".into(),
            })
            .unwrap();
        assert_eq!(next.text, "current!");
        let shortened = next
            .apply(OutputUpdate::Tail(Output {
                offset: 67,
                text: "!".into(),
            }))
            .unwrap();
        assert_eq!(
            shortened,
            Output {
                offset: 67,
                text: "!".into()
            }
        );
        assert_eq!(
            next.apply(OutputUpdate::Tail(Output {
                offset: 0,
                text: "old".into()
            }))
            .unwrap(),
            next
        );
        assert_eq!(
            next.apply(OutputUpdate::Append {
                offset: 67,
                text: "!".into()
            })
            .unwrap(),
            next
        );
    }

    #[test]
    fn utf8_tail_is_bounded_and_marks_the_bytes_discarded() {
        let all = "あ".repeat(TAIL_BYTES);
        let output = Output {
            offset: 0,
            text: String::new(),
        }
        .apply(OutputUpdate::Tail(Output {
            offset: 0,
            text: all.clone(),
        }))
        .unwrap();
        assert!(output.text.len() <= TAIL_BYTES);
        assert_eq!(output.offset as usize + output.text.len(), all.len());
        assert!(all.ends_with(&output.text));
        assert_eq!(
            output.apply(OutputUpdate::Append {
                offset: u64::MAX,
                text: "!".into()
            }),
            Err(OutputError::Overflow)
        );
    }

    proptest! {
        #[test]
        fn append_and_tail_sequences_always_show_the_host_suffix(parts in prop::collection::vec((0usize..4096, any::<bool>()), 0..32), tails in prop::collection::vec(any::<bool>(), 1..80)) {
            let mut host = String::new();
            let mut output = Output { offset: 0, text: String::new() };
            let mut previous = None;
            for (index, (length, unicode)) in parts.into_iter().enumerate() {
                let part = if unicode { "あ" } else { "a" }.repeat(length);
                let before = host.len() as u64;
                host.push_str(&part);
                let update = if tails[index % tails.len()] {
                    OutputUpdate::Tail(Output { offset: 0, text: host.clone() })
                } else { OutputUpdate::Append { offset: before, text: part } };
                output = output.apply(update.clone()).unwrap();
                prop_assert_eq!(output.apply(update.clone()).unwrap(), output.clone());
                if let Some(previous) = previous {
                    prop_assert_eq!(output.apply(previous).unwrap(), output.clone());
                }
                previous = Some(update);
                prop_assert!(output.text.len() <= TAIL_BYTES);
                prop_assert!(host.ends_with(&output.text));
                prop_assert_eq!(output.offset + output.text.len() as u64, host.len() as u64);
            }
        }
    }
}
