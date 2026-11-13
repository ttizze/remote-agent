//! Command output has an ephemeral sequence, independent of saved row cursors.
//! A gap requests a terminal checkpoint, retaining T3's cursor/style semantics.
use serde::{Deserialize, Serialize};
mod terminal;

pub const TAIL_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub sequence: u64,
    pub text: String,
    pub truncated: bool,
    resume: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputUpdate {
    Tail(Output),
    Append {
        from: u64,
        through: u64,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("command output has a gap; request its current tail")]
    Gap,
}

impl Output {
    pub fn new(sequence: u64, text: &str) -> Self {
        let folded = terminal::fold(text);
        Self {
            sequence,
            text: folded.text,
            resume: folded.resume,
            truncated: folded.truncated,
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
    pub fn apply(&self, update: OutputUpdate) -> Result<Self, OutputError> {
        match update {
            OutputUpdate::Tail(tail) => {
                if tail.sequence < self.sequence {
                    return Ok(self.clone());
                }
                let mut next = Self::new(tail.sequence, &tail.resume);
                next.truncated |= tail.truncated;
                Ok(next)
            }
            OutputUpdate::Append {
                from,
                through,
                text,
            } => {
                if through <= self.sequence {
                    return Ok(self.clone());
                }
                if from != self.sequence || through <= from {
                    return Err(OutputError::Gap);
                }
                let mut next = Self::new(through, &(self.resume.clone() + &text));
                next.truncated |= self.truncated;
                Ok(next)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn sequence_gaps_duplicates_and_checkpoint_recovery_do_not_change_history() {
        let output = Output::new(3, "before");
        assert_eq!(
            output.apply(OutputUpdate::Append {
                from: 4,
                through: 5,
                text: "gap".into()
            }),
            Err(OutputError::Gap)
        );
        let next = output
            .apply(OutputUpdate::Tail(Output::new(6, "current")))
            .unwrap();
        let appended = next
            .apply(OutputUpdate::Append {
                from: 6,
                through: 8,
                text: "!".into(),
            })
            .unwrap();
        assert_eq!(appended.text, "current!");
        assert_eq!(
            appended.apply(OutputUpdate::Tail(output)).unwrap(),
            appended
        );
        assert_eq!(
            appended
                .apply(OutputUpdate::Append {
                    from: 6,
                    through: 8,
                    text: "!".into()
                })
                .unwrap(),
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
            let restored = Output::default()
                .apply(OutputUpdate::Tail(tail.clone()))
                .unwrap();
            let update = OutputUpdate::Append {
                from: 1,
                through: 2,
                text: "!".into(),
            };
            assert_eq!(
                restored.apply(update.clone()).unwrap(),
                tail.apply(update).unwrap()
            );
        }
    }
    #[test]
    fn terminal_output_matches_the_t3_reference_corpus_after_each_chunk_and_reconnect() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("output/terminal-cases.json")).unwrap();
        for case in corpus["cases"].as_array().unwrap() {
            let mut output = Output::default();
            for step in case["steps"].as_array().unwrap() {
                let text = step["text"]
                    .as_str()
                    .unwrap()
                    .repeat(step["repeat"].as_u64().unwrap() as usize);
                output = output
                    .apply(OutputUpdate::Append {
                        from: output.sequence,
                        through: output.sequence + 1,
                        text,
                    })
                    .unwrap();
                assert_eq!(
                    output.text,
                    step["expected"].as_str().unwrap(),
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    output.truncated,
                    step["truncated"].as_bool().unwrap(),
                    "{}",
                    case["name"]
                );
            }
            let restored = Output::default()
                .apply(OutputUpdate::Tail(output.clone()))
                .unwrap();
            let after = restored
                .apply(OutputUpdate::Append {
                    from: restored.sequence,
                    through: restored.sequence + 1,
                    text: "!".into(),
                })
                .unwrap();
            assert_eq!(
                after.text,
                case["after"].as_str().unwrap(),
                "{} after reconnect",
                case["name"]
            );
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
                let update = OutputUpdate::Append { from: host.sequence, through: host.sequence + 1, text: part };
                host = host.apply(update.clone()).unwrap();
                let update = if checkpoints[index % checkpoints.len()] { OutputUpdate::Tail(host.clone()) } else { update };
                client = client.apply(update.clone()).unwrap();
                prop_assert_eq!(&client, &host);
                prop_assert_eq!(client.apply(update).unwrap(), client.clone());
                prop_assert_eq!(host.text.clone(), Output::new(host.sequence, &source).text);
            }
        }
    }
}
