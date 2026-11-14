//! T3 terminalOutput.ts semantics. Its deliberately small escape grammar and
//! SGR rules differ from a full terminal emulator (for example SGR 21 is ignored).
//! Only 64 live lines are addressable; output is bounded in UTF-16 code units.
use super::TAIL_CHARS;
use std::{collections::VecDeque, sync::Arc};

#[cfg(test)]
std::thread_local! { pub(super) static PRINTED_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

const ESC: &str = "\x1b";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    strike: bool,
    fg: String,
    bg: String,
}
impl Style {
    fn pen(&self) -> String {
        let mut codes: Vec<String> = [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.inverse, "7"),
            (self.strike, "9"),
        ]
        .into_iter()
        .filter(|(set, _)| *set)
        .map(|(_, code)| code.into())
        .collect();
        codes.extend(
            [&self.fg, &self.bg]
                .into_iter()
                .filter(|code| !code.is_empty())
                .cloned(),
        );
        codes.join(";")
    }
}
impl Style {
    fn attribute(&mut self, code: u32) {
        match code {
            0 => *self = Self::default(),
            1 => self.bold = true,
            2 => self.dim = true,
            3 => self.italic = true,
            4 => self.underline = true,
            7 => self.inverse = true,
            9 => self.strike = true,
            22 => {
                self.bold = false;
                self.dim = false;
            }
            23 => self.italic = false,
            24 => self.underline = false,
            27 => self.inverse = false,
            29 => self.strike = false,
            30..=37 | 90..=97 => self.fg = code.to_string(),
            40..=47 | 100..=107 => self.bg = code.to_string(),
            39 => self.fg.clear(),
            49 => self.bg.clear(),
            _ => {}
        }
    }
    fn apply(&mut self, params: &[f64]) {
        let mut index = 0;
        while index < params.len() {
            let code = params[index] as u32;
            if matches!(code, 38 | 48 | 58) {
                let (color, next) = extended_color(code, params, index);
                if let Some(color) = color {
                    match code {
                        38 => self.fg = color,
                        48 => self.bg = color,
                        _ => {}
                    }
                }
                index = next;
            } else {
                self.attribute(code);
                index += 1;
            }
        }
    }
}

fn extended_color(code: u32, params: &[f64], index: usize) -> (Option<String>, usize) {
    match params.get(index + 1).copied() {
        Some(5.0) => (
            params
                .get(index + 2)
                .filter(|n| **n <= 255.0)
                .map(|n| format!("{code};5;{n}")),
            index + 3,
        ),
        Some(2.0) => (
            params.get(index + 2..index + 5).map(|rgb| {
                // JS bitwise colour conversion keeps the low eight bits.
                let channel = |value: f64| {
                    if value.is_finite() {
                        value.rem_euclid(256.0) as u8
                    } else {
                        0
                    }
                };
                format!(
                    "{code};2;{};{};{}",
                    channel(rgb[0]),
                    channel(rgb[1]),
                    channel(rgb[2])
                )
            }),
            index + 5,
        ),
        _ => (None, index + 2),
    }
}

fn csi_params(params: &str) -> Vec<f64> {
    enum Pending {
        Code,
        Mode,
        Values(usize),
    }
    let mut pending = Pending::Code;
    let mut numbers = Vec::new();
    for part in params.split(';') {
        let mut sub: Vec<f64> = part
            .split(':')
            .map(|n| {
                if n.is_empty() {
                    0.0
                } else {
                    n.parse().unwrap_or(f64::INFINITY)
                }
            })
            .collect();
        let head = sub[0];
        match pending {
            Pending::Values(count) => {
                numbers.push(head);
                pending = if count > 1 {
                    Pending::Values(count - 1)
                } else {
                    Pending::Code
                };
            }
            Pending::Mode => {
                pending = Pending::Code;
                if sub.len() == 1 {
                    numbers.push(head);
                    pending = match head {
                        5.0 => Pending::Values(1),
                        2.0 => Pending::Values(3),
                        _ => Pending::Code,
                    };
                } else {
                    if head == 2.0 && sub.len() >= 5 {
                        sub.remove(1);
                    }
                    numbers.extend(sub.into_iter().take(match head {
                        5.0 => 2,
                        2.0 => 4,
                        _ => 1,
                    }));
                }
            }
            Pending::Code if sub.len() == 1 => {
                numbers.push(head);
                if matches!(head, 38.0 | 48.0 | 58.0) {
                    pending = Pending::Mode;
                }
            }
            Pending::Code if !matches!(head, 38.0 | 48.0 | 58.0) => {
                numbers.push(if head == 4.0 && sub[1] == 0.0 {
                    24.0
                } else {
                    head
                })
            }
            Pending::Code => {
                if sub[1] == 2.0 && sub.len() >= 6 {
                    sub.remove(2);
                }
                let keep = match sub[1] {
                    5.0 => 3,
                    2.0 => 5,
                    _ => 2,
                };
                numbers.extend(sub.into_iter().take(keep));
            }
        }
    }
    numbers
}

#[derive(Debug, Clone)]
struct Cell {
    unit: u16,
    style: Arc<Style>,
}
#[derive(Debug, Clone, Default)]
struct Line {
    cells: VecDeque<Cell>,
    runs: usize,
}
impl Line {
    fn push(&mut self, cell: Cell) {
        if self
            .cells
            .back()
            .is_none_or(|last| last.style != cell.style)
        {
            self.runs += 1;
        }
        self.cells.push_back(cell);
    }
    fn replace(&mut self, index: usize, cell: Cell) {
        let edges = |style: &Arc<Style>| {
            usize::from(index > 0 && self.cells[index - 1].style != *style)
                + usize::from(index + 1 < self.cells.len() && self.cells[index + 1].style != *style)
        };
        self.runs = self.runs + edges(&cell.style) - edges(&self.cells[index].style);
        self.cells[index] = cell;
    }
    fn recount(&mut self) {
        let mut previous = None;
        self.runs = 0;
        for cell in &self.cells {
            if previous.is_none_or(|previous: &Arc<Style>| previous != &cell.style) {
                self.runs += 1;
            }
            previous = Some(&cell.style);
        }
    }
    fn pop_front(&mut self) {
        if let Some(old) = self.cells.pop_front()
            && self
                .cells
                .front()
                .is_none_or(|next| old.style != next.style)
        {
            self.runs = self.runs.saturating_sub(1);
        }
    }
    fn bound_styles(&mut self) {
        if self.runs <= 2048 {
            return;
        }
        let flatten = self.runs - 256;
        let mut previous = None;
        let mut run = 0;
        let plain: Arc<Style> = Arc::default();
        for cell in &mut self.cells {
            if previous
                .as_ref()
                .is_none_or(|previous: &Arc<Style>| previous != &cell.style)
            {
                run += 1;
                previous = Some(cell.style.clone());
            }
            if run > flatten {
                break;
            }
            cell.style = plain.clone();
        }
        self.recount();
    }
}

fn sgr(pen: &str) -> String {
    if pen.is_empty() {
        format!("{ESC}[0m")
    } else {
        format!("{ESC}[0;{pen}m")
    }
}
fn line_text(line: &Line) -> String {
    let mut text = String::new();
    let mut style = Arc::new(Style::default());
    let mut units = Vec::new();
    for cell in &line.cells {
        if style != cell.style {
            text.push_str(&String::from_utf16_lossy(&units));
            units.clear();
            text.push_str(&sgr(&cell.style.pen()));
            style = cell.style.clone();
        }
        units.push(cell.unit);
    }
    text.push_str(&String::from_utf16_lossy(&units));
    if *style != Style::default() {
        text.push_str(&sgr(""));
    }
    text
}

#[derive(Debug, Clone)]
struct Screen {
    settled: String,
    settled_units: usize,
    lines: VecDeque<Line>,
    row: usize,
    col: usize,
    pen: Arc<Style>,
    saved: Option<(usize, usize)>,
    dropped: bool,
}
impl Default for Screen {
    fn default() -> Self {
        Self {
            settled: String::new(),
            settled_units: 0,
            lines: [Line::default()].into(),
            row: 0,
            col: 0,
            pen: Arc::default(),
            saved: None,
            dropped: false,
        }
    }
}
impl Screen {
    fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.lines.len() - 1);
        self.col = col.min(TAIL_CHARS);
    }
    fn scroll(&mut self) -> usize {
        let line = self.lines.pop_front().expect("live line");
        let text = line_text(&line);
        self.settled_units += text.encode_utf16().count() + 1;
        self.settled.push_str(&text);
        self.settled.push('\n');
        if self.settled_units > 2 * TAIL_CHARS {
            self.settled = tail(&self.settled).0;
            self.settled_units = self.settled.encode_utf16().count();
            self.dropped = true;
        }
        self.row -= 1;
        if let Some((row, _)) = &mut self.saved {
            *row = row.saturating_sub(1);
        }
        line.cells.len()
    }
    fn trim_line(&mut self, row: usize, keep: usize) {
        let line = &mut self.lines[row];
        let mut dropped = line.cells.len().saturating_sub(keep);
        for _ in 0..dropped {
            line.pop_front();
        }
        if line
            .cells
            .front()
            .is_some_and(|cell| (0xdc00..=0xdfff).contains(&cell.unit))
        {
            line.pop_front();
            dropped += 1;
        }
        if dropped == 0 {
            return;
        }
        self.dropped = true;
        if self.row == row {
            self.col = self.col.saturating_sub(dropped);
        }
        if let Some((saved_row, col)) = &mut self.saved
            && *saved_row == row
        {
            *col = col.saturating_sub(dropped);
        }
    }
    fn erase(&mut self, mode: u32) {
        let line = &mut self.lines[self.row];
        match mode {
            0 => line.cells.truncate(self.col),
            1 => {
                let plain = Arc::default();
                for cell in line.cells.iter_mut().take(self.col.saturating_add(1)) {
                    cell.unit = b' ' as u16;
                    cell.style = Arc::clone(&plain);
                }
            }
            2 => line.cells.clear(),
            _ => {}
        }
        line.recount();
    }
    fn clear(&mut self, mode: u32) {
        match mode {
            0 => {
                self.erase(0);
                self.lines.truncate(self.row + 1);
            }
            1 => {
                for line in self.lines.iter_mut().take(self.row) {
                    *line = Line::default();
                }
                self.erase(1);
            }
            2 => {
                self.lines = [Line::default()].into();
                self.row = 0;
                self.col = 0;
            }
            3 => {
                self.dropped |= !self.settled.is_empty();
                self.settled.clear();
                self.settled_units = 0;
            }
            _ => {}
        }
    }
}
impl Screen {
    fn print(&mut self, ch: char) {
        #[cfg(test)]
        PRINTED_UNITS.set(PRINTED_UNITS.get() + ch.len_utf16());
        let line = &mut self.lines[self.row];
        let plain = Arc::default();
        while line.cells.len() < self.col {
            line.push(Cell {
                unit: b' ' as u16,
                style: Arc::clone(&plain),
            });
        }
        for unit in ch.encode_utf16(&mut [0; 2]) {
            let cell = Cell {
                unit: *unit,
                style: self.pen.clone(),
            };
            if self.col == line.cells.len() {
                line.push(cell);
            } else {
                line.replace(self.col, cell);
            }
            self.col += 1;
        }
        line.bound_styles();
        self.trim_line(self.row, TAIL_CHARS);
    }
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.row += 1;
                self.col = 0;
                if self.row == self.lines.len() {
                    self.lines.push_back(Line::default());
                }
                while self.lines.len() > 64 {
                    self.scroll();
                }
            }
            b'\r' => self.col = 0,
            8 => self.col = self.col.saturating_sub(1),
            _ => {}
        }
    }
    fn escape(&mut self, sequence: &str) {
        if sequence.starts_with("\x1b[") {
            let final_byte = sequence.as_bytes()[sequence.len() - 1];
            self.csi(&sequence[2..sequence.len() - 1], final_byte);
        } else {
            match sequence {
                "\x1b7" => self.saved = Some((self.row, self.col)),
                "\x1b8" => {
                    if let Some((row, col)) = self.saved {
                        self.move_to(row, col);
                    }
                }
                "\x1bM" => self.row = self.row.saturating_sub(1),
                _ => {}
            }
        }
    }
    fn csi(&mut self, params: &str, action: u8) {
        if !params
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b';' | b':'))
            || action != b'm' && params.contains(':')
        {
            return;
        }
        let values = csi_params(params);
        if action == b'm' {
            Arc::make_mut(&mut self.pen).apply(&values);
            return;
        }
        let first = values.first().copied().unwrap_or(0.0);
        let count = first.max(1.0) as usize;
        match action {
            b'A' => self.row = self.row.saturating_sub(count),
            b'B' => self.move_to(self.row.saturating_add(count), self.col),
            b'C' => self.move_to(self.row, self.col.saturating_add(count)),
            b'D' => self.col = self.col.saturating_sub(count),
            b'E' => self.move_to(self.row.saturating_add(count), 0),
            b'F' => self.move_to(self.row.saturating_sub(count), 0),
            b'G' => self.move_to(self.row, count - 1),
            b'H' | b'f' => self.move_to(
                count - 1,
                values.get(1).copied().unwrap_or(1.0).max(1.0) as usize - 1,
            ),
            b'K' => self.erase(first as u32),
            b'J' => self.clear(first as u32),
            b's' => self.saved = Some((self.row, self.col)),
            b'u' => {
                if let Some((row, col)) = self.saved {
                    self.move_to(row, col);
                }
            }
            _ => {}
        }
    }
}

/// A safe tail cut preserves the active SGR style and prefers a fresh line.
fn tail(text: &str) -> (String, bool) {
    let units = text.encode_utf16().count();
    if units <= TAIL_CHARS {
        return (text.into(), false);
    }
    let mut remaining = units - TAIL_CHARS;
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if remaining == 0 {
            start = index;
            break;
        }
        remaining = remaining.saturating_sub(ch.len_utf16());
        start = index + ch.len_utf8();
    }
    if let Some(newline) = text[start..].find('\n')
        && text[start..start + newline].encode_utf16().count() < TAIL_CHARS / 4
    {
        return (text[start + newline + 1..].into(), true);
    }
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let mut prefix = "";
    let mut cursor = line_start;
    while cursor < start {
        let Some(at) = text[cursor..]
            .find("\x1b[")
            .map(|at| cursor + at)
            .filter(|at| *at < start)
        else {
            break;
        };
        let Some(end) = text[at..].find('m').map(|end| at + end) else {
            break;
        };
        prefix = &text[at..=end];
        if end >= start {
            return (format!("{prefix}{}", &text[end + 1..]), true);
        }
        cursor = end + 1;
    }
    if prefix == "\x1b[0m" {
        prefix = "";
    }
    (format!("{prefix}{}", &text[start..]), true)
}

/// Complete escape length in bytes. ASCII control delimiters are UTF-8 boundaries.
fn escape_length(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let next = *bytes.get(1)?;
    match next {
        27 => Some(1),
        b'[' => bytes
            .iter()
            .enumerate()
            .skip(2)
            .find(|(_, b)| (0x40..=0x7e).contains(*b))
            .map(|(at, _)| at + 1),
        b']' | b'P' | b'X' | b'_' | b'^' => {
            for (at, byte) in bytes.iter().copied().enumerate().skip(2) {
                match byte {
                    7 => return Some(at + 1),
                    b'\n' => return Some(at),
                    27 => {
                        return Some(if *bytes.get(at + 1)? == b'\\' {
                            at + 2
                        } else {
                            at
                        });
                    }
                    _ => {}
                }
            }
            None
        }
        _ => {
            let mut at = 1;
            while bytes.get(at).is_some_and(|b| (0x20..=0x2f).contains(b)) {
                at += 1;
            }
            let byte = *bytes.get(at)?;
            if at > 1 && !(0x30..=0x7e).contains(&byte) {
                Some(at)
            } else {
                Some(at + input[at..].chars().next()?.len_utf8())
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Buffer {
    screen: Screen,
    pending: String,
}
impl Buffer {
    pub fn append(&mut self, input: &str) {
        let input = std::mem::take(&mut self.pending) + input;
        let screen = &mut self.screen;
        let mut cursor = 0;
        while cursor < input.len() {
            let ch = input[cursor..].chars().next().expect("character");
            if ch == '\x1b' {
                let Some(length) = escape_length(&input[cursor..]) else {
                    let fragment = &input[cursor..];
                    if fragment.encode_utf16().count() <= 64 {
                        self.pending = fragment.into();
                    } else if fragment
                        .as_bytes()
                        .get(1)
                        .is_some_and(|b| b"]PX_^".contains(b))
                    {
                        self.pending = fragment[..2].to_owned();
                        if fragment.ends_with('\x1b') {
                            self.pending.push('\x1b');
                        }
                    }
                    break;
                };
                screen.escape(&input[cursor..cursor + length]);
                cursor += length;
            } else {
                if ch == '\t' || ch >= ' ' && ch != '\x7f' && !('\u{80}'..='\u{9f}').contains(&ch) {
                    screen.print(ch);
                } else if ch.is_ascii() {
                    screen.execute(ch as u8);
                }
                cursor += ch.len_utf8();
            }
        }
        let mut columns: usize = screen.lines.iter().map(|line| line.cells.len()).sum();
        while columns > TAIL_CHARS && screen.row > 0 {
            columns -= screen.scroll();
        }
        let mut budget = TAIL_CHARS;
        for row in 0..screen.lines.len() {
            let keep = screen.lines[row].cells.len().min(budget);
            screen.trim_line(row, keep);
            budget -= keep;
        }
    }
    pub fn display(&self) -> (String, bool) {
        let screen = &self.screen;
        let (settled, cut) = tail(&screen.settled);
        let window = screen
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        let (text, window_cut) = tail(&(settled + &window));
        (text, screen.dropped || cut || window_cut)
    }
    pub fn resume(&self) -> String {
        let screen = &self.screen;
        let (text, _) = self.display();
        let reachable = text.bytes().filter(|byte| *byte == b'\n').take(63).count();
        let last = screen.lines.len() - 1;
        let go_to = |row: usize, col: usize| {
            let up = last.saturating_sub(row).min(reachable);
            format!(
                "{}{ESC}[{}G",
                if up > 0 {
                    format!("{ESC}[{up}A")
                } else {
                    String::new()
                },
                col + 1
            )
        };
        let mut resume = text.clone();
        if let Some((row, col)) = screen.saved {
            resume.push_str(&go_to(row, col));
            resume.push_str(&format!("{ESC}7"));
            let up = last.saturating_sub(row).min(reachable);
            if up > 0 {
                resume.push_str(&format!("{ESC}[{up}B"));
            }
        }
        if screen.saved.is_some()
            || screen.row < last
            || screen.col != screen.lines[screen.row].cells.len()
        {
            resume.push_str(&go_to(screen.row, screen.col));
        }
        let pen = screen.pen.pen();
        if !pen.is_empty() {
            resume.push_str(&sgr(&pen));
        }
        resume.push_str(&self.pending);
        resume
    }
}
