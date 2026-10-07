use super::*;
use proptest::prelude::*;

const UNLIMITED: usize = usize::MAX;

fn limits(lines: usize, bytes: usize) -> Limits {
    Limits { lines, bytes }
}

/// The newest `lines` lines, then the longest tail of whole characters within
/// `bytes`.
fn retained(text: &str, lines: usize, bytes: usize) -> String {
    let terminated = text.ends_with('\n');
    let mut parts: Vec<&str> = text.split('\n').collect();
    if terminated {
        parts.pop();
    }
    let kept = parts[parts.len().saturating_sub(lines)..].join("\n");
    let capped = if terminated {
        format!("{kept}\n")
    } else {
        kept
    };
    if capped.len() <= bytes {
        return capped;
    }
    let mut start = capped.len();
    for (index, _) in capped.char_indices().rev() {
        if capped.len() - index > bytes {
            break;
        }
        start = index;
    }
    capped[start..].to_owned()
}

const FRAGMENTS: [&str; 12] = [
    "",
    "a",
    "\n",
    "\n\n",
    "\r",
    "\r\n",
    "café",
    "名",
    "🚀",
    "\x1b[31m",
    "\x1b[0m",
    "\x1b]8;;url\x07",
];

proptest! {
    // Manager.test.ts "preserves line and byte limits across arbitrary chunks,
    // Unicode, ANSI sequences, and clear".
    #[test]
    fn keeps_the_newest_lines_and_bytes_across_any_chunks_and_clears(
        steps in proptest::collection::vec(
            prop_oneof![
                8 => (0..FRAGMENTS.len(), 0..FRAGMENTS.len()).prop_map(Some),
                1 => Just(None),
            ],
            0..120,
        ),
    ) {
        for bytes in [0, 3, 8, 64, UNLIMITED] {
            for lines in [0, 1, 3, 5, 5_000] {
                let initial = "before\ninitial\n";
                let mut expected = retained(initial, lines, bytes);
                let mut text = Bounded::new(limits(lines, bytes), initial);
                prop_assert_eq!(text.value(), expected.clone());
                for step in &steps {
                    match step {
                        Some((first, second)) => {
                            let chunk = format!("{}{}", FRAGMENTS[*first], FRAGMENTS[*second]);
                            text.append(&chunk);
                            expected = retained(&(expected + &chunk), lines, bytes);
                        }
                        None => {
                            text.clear();
                            expected = String::new();
                        }
                    }
                    prop_assert_eq!(text.value(), expected.clone(), "{} lines, {} bytes", lines, bytes);
                }
            }
        }
    }
}

// Manager.test.ts "bounds long partial lines and joins surrogate pairs across
// chunk boundaries": a long line is cut by bytes on a character boundary, and a
// character split between output chunks is kept whole.
#[test]
fn bounds_long_partial_lines_and_joins_characters_split_between_chunks() {
    let bytes = 65_539;
    let mut history = History::new(limits(5_000, bytes), "");
    let mut expected = String::new();
    let rocket = "🚀".as_bytes();
    let writes: [Vec<u8>; 4] = [
        format!("{}😀{}", "a".repeat(16_383), "b".repeat(70_000)).into_bytes(),
        [format!("\r{}", "c".repeat(70_000)).as_bytes(), &rocket[..2]].concat(),
        [&rocket[2..], "d".repeat(100).as_bytes()].concat(),
        format!("\u{feff}{}", "名".repeat(30_000)).into_bytes(),
    ];
    let mut decoded = String::new();
    let mut all = vec![];
    for (index, data) in writes.iter().enumerate() {
        history.record(data);
        if index == 2 {
            assert!(history.value().contains("🚀d"));
        }
        all.extend_from_slice(data);
        let complete = match std::str::from_utf8(&all) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&all[..error.valid_up_to()]).unwrap(),
        };
        expected = retained(&(expected + &complete[decoded.len()..]), 5_000, bytes);
        decoded = complete.to_owned();
        assert_eq!(history.value(), expected);
        assert!(history.value().len() <= bytes);
    }
}

// Manager.test.ts "preserves retained lines as older storage is compacted".
#[test]
fn keeps_the_newest_lines_as_older_text_is_dropped() {
    for lines in [3, 5_000] {
        let mut expected = String::new();
        let mut text = Bounded::new(limits(lines, UNLIMITED), "");
        for batch in 0..40 {
            let chunk: String = (0..300).map(|line| format!("{batch}:{line}\n")).collect();
            text.append(&chunk);
            expected = retained(&(expected + &chunk), lines, UNLIMITED);
            assert_eq!(text.value(), expected);
        }
    }
}

#[test]
fn invalid_output_bytes_become_replacement_characters() {
    let mut history = History::new(Limits::default(), "");
    history.record(b"ok\xff\xe6\x97");
    assert_eq!(history.value(), "ok\u{fffd}");
    history.record(b"\xa5!");
    assert_eq!(history.value(), "ok\u{fffd}日!");
}

/// What a terminal shows when opened again after writing `chunks` and closing:
/// the history read when it opened, its output, the write when it closed and
/// the read when it opens again.
async fn reopened(limits: Limits, chunks: &[&[u8]]) -> (String, String) {
    let directory = tempfile::tempdir().unwrap();
    let files = HistoryFiles::new(directory.path().to_path_buf(), limits);
    let thread = ThreadId::new("thread-1").unwrap();
    let mut history = files.read(&thread, "term-1").await.unwrap();
    for chunk in chunks {
        history.record(chunk);
    }
    let path = files.path(&thread, "term-1");
    HistoryFiles::write(path.clone(), history.value()).await;
    let persisted = std::fs::read_to_string(&path).unwrap();
    (
        persisted,
        files.read(&thread, "term-1").await.unwrap().value(),
    )
}

// Manager.test.ts "caps persisted history to configured line limit".
#[tokio::test]
async fn caps_persisted_history_to_the_line_limit() {
    let (_, history) = reopened(
        limits(3, Limits::default().bytes),
        &[b"line1\nline2\nline3\nline4\n"],
    )
    .await;
    let lines: Vec<&str> = history
        .split('\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, ["line2", "line3", "line4"]);
}

// Manager.test.ts "caps incrementally appended history without losing partial
// or empty lines".
#[tokio::test]
async fn caps_appended_history_without_losing_partial_or_empty_lines() {
    let (_, history) = reopened(
        limits(3, Limits::default().bytes),
        &[b"line1\n", b"\n", b"line3", b"-continued\nline4"],
    )
    .await;
    assert_eq!(history, "\nline3-continued\nline4");
}

// Manager.test.ts "bounds persisted and attached history without truncating
// live output": the kept history is bounded by bytes on a character boundary.
#[tokio::test]
async fn bounds_persisted_history_by_bytes() {
    let (persisted, history) = reopened(
        limits(5, 10),
        &["a".repeat(32).as_bytes(), "😀\rEND".as_bytes()],
    )
    .await;
    assert_eq!(persisted, "aa😀\rEND");
    assert_eq!(history, "aa😀\rEND");
}

// Manager.test.ts "reads only a Unicode-safe tail from oversized %s history".
#[tokio::test]
async fn reads_only_a_whole_character_tail_of_an_oversized_history() {
    let directory = tempfile::tempdir().unwrap();
    let files = HistoryFiles::new(directory.path().to_path_buf(), limits(5, 15));
    let thread = ThreadId::new("thread-1").unwrap();
    let path = files.path(&thread, "term-1");
    std::fs::write(
        &path,
        format!("{}😀\u{feff}newest\ré", "old".repeat(32_768)),
    )
    .unwrap();
    let history = files.read(&thread, "term-1").await.unwrap();
    assert_eq!(history.value(), "\u{feff}newest\ré");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "\u{feff}newest\ré");
    assert_eq!(
        files.read(&thread, "term-1").await.unwrap().value(),
        "\u{feff}newest\ré"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_history_fails_the_open() {
    let directory = tempfile::tempdir().unwrap();
    let files = HistoryFiles::new(directory.path().to_path_buf(), Limits::default());
    let thread = ThreadId::new("thread-1").unwrap();
    std::fs::create_dir(files.path(&thread, "term-1")).unwrap();
    assert_eq!(
        files.read(&thread, "term-1").await.err().unwrap(),
        "Failed to read terminal history for thread: thread-1, terminal: term-1"
    );
}

// Manager.test.ts "strips replay-unsafe terminal query and reply sequences
// from persisted history".
#[tokio::test]
async fn strips_terminal_queries_and_replies_from_the_history() {
    let (_, history) = reopened(
        Limits::default(),
        &[
            b"prompt ",
            b"\x1b[32mok\x1b[0m ",
            b"\x1b]11;rgb:ffff/ffff/ffff\x07",
            b"\x1b[1;1R",
            b"done\n",
        ],
    )
    .await;
    assert_eq!(history, "prompt \x1b[32mok\x1b[0m done\n");
}

// Manager.test.ts "strips replayable CSI and DCS traffic while preserving
// setters".
#[tokio::test]
async fn strips_csi_and_dcs_queries_but_keeps_setters() {
    let (_, history) = reopened(
        Limits::default(),
        &[
            b"prompt ",
            // DECRQM/DECRPM, XTVERSION and kitty keyboard queries and replies.
            b"\x1b[?2026$p\x1b[?2026;2$y\x1b[>q\x1b[?u\x1b[?31u",
            // DECRQSS and XTGETTCAP in 7-bit DCS form.
            b"\x1bP$q m\x1b\\\x1bP1$r0m\x1b\\",
            b"\x1bP+q544e\x1b\\\x1bP1+r544e=1b\x1b\\",
            // The same in 8-bit form.
            "\u{90}$q m\u{9c}\u{90}1$r0m\u{9c}".as_bytes(),
            "\u{90}+q544e\u{9c}\u{90}1+r544e=1b\u{9c}".as_bytes(),
            // Setters that share final bytes with the queries.
            b"\x1b[!p\x1b[\"p\x1b[4 q\x1b[u",
            b"done\n",
        ],
    )
    .await;
    assert_eq!(history, "prompt \x1b[!p\x1b[\"p\x1b[4 q\x1b[udone\n");
}

// Manager.test.ts "handles CSI and DCS query sequences split across output
// chunks".
#[tokio::test]
async fn strips_queries_split_between_output_chunks() {
    let (_, history) = reopened(
        Limits::default(),
        &[
            b"before ",
            b"\x1b[?2026$",
            b"pafter ",
            b"\x1bP$q ",
            b"m\x1b",
            b"\\after ",
            "\u{9b}?3".as_bytes(),
            b"1uafter ",
            "\u{90}+q544e".as_bytes(),
            "\u{9c}after\n".as_bytes(),
        ],
    )
    .await;
    assert_eq!(history, "before after after after after\n");
}

// Manager.test.ts "preserves clear and style control sequences while dropping
// chunk-split query traffic".
#[tokio::test]
async fn keeps_clear_and_style_sequences_while_dropping_split_queries() {
    let (_, history) = reopened(
        Limits::default(),
        &[
            b"before clear\n",
            b"\x1b[H\x1b[2J",
            b"prompt ",
            b"\x1b]11;",
            b"rgb:ffff/ffff/ffff\x07\x1b[1;1",
            b"R\x1b[36mdone\x1b[0m\n",
        ],
    )
    .await;
    assert_eq!(
        history,
        "before clear\n\x1b[H\x1b[2Jprompt \x1b[36mdone\x1b[0m\n"
    );
}

// Manager.test.ts "does not leak final bytes from ESC sequences with
// intermediate bytes" and "preserves chunk-split ESC sequences with
// intermediate bytes without leaking final bytes".
#[tokio::test]
async fn keeps_escape_sequences_with_intermediate_bytes_whole() {
    for chunks in [
        [b"before ".as_slice(), b"\x1b(B", b"after\n"],
        [b"before ".as_slice(), b"\x1b(", b"Bafter\n"],
    ] {
        let (_, history) = reopened(Limits::default(), &chunks).await;
        assert_eq!(history, "before \x1b(Bafter\n");
    }
}

#[test]
fn a_finished_process_drops_its_unfinished_sequence() {
    let mut history = History::new(Limits::default(), "");
    assert!(!history.record(b"\x1b]0;tit"));
    history.end();
    assert!(history.record(b"le\x07"));
    assert_eq!(history.value(), "le\x07");
}

#[test]
fn an_unterminated_control_sequence_is_kept_as_output_past_its_bound() {
    let mut history = History::new(Limits::default(), "");
    assert!(!history.record(b"\x1b]0;"));
    let text = "x".repeat(MAX_UNFINISHED_CONTROL_BYTES);
    assert!(history.record(text.as_bytes()));
    assert!(history.control.is_empty());
    assert_eq!(history.value(), format!("\x1b]0;{text}"));
    assert!(history.record(b"more\x07"));
    assert!(history.control.is_empty());
}
