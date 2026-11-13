//! T3 terminalOutput.ts semantics, using the existing VTE parser for escapes.
//! Only 64 live lines are addressable; output is bounded in UTF-16 code units.
use super::TAIL_CHARS;
use std::{collections::VecDeque, sync::Arc};
use vte::ansi::{self, Attr, Color, Handler, NamedColor};

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
fn color(color: Color, background: bool) -> String {
    let prefix = if background { 48 } else { 38 };
    match color {
        Color::Spec(rgb) => format!("{prefix};2;{};{};{}", rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => format!("{prefix};5;{index}"),
        Color::Named(NamedColor::Foreground | NamedColor::Background) => String::new(),
        Color::Named(color) => {
            let index = color as usize;
            match index {
                0..8 => (if background { 40 + index } else { 30 + index }).to_string(),
                8..16 => (if background {
                    100 + index - 8
                } else {
                    90 + index - 8
                })
                .to_string(),
                _ => String::new(),
            }
        }
    }
}
impl Handler for Style {
    fn terminal_attribute(&mut self, attr: Attr) {
        match attr {
            Attr::Reset => *self = Self::default(),
            Attr::Bold => self.bold = true,
            Attr::Dim => self.dim = true,
            Attr::Italic => self.italic = true,
            Attr::Underline
            | Attr::DoubleUnderline
            | Attr::Undercurl
            | Attr::DottedUnderline
            | Attr::DashedUnderline => self.underline = true,
            Attr::Reverse => self.inverse = true,
            Attr::Strike => self.strike = true,
            Attr::CancelBoldDim => {
                self.bold = false;
                self.dim = false;
            }
            Attr::CancelBold => self.bold = false,
            Attr::CancelItalic => self.italic = false,
            Attr::CancelUnderline => self.underline = false,
            Attr::CancelReverse => self.inverse = false,
            Attr::CancelStrike => self.strike = false,
            Attr::Foreground(value) => self.fg = color(value, false),
            Attr::Background(value) => self.bg = color(value, true),
            _ => {}
        }
    }
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
    fn erase(&mut self, mode: u16) {
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
    fn clear(&mut self, mode: u16) {
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
impl vte::Perform for Screen {
    fn print(&mut self, ch: char) {
        #[cfg(test)]
        PRINTED_UNITS.set(PRINTED_UNITS.get() + ch.len_utf16());
        if ('\u{80}'..='\u{9f}').contains(&ch) {
            return;
        }
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
            b'\t' => self.print('\t'),
            _ => {}
        }
    }
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = Some((self.row, self.col)),
            b'8' => {
                if let Some((row, col)) = self.saved {
                    self.move_to(row, col);
                }
            }
            b'M' => self.row = self.row.saturating_sub(1),
            _ => {}
        }
    }
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        if action == 'm' {
            let params = params
                .iter()
                .map(|group| {
                    group
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(":")
                })
                .collect::<Vec<_>>()
                .join(";");
            ansi::Processor::<ansi::StdSyncHandler>::new().advance(
                Arc::make_mut(&mut self.pen),
                format!("{ESC}[{params}m").as_bytes(),
            );
            return;
        }
        if params.iter().any(|group| group.len() > 1) {
            return;
        }
        let mut values = params.iter().map(|group| group[0]);
        let first = values.next().unwrap_or(0);
        let count = first.max(1) as usize;
        match action {
            'A' => self.row = self.row.saturating_sub(count),
            'B' => self.move_to(self.row.saturating_add(count), self.col),
            'C' => self.move_to(self.row, self.col.saturating_add(count)),
            'D' => self.col = self.col.saturating_sub(count),
            'E' => self.move_to(self.row.saturating_add(count), 0),
            'F' => self.move_to(self.row.saturating_sub(count), 0),
            'G' => self.move_to(self.row, count - 1),
            'H' | 'f' => self.move_to(count - 1, values.next().unwrap_or(1).max(1) as usize - 1),
            'K' => self.erase(first),
            'J' => self.clear(first),
            's' => self.saved = Some((self.row, self.col)),
            'u' => {
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
    if let Some(at) = text[line_start..start].rfind("\x1b[") {
        let at = line_start + at;
        if let Some(end) = text[at..].find('m').map(|end| at + end + 1) {
            prefix = &text[at..end];
            start = start.max(end);
        }
    }
    if prefix == "\x1b[0m" {
        prefix = "";
    }
    (format!("{prefix}{}", &text[start..]), true)
}

#[derive(Debug, Clone, Default)]
pub(super) struct Buffer {
    screen: Screen,
    pending: Vec<u8>,
}
impl Buffer {
    pub fn append(&mut self, input: &str) {
        let screen = &mut self.screen;
        let mut parser = vte::Parser::new();
        parser.advance(screen, &self.pending);
        // T3 terminates otherwise-hidden string escapes at a newline too.
        for part in input.split_inclusive('\n') {
            let text = part.strip_suffix('\n').unwrap_or(part);
            parser.advance(screen, text.as_bytes());
            if part.ends_with('\n') {
                let pending = parser.pending_input();
                if pending.starts_with(b"\x1b")
                    && pending.get(1).is_some_and(|byte| b"]PX_^".contains(byte))
                {
                    parser.advance(screen, b"\x1b\\");
                }
                parser.advance(screen, b"\n");
            }
        }
        self.pending = parser.pending_input();
        if self.pending.len() > 64 {
            let hidden = self
                .pending
                .get(1)
                .is_some_and(|byte| b"]PX_^".contains(byte));
            let escaped = self.pending.ends_with(b"\x1b");
            self.pending.truncate(if hidden { 2 } else { 0 });
            if hidden && escaped {
                self.pending.push(27);
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
        resume.push_str(std::str::from_utf8(&self.pending).unwrap_or(""));
        resume
    }
}
