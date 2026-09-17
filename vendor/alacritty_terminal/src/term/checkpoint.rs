//! ANSI checkpoint used by Bex to attach native emulators to an existing PTY.
use super::*;
use crate::grid::Cursor;
use crate::term::cell::Cell;
use crate::vte::ansi::{Color, CursorShape};
use std::fmt::Write;

impl<T: EventListener> Term<T> {
    /// Repaint both buffers and restore the state needed by subsequent PTY bytes.
    /// The caller appends the VTE processor's unfinished input after this output.
    pub fn ansi_checkpoint(&self, preceding: Option<char>) -> Vec<u8> {
        let mut out = String::from("\x1bc\x1b[?25l\x1b[?7h");
        for index in 0..256 {
            if let Some(rgb) = self.colors[index] {
                let _ = write!(
                    out,
                    "\x1b]4;{index};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
                    rgb.r, rgb.g, rgb.b
                );
            }
        }
        for (index, code) in [(256, 10), (257, 11), (258, 12)] {
            if let Some(rgb) = self.colors[index] {
                let _ = write!(
                    out,
                    "\x1b]{code};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
                    rgb.r, rgb.g, rgb.b
                );
            }
        }
        let alternate = self.mode.contains(TermMode::ALT_SCREEN);
        let primary = if alternate {
            &self.inactive_grid
        } else {
            &self.grid
        };
        save_checkpoint_cursor(&mut out, &primary.saved_cursor);
        paint_grid(&mut out, primary, true);
        if alternate {
            out.push_str("\x1b8\x1b[?1049h");
            save_checkpoint_cursor(&mut out, &self.grid.saved_cursor);
            paint_grid(&mut out, &self.grid, false);
        }
        // Restore tab stops after painting (painting must not depend on them).
        out.push_str("\x1b[3g");
        for column in 0..self.columns() {
            if self.tabs[Column(column)] {
                let _ = write!(out, "\x1b[1;{}H\x1bH", column + 1);
            }
        }
        // REP uses the parser's last printed character, independently of the cursor.
        if let Some(ch) = preceding {
            let mut found = false;
            'rows: for row in 0..self.screen_lines() {
                for col in 0..self.columns() {
                    let cell = &self.grid[Line(row as i32)][Column(col)];
                    if (cell.c == ch
                        || cell
                            .zerowidth()
                            .is_some_and(|chars| chars.last() == Some(&ch)))
                        && !cell
                            .flags
                            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
                        paint_cell(&mut out, cell);
                        found = true;
                        break 'rows;
                    }
                }
            }
            if !found {
                'blank: for row in 0..self.screen_lines() {
                    for col in 0..self.columns() {
                        let cell = &self.grid[Line(row as i32)][Column(col)];
                        if cell.c == ' '
                            && !cell
                                .flags
                                .intersects(Flags::WIDE_CHAR_SPACER | Flags::WIDE_CHAR)
                        {
                            let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
                            style(&mut out, cell);
                            if unicode_width::UnicodeWidthChar::width(ch) == Some(0) {
                                out.push(' ');
                            }
                            out.push(ch);
                            let _ = write!(out, "\x1b[{};{}H\x1b[X", row + 1, col + 1);
                            break 'blank;
                        }
                    }
                }
            }
        }
        let _ = write!(
            out,
            "\x1b[{};{}r",
            self.scroll_region.start.0 + 1,
            self.scroll_region.end.0
        );
        for (flag, code) in [
            (TermMode::APP_CURSOR, 1),
            (TermMode::ORIGIN, 6),
            (TermMode::LINE_WRAP, 7),
            (TermMode::SHOW_CURSOR, 25),
            (TermMode::MOUSE_REPORT_CLICK, 1000),
            (TermMode::MOUSE_DRAG, 1002),
            (TermMode::MOUSE_MOTION, 1003),
            (TermMode::FOCUS_IN_OUT, 1004),
            (TermMode::UTF8_MOUSE, 1005),
            (TermMode::SGR_MOUSE, 1006),
            (TermMode::ALTERNATE_SCROLL, 1007),
            (TermMode::BRACKETED_PASTE, 2004),
        ] {
            let _ = write!(
                out,
                "\x1b[?{code}{}",
                if self.mode.contains(flag) { 'h' } else { 'l' }
            );
        }
        for (flag, code) in [(TermMode::INSERT, 4), (TermMode::LINE_FEED_NEW_LINE, 20)] {
            let _ = write!(
                out,
                "\x1b[{code}{}",
                if self.mode.contains(flag) { 'h' } else { 'l' }
            );
        }
        out.push_str(if self.mode.contains(TermMode::APP_KEYPAD) {
            "\x1b="
        } else {
            "\x1b>"
        });
        // Cursor positioning under DECOM is relative to the scrolling region.
        let mut cursor = self.grid.cursor.clone();
        if self.mode.contains(TermMode::ORIGIN) && !self.scroll_region.contains(&cursor.point.line)
        {
            // Returning from 1049 can leave the cursor outside the scrolling region.
            // CUP is clamped under DECOM; restore its saved absolute position instead.
            out.push_str("\x1b8");
            let delta = cursor.point.line.0 - self.grid.saved_cursor.point.line.0;
            if delta != 0 {
                let _ = write!(
                    out,
                    "\x1b[{}{}",
                    delta.unsigned_abs(),
                    if delta < 0 { 'A' } else { 'B' }
                );
            }
            let col = cursor.point.column.0 as i32 - self.grid.saved_cursor.point.column.0 as i32;
            if col != 0 {
                let _ = write!(
                    out,
                    "\x1b[{}{}",
                    col.unsigned_abs(),
                    if col < 0 { 'D' } else { 'C' }
                );
            }
            cursor_attributes(&mut out, &cursor);
        } else {
            if self.mode.contains(TermMode::ORIGIN) {
                cursor.point.line -= self.scroll_region.start.0;
            }
            restore_cursor(&mut out, &cursor);
        }
        // Preserve delayed auto-wrap without moving to the next line prematurely.
        if cursor.input_needs_wrap {
            let column = self.columns() - 1;
            let cell = &self.grid[self.grid.cursor.point.line][Column(column)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) && column > 0 {
                out.push_str("\x1b[D");
                paint_cell(
                    &mut out,
                    &self.grid[self.grid.cursor.point.line][Column(column - 1)],
                );
            } else {
                paint_cell(&mut out, cell);
            }
            style(&mut out, &cursor.template);
        }
        let active = match self.active_charset {
            CharsetIndex::G0 => "\x0f",
            CharsetIndex::G1 => "\x0e",
            CharsetIndex::G2 => "\x1bn",
            CharsetIndex::G3 => "\x1bo",
        };
        out.push_str(active);
        let cursor_style = self.cursor_style();
        let shape = match cursor_style.shape {
            CursorShape::Block => 2,
            CursorShape::Underline => 4,
            CursorShape::Beam => 6,
            CursorShape::HollowBlock => 2,
            CursorShape::Hidden => 2,
        };
        let _ = write!(out, "\x1b[{} q", shape - u8::from(cursor_style.blinking));
        out.into_bytes()
    }
}
fn paint_grid(out: &mut String, grid: &Grid<Cell>, history: bool) {
    out.push_str("\x1b[0m\x1b[H");
    let start = if history {
        -(grid.history_size() as i32)
    } else {
        0
    };
    let mut previous: Option<&Cell> = None;
    for row in start..grid.screen_lines() as i32 {
        if row != start {
            let previous = &grid[Line(row - 1)];
            if !previous[Column(grid.columns() - 1)]
                .flags
                .contains(Flags::WRAPLINE)
            {
                out.push_str("\r\n");
            }
        }
        let line = &grid[Line(row)];
        for column in 0..grid.columns() {
            let cell = &line[Column(column)];
            if !cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                if previous.is_none_or(|old| {
                    old.fg != cell.fg
                        || old.bg != cell.bg
                        || old.flags != cell.flags
                        || old.hyperlink() != cell.hyperlink()
                        || old.underline_color() != cell.underline_color()
                }) {
                    style(out, cell);
                    if let Some(link) = cell.hyperlink() {
                        let _ = write!(out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
                    } else {
                        out.push_str("\x1b]8;;\x1b\\");
                    }
                }
                out.push(cell.c);
                if let Some(chars) = cell.zerowidth() {
                    out.extend(chars);
                }
                previous = Some(cell);
            }
        }
    }
    out.push_str("\x1b]8;;\x1b\\");
}
fn save_checkpoint_cursor(out: &mut String, cursor: &Cursor<Cell>) {
    restore_cursor(out, cursor);
    // Painting the real grid afterwards removes this scratch glyph while preserving DECSC.
    if cursor.input_needs_wrap {
        out.push(' ');
    }
    out.push_str("\x1b7");
}
fn restore_cursor(out: &mut String, cursor: &Cursor<Cell>) {
    let _ = write!(
        out,
        "\x1b[{};{}H",
        cursor.point.line.0 + 1,
        cursor.point.column.0 + 1
    );
    cursor_attributes(out, cursor);
}
fn cursor_attributes(out: &mut String, cursor: &Cursor<Cell>) {
    style(out, &cursor.template);
    for (index, code) in [
        (CharsetIndex::G0, '('),
        (CharsetIndex::G1, ')'),
        (CharsetIndex::G2, '*'),
        (CharsetIndex::G3, '+'),
    ] {
        let set = match cursor.charsets[index] {
            StandardCharset::Ascii => 'B',
            StandardCharset::SpecialCharacterAndLineDrawing => '0',
        };
        let _ = write!(out, "\x1b{code}{set}");
    }
}
fn paint_cell(out: &mut String, cell: &Cell) {
    style(out, cell);
    if let Some(link) = cell.hyperlink() {
        let _ = write!(out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
    } else {
        out.push_str("\x1b]8;;\x1b\\");
    }
    out.push(cell.c);
    if let Some(chars) = cell.zerowidth() {
        out.extend(chars);
    }
}
fn style(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
        (Flags::DOUBLE_UNDERLINE, 21),
    ] {
        if cell.flags.contains(flag) {
            let _ = write!(out, ";{code}");
        }
    }
    for (flag, kind) in [
        (Flags::UNDERCURL, 3),
        (Flags::DOTTED_UNDERLINE, 4),
        (Flags::DASHED_UNDERLINE, 5),
    ] {
        if cell.flags.contains(flag) {
            let _ = write!(out, ";4:{kind}");
        }
    }
    color(out, cell.fg, true);
    color(out, cell.bg, false);
    if let Some(underline) = cell.underline_color() {
        match underline {
            Color::Spec(rgb) => {
                let _ = write!(out, ";58;2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
            Color::Indexed(index) => {
                let _ = write!(out, ";58;5;{index}");
            }
            Color::Named(_) => {}
        }
    }
    out.push('m');
}
fn color(out: &mut String, value: Color, foreground: bool) {
    let code = if foreground { 38 } else { 48 };
    match value {
        Color::Spec(rgb) => {
            let _ = write!(out, ";{code};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(index) => {
            let _ = write!(out, ";{code};5;{index}");
        }
        Color::Named(name) => {
            let index = name as usize;
            if index < 16 {
                let base = if foreground { 30 } else { 40 };
                let sgr = if index < 8 {
                    base + index
                } else {
                    base + 60 + index - 8
                };
                let _ = write!(out, ";{sgr}");
            }
        }
    }
}
