//! Command output has an ephemeral sequence, independent of saved row cursors.
//! The Host sends ordered appends or a terminal checkpoint for a slow viewer.
use serde::{Deserialize, Serialize};
mod terminal;

pub const TAIL_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Output {
    pub sequence: u64,
    pub text: String,
    pub truncated: bool,
    buffer: std::sync::Arc<terminal::Buffer>,
}

// The wire checkpoint contains the terminal resume string once; text and the
// incremental screen are local projections of it.
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    sequence: u64,
    resume: String,
    truncated: bool,
}
impl Serialize for Output {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Checkpoint {
            sequence: self.sequence,
            resume: self.buffer.resume(),
            truncated: self.truncated,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Output {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Checkpoint::deserialize(deserializer)?;
        let mut output = Self::new(value.sequence, &value.resume);
        output.truncated |= value.truncated;
        Ok(output)
    }
}
impl PartialEq for Output {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
            && self.truncated == other.truncated
            && self.text == other.text
    }
}
impl Eq for Output {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputUpdate {
    Tail(Output),
    Append { through: u64, text: String },
}

impl Output {
    pub fn new(sequence: u64, text: &str) -> Self {
        let mut buffer = terminal::Buffer::default();
        buffer.append(text);
        let (text, truncated) = buffer.display();
        Self {
            sequence,
            text,
            truncated,
            buffer: std::sync::Arc::new(buffer),
        }
    }
    /// T3 bounds the raw final snapshot to four tails before interpreting it.
    /// Live appends retain the already folded checkpoint instead.
    pub fn persisted(text: &str) -> Self {
        let mut units = 0;
        let mut start = text.len();
        for (index, ch) in text.char_indices().rev() {
            if units + ch.len_utf16() > 4 * TAIL_CHARS {
                break;
            }
            units += ch.len_utf16();
            start = index;
        }
        let truncated = start > 0;
        if truncated
            && let Some(newline) = text[start..].find('\n')
            && start + newline + 1 < text.len()
        {
            start += newline + 1;
        }
        let mut output = Self::new(0, &text[start..]);
        output.truncated |= truncated;
        output
    }
    pub fn append(&mut self, through: u64, text: &str) {
        if through <= self.sequence {
            return;
        }
        std::sync::Arc::make_mut(&mut self.buffer).append(text);
        let (text, truncated) = self.buffer.display();
        self.sequence = through;
        self.text = text;
        self.truncated |= truncated;
    }
    pub fn apply(&self, update: OutputUpdate) -> Self {
        match update {
            OutputUpdate::Tail(tail) => {
                if tail.sequence < self.sequence {
                    return self.clone();
                }
                tail
            }
            OutputUpdate::Append { through, text } => {
                let mut next = self.clone();
                next.append(through, &text);
                next
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn live_appends_interpret_only_new_terminal_text() {
        let mut stream = Output::default();
        stream.append(
            1,
            &"verbose command line with output and columns................................\n"
                .repeat(1000),
        );
        let before = terminal::PRINTED_UNITS.get();
        stream.append(2, "next line\n");
        assert_eq!(terminal::PRINTED_UNITS.get() - before, "next line".len());
        assert!(stream.text.ends_with("next line\n"));
    }

    fn checkpoint(output: Output) -> Output {
        crate::protocol::decode(&crate::protocol::encode(output).unwrap()).unwrap()
    }

    #[test]
    fn duplicate_updates_and_older_checkpoints_do_not_change_history() {
        let output = Output::new(3, "before");
        let next = output.apply(OutputUpdate::Tail(Output::new(6, "current")));
        let appended = next.apply(OutputUpdate::Append {
            through: 8,
            text: "!".into(),
        });
        assert_eq!(appended.text, "current!");
        assert_eq!(appended.apply(OutputUpdate::Tail(output)), appended);
        assert_eq!(
            appended.apply(OutputUpdate::Append {
                through: 8,
                text: "!".into()
            }),
            appended
        );
    }
    #[test]
    fn tails_use_utf16_units_and_keep_unicode_and_color_boundaries() {
        let cjk = Output::new(0, &"あ".repeat(TAIL_CHARS + 1));
        assert_eq!(cjk.text.chars().count(), TAIL_CHARS);
        assert!(cjk.truncated);
        let emoji = Output::new(0, &"😀".repeat(TAIL_CHARS));
        assert_eq!(emoji.text.encode_utf16().count(), TAIL_CHARS);
        assert!(!emoji.text.contains('\u{fffd}'));
        let colored = Output::new(0, &("\x1b[31m".to_owned() + &"a".repeat(TAIL_CHARS + 100)));
        assert!(colored.text.starts_with("\x1b[0;31m"));
        assert!(colored.text.ends_with("\x1b[0m"));
    }
    #[test]
    fn terminal_edits_and_style_survive_late_subscription() {
        for (source, expected) in [
            ("loading 1%\rloading 99%\x1b[K", "loading 99%"),
            ("one\ntwo\x1b[1A\x1b[1GONE", "ONE\ntwo"),
            ("abc\x1b[2D!", "a!c"),
            (
                "red\x1b]8;;https://example.invalid\x1b\\link\x1b]8;;\x1b\\",
                "redlink",
            ),
            ("visible\x1b]hidden\nnext", "visible\nnext"),
        ] {
            let tail = Output::new(1, source);
            assert_eq!(tail.text, expected, "{source:?}");
            let restored = Output::default().apply(OutputUpdate::Tail(tail.clone()));
            let update = OutputUpdate::Append {
                through: 2,
                text: "!".into(),
            };
            assert_eq!(restored.apply(update.clone()), tail.apply(update));
        }
    }
    #[test]
    fn terminal_output_matches_the_t3_reference_corpus_after_each_chunk_and_reconnect() {
        fn unpack(runs: &serde_json::Value) -> String {
            runs.as_array()
                .unwrap()
                .iter()
                .map(|run| {
                    run[0]
                        .as_str()
                        .unwrap()
                        .repeat(run[1].as_u64().unwrap() as usize)
                })
                .collect()
        }
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("output/terminal-cases.json")).unwrap();
        for case in corpus["cases"].as_array().unwrap() {
            let mut output = Output::default();
            let mut stream = Output::default();
            for step in case["steps"].as_array().unwrap() {
                let text = step["text"]
                    .as_str()
                    .unwrap()
                    .repeat(step["repeat"].as_u64().unwrap() as usize);
                output = output.apply(OutputUpdate::Append {
                    through: output.sequence + 1,
                    text: text.clone(),
                });
                stream.append(output.sequence, &text);
                assert_eq!(stream, output, "{} host checkpoint", case["name"]);
                assert_eq!(output.text, unpack(&step["expected"]), "{}", case["name"]);
                assert_eq!(
                    output.truncated,
                    step["truncated"].as_bool().unwrap(),
                    "{}",
                    case["name"]
                );
            }
            let restored = Output::default().apply(OutputUpdate::Tail(checkpoint(output.clone())));
            let after = restored.apply(OutputUpdate::Append {
                through: restored.sequence + 1,
                text: "!".into(),
            });
            assert_eq!(
                after.text,
                unpack(&case["after"]),
                "{} after reconnect",
                case["name"]
            );
        }
        for case in corpus["persisted"].as_array().unwrap() {
            let output = Output::persisted(&unpack(&case["chunks"]));
            assert_eq!(output.text, unpack(&case["expected"]));
            assert_eq!(output.truncated, case["truncated"].as_bool().unwrap());
        }
    }
    #[test]
    fn final_snapshots_bound_the_raw_source_before_folding() {
        let source = "old scrollback\n".to_owned() + &"x\r".repeat(200_000) + "done";
        let output = Output::persisted(&source);
        assert_eq!(output.text, "done");
        assert!(output.truncated);
    }
    proptest! {
        #[test]
        fn checkpoints_and_arbitrary_chunk_boundaries_fold_identically(parts in prop::collection::vec("[a-zあ😀\\n\\r\\x08]{0,80}", 0..35), checkpoints in prop::collection::vec(any::<bool>(), 1..40)) {
            let mut source = String::new();
            let mut host = Output::default();
            let mut client = Output::default();
            for (index, part) in parts.into_iter().enumerate() {
                source.push_str(&part);
                let update = OutputUpdate::Append { through: host.sequence + 1, text: part };
                host = host.apply(update.clone());
                let update = if checkpoints[index % checkpoints.len()] { OutputUpdate::Tail(checkpoint(host.clone())) } else { update };
                client = client.apply(update.clone());
                prop_assert_eq!(&client, &host);
                prop_assert_eq!(client.apply(update), client.clone());
                prop_assert_eq!(host.text.clone(), Output::new(host.sequence, &source).text);
            }
        }
    }
}
