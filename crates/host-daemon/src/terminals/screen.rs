//! Streaming terminal state, query replies, and reconnect checkpoints.
//! The parser and emulator stay together; this module never routes client events
//! or writes to the PTY supervisor.
use agent_protocol::operations::TerminalSize;
use alacritty_terminal::{
    Term,
    event::{Event, WindowSize},
    grid::Dimensions as _,
    term::Config,
    vte::ansi::{Processor, Rgb},
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<alacritty_terminal::event::Event>>>);
impl alacritty_terminal::event::EventListener for Replies {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        use alacritty_terminal::event::Event;
        if matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::ClipboardLoad(..)
        ) {
            self.0.lock().unwrap().push(event);
        }
    }
}
struct Dimensions(TerminalSize);
impl alacritty_terminal::grid::Dimensions for Dimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        self.0.rows as usize
    }
    fn columns(&self) -> usize {
        self.0.cols as usize
    }
}
pub(super) struct TerminalScreen {
    terminal: Term<Replies>,
    parser: Processor,
    replies: Replies,
}

pub(super) struct Checkpoint {
    pub(super) data: Vec<u8>,
    pub(super) size: TerminalSize,
}

impl TerminalScreen {
    pub(super) fn new(size: TerminalSize) -> Self {
        let replies = Replies::default();
        Self {
            terminal: Term::new(Config::default(), &Dimensions(size), replies.clone()),
            parser: Processor::default(),
            replies,
        }
    }

    /// Consume output and drain host-owned query replies exactly once, in order.
    pub(super) fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.parser.advance(&mut self.terminal, data);
        let replies = std::mem::take(&mut *self.replies.0.lock().unwrap());
        replies
            .into_iter()
            .map(|reply| {
                let data = match reply {
                    Event::PtyWrite(data) => data,
                    Event::ColorRequest(index, format) => {
                        let value = agent_protocol::operations::terminal_color(index as u16);
                        let color = self.terminal.colors()[index].unwrap_or(Rgb {
                            r: (value >> 16) as u8,
                            g: (value >> 8) as u8,
                            b: value as u8,
                        });
                        format(color)
                    }
                    Event::TextAreaSizeRequest(format) => format(WindowSize {
                        num_cols: self.terminal.columns() as u16,
                        num_lines: self.terminal.screen_lines() as u16,
                        cell_width: 0,
                        cell_height: 0,
                    }),
                    Event::ClipboardLoad(_, format) => format(""),
                    _ => unreachable!(),
                };
                data.into_bytes()
            })
            .collect()
    }

    pub(super) fn size(&self) -> TerminalSize {
        TerminalSize {
            cols: self.terminal.columns() as u16,
            rows: self.terminal.screen_lines() as u16,
        }
    }

    pub(super) fn resize(&mut self, size: TerminalSize) {
        self.terminal.resize(Dimensions(size));
    }

    /// The startup notification predates any client input and omits parser replay.
    pub(super) fn initial_checkpoint(&self) -> Vec<u8> {
        self.terminal.ansi_checkpoint(None)
    }

    pub(super) fn checkpoint(&self) -> Checkpoint {
        let mut data = self.terminal.ansi_checkpoint(self.parser.preceding_char());
        data.extend(self.parser.checkpoint_tail());
        Checkpoint {
            data,
            size: self.size(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_queries_reply_in_order_and_are_drained_once() {
        let output = b"abc\x1b[2;4H\x1b[6n\x1b[5n";
        for split in 0..=output.len() {
            let mut screen = TerminalScreen::new(TerminalSize { rows: 5, cols: 12 });
            let mut replies = screen.feed(&output[..split]);
            replies.extend(screen.feed(&output[split..]));
            assert_eq!(replies, [b"\x1b[2;4R".to_vec(), b"\x1b[0n".to_vec()]);
            assert!(screen.feed(b"").is_empty());
            assert!(screen.feed(b"more output").is_empty());
        }
    }

    #[test]
    fn color_queries_use_host_palette_and_terminal_overrides() {
        let mut screen = TerminalScreen::new(TerminalSize { rows: 5, cols: 12 });
        assert_eq!(
            screen.feed(b"\x1b]4;1;?\x07\x1b]10;?\x1b\\\x1b]11;?\x07"),
            [
                b"\x1b]4;1;rgb:cccc/6666/6666\x07".to_vec(),
                b"\x1b]10;rgb:e5e5/e5e5/e5e5\x1b\\".to_vec(),
                b"\x1b]11;rgb:1818/1818/1818\x07".to_vec(),
            ]
        );
        assert!(screen.feed(b"\x1b]4;1;rgb:12/34/56\x07").is_empty());
        assert_eq!(
            screen.feed(b"\x1b]4;1;?\x07"),
            [b"\x1b]4;1;rgb:1212/3434/5656\x07".to_vec()]
        );
        assert!(screen.feed(b"\x1b]104;1\x07").is_empty());
        assert_eq!(
            screen.feed(b"\x1b]4;1;?\x07"),
            [b"\x1b]4;1;rgb:cccc/6666/6666\x07".to_vec()]
        );
    }

    #[test]
    fn resized_checkpoint_restores_dimensions_and_an_unfinished_query() {
        let initial = TerminalSize { rows: 5, cols: 12 };
        let mut screen = TerminalScreen::new(initial);
        assert_eq!(screen.size(), initial);
        assert_eq!(screen.initial_checkpoint(), screen.checkpoint().data);
        let resized = TerminalSize { rows: 7, cols: 20 };
        screen.resize(resized);
        assert_eq!(screen.size(), resized);
        assert_eq!(
            screen.feed(b"\x1b[18t\x1b[14t"),
            [b"\x1b[8;7;20t".to_vec(), b"\x1b[4;0;0t".to_vec()]
        );
        assert!(screen.feed(b"\x1b[4;9H\x1b[6").is_empty());
        let checkpoint = screen.checkpoint();
        assert_eq!(checkpoint.size, resized);
        let mut restored = TerminalScreen::new(checkpoint.size);
        assert!(restored.feed(&checkpoint.data).is_empty());
        let replies = restored.feed(b"n\x1b[18t");
        assert_eq!(replies, [b"\x1b[4;9R".to_vec(), b"\x1b[8;7;20t".to_vec()]);
        assert_eq!(screen.feed(b"n\x1b[18t"), replies);
    }

    #[test]
    fn clipboard_and_title_events_do_not_produce_host_replies() {
        let mut screen = TerminalScreen::new(TerminalSize { rows: 5, cols: 12 });
        // Preserve Alacritty's default OSC52 policy: reads are disabled, while
        // clipboard writes and window-title events are not terminal-query replies.
        assert!(
            screen
                .feed(b"\x1b]0;title\x07\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07")
                .is_empty()
        );
        assert!(screen.feed(b"").is_empty());
    }

    #[test]
    fn checkpoint_restores_screen_modes_and_split_sequences() {
        let size = TerminalSize { rows: 5, cols: 12 };
        let basic = [
            (
                b"hello\r\nworld\x1b[31m!\x1b[3;8H".as_slice(),
                b"again".as_slice(),
            ),
            (
                b"original\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?2004h",
                b"\r\nmore\x1b[?1049l!",
            ),
            (b"012345678901", b"next"),
            (b"hi\x1b[38;2;12;", b"34;56mcolor"),
            (b"\xe6\x97", b"\xa5\xe6\x9c\xac"),
            (b"abc\x1b[2J", b"\x1b[3b"),
            (b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", b"\r\nseven"),
        ];
        let sequences = [
            "日本語\r\n12345678901日\r\ne\u{301}\x1b[31;44;1mred\x1b[0m",
            "abc\x1b7\r\nother\x1b8!",
            "123456789012\x1b7\r\nnext\x1b8X",
            "screen\x1b[?1049h\x1b[2;4r\x1b[?6hALT\x1b[?1049l!",
            "abc\x1b]0;title\x1b\\hello\x1b[38;2;12;34;56mRGB",
            "before\x1b[?2026hupdate\r\nmore\x1b[?2026lafter",
        ];
        let cases =
            basic.into_iter().chain(sequences.iter().flat_map(|text| {
                (0..=text.len()).map(move |index| text.as_bytes().split_at(index))
            }));
        for (before, after) in cases {
            let mut original = TerminalScreen::new(size);
            original.feed(before);
            let checkpoint = original.checkpoint();
            let mut restored = TerminalScreen::new(checkpoint.size);
            restored.feed(&checkpoint.data);
            original.feed(after);
            restored.feed(after);
            assert_eq!(
                original.terminal.mode(),
                restored.terminal.mode(),
                "{before:?}"
            );
            assert_eq!(
                original.terminal.grid().cursor.point,
                restored.terminal.grid().cursor.point,
                "{before:?}"
            );
            assert_eq!(
                original.terminal.grid().history_size(),
                restored.terminal.grid().history_size(),
                "{before:?}"
            );
            for row in -(original.terminal.grid().history_size() as i32)..5 {
                for col in 0..12 {
                    use alacritty_terminal::index::{Column, Line};
                    let a = &original.terminal.grid()[Line(row)][Column(col)];
                    let b = &restored.terminal.grid()[Line(row)][Column(col)];
                    assert_eq!(
                        (a.c, a.fg, a.bg, a.flags, a.zerowidth()),
                        (b.c, b.fg, b.bg, b.flags, b.zerowidth()),
                        "{before:?}, {row}:{col}"
                    );
                }
            }
        }
    }
}
