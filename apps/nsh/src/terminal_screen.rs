//! The terminal off Apple platforms: the Host streams a terminal's bytes,
//! alacritty's parser keeps the screen they describe here, and NSH draws its
//! rows as styled text under a view that turns the keyboard, the mouse and the
//! wheel back into the bytes the terminal reads, or scrolls and selects its
//! own lines. Apple platforms hand the same bytes to SwiftTerm.
use agent_core::state::TerminalOutput;
use agent_protocol::operations::{TerminalSize, terminal_color};
use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags},
    vte::ansi::{Color, Processor},
};
use nsh_core::keymap::Modifiers;
use std::{cell::RefCell, rc::Rc};

/// The font the grid is drawn in, and the cell it lays each character in.
pub(crate) const FONT_SIZE: f32 = 13.0;
pub(crate) const LINE_HEIGHT: f32 = 18.0;
/// Consolas' advance is 0.55 of its size.
pub(crate) const CELL_WIDTH: f32 = FONT_SIZE * 0.55;

#[derive(Clone, Copy)]
struct Grid(TerminalSize);

impl Dimensions for Grid {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.0.cols)
    }
}

/// Of what the parser reports back, the screen keeps only the text its
/// program copies (OSC 52); the Host's terminal already answers its program
/// (titles, bells, replies).
#[derive(Clone, Default)]
struct Copied(Rc<RefCell<Vec<String>>>);

impl EventListener for Copied {
    fn send_event(&self, event: Event) {
        if let Event::ClipboardStore(_, text) = event {
            self.0.borrow_mut().push(text);
        }
    }
}

/// Selected cells' background.
const SELECTION: u32 = 0x264f78;
/// Wheel steps sent to a program at once, at most.
const WHEEL_STEPS: u32 = 50;

/// Characters drawn alike, side by side on one row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Run {
    pub text: String,
    pub foreground: u32,
    pub background: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// What the grid shows: its rows' runs and the cursor's cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Frame {
    pub rows: Vec<Vec<Run>>,
    pub cursor: Option<(usize, usize)>,
}

pub(crate) struct Screen {
    term: Term<Copied>,
    copied: Copied,
    parser: Processor,
    size: TerminalSize,
    sequence: u64,
    /// A press went to the program, which hears its release.
    reporting: bool,
    /// A press started a selection the pointer extends.
    selecting: bool,
    /// The part of a line the wheel moved and no step took yet.
    wheel_rest: f32,
}

impl Screen {
    pub(crate) fn new(size: TerminalSize) -> Self {
        let copied = Copied::default();
        Self {
            term: Term::new(Config::default(), &Grid(size), copied.clone()),
            copied,
            parser: Processor::new(),
            size,
            sequence: 0,
            reporting: false,
            selecting: false,
            wheel_rest: 0.0,
        }
    }

    /// Takes the chunks after the last one taken; a chunk with a reset size
    /// replaces the screen first, as the Host's replay of its checkpoint does.
    /// The last sequence taken, when it moved, is the acknowledgment to send.
    pub(crate) fn feed(&mut self, chunks: &[TerminalOutput]) -> Option<u64> {
        let before = self.sequence;
        for chunk in chunks.iter().filter(|chunk| chunk.sequence > before) {
            if let Some(size) = chunk.reset_size {
                self.term = Term::new(Config::default(), &Grid(size), self.copied.clone());
                self.parser = Processor::new();
            }
            self.parser.advance(&mut self.term, &chunk.data);
            self.sequence = chunk.sequence;
        }
        // A replay at the Host's size keeps the size this view lays out.
        if self.term.columns() != usize::from(self.size.cols)
            || self.term.screen_lines() != usize::from(self.size.rows)
        {
            self.term.resize(Grid(self.size));
        }
        (self.sequence > before).then_some(self.sequence)
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(Grid(size));
    }

    pub(crate) fn mode(&self) -> Mode {
        let mode = self.term.mode();
        Mode {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        }
    }

    /// The rows shown, scrolled back `display_offset` lines, with the
    /// selection; the cursor only at the bottom, where the program writes.
    pub(crate) fn frame(&self) -> Frame {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let selection = self
            .term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&self.term));
        let rows = (0..self.term.screen_lines())
            .map(|line| {
                let line = Line(line as i32 - offset);
                let row = &grid[line];
                let mut runs: Vec<Run> = Vec::new();
                for column in 0..self.term.columns() {
                    let cell = &row[Column(column)];
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }
                    let mut foreground = self.color(cell.fg);
                    let mut background = self.color(cell.bg);
                    if cell.flags.contains(Flags::INVERSE) {
                        std::mem::swap(&mut foreground, &mut background);
                    }
                    if selection
                        .is_some_and(|range| range.contains(Point::new(line, Column(column))))
                    {
                        background = SELECTION;
                    }
                    let mut text = if cell.flags.contains(Flags::HIDDEN) {
                        " ".to_owned()
                    } else {
                        cell.c.to_string()
                    };
                    if let Some(marks) = cell.zerowidth() {
                        text.extend(marks);
                    }
                    let run = Run {
                        text,
                        foreground,
                        // The panel's own background shows through the default one.
                        background: (background
                            != self.color(Color::Named(
                                alacritty_terminal::vte::ansi::NamedColor::Background,
                            )))
                        .then_some(background),
                        bold: cell.flags.contains(Flags::BOLD),
                        italic: cell.flags.contains(Flags::ITALIC),
                        underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                        strikethrough: cell.flags.contains(Flags::STRIKEOUT),
                    };
                    match runs.last_mut() {
                        Some(last)
                            if Run {
                                text: last.text.clone(),
                                ..run.clone()
                            } == *last =>
                        {
                            last.text.push_str(&run.text)
                        }
                        _ => runs.push(run),
                    }
                }
                // Trailing blanks in the default look draw nothing.
                if let Some(last) = runs.last_mut()
                    && last.background.is_none()
                    && !last.underline
                    && !last.strikethrough
                {
                    last.text.truncate(last.text.trim_end_matches(' ').len());
                    if last.text.is_empty() {
                        runs.pop();
                    }
                }
                runs
            })
            .collect();
        let cursor = self.term.renderable_content().cursor.point;
        Frame {
            rows,
            cursor: (offset == 0 && self.term.mode().contains(TermMode::SHOW_CURSOR)).then_some((
                usize::try_from(cursor.line.0).unwrap_or_default(),
                cursor.column.0,
            )),
        }
    }

    fn color(&self, color: Color) -> u32 {
        let index = match color {
            Color::Spec(rgb) => {
                return (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
            }
            Color::Indexed(index) => usize::from(index),
            Color::Named(name) => name as usize,
        };
        match self.term.colors()[index] {
            Some(rgb) => (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b),
            None => terminal_color(index as u16),
        }
    }
}

/// The terminal modes that change what keys and pastes send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mode {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
}

/// The keys the terminal takes beside the text typed into its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Backspace,
    Escape,
    Tab,
    BackTab,
    /// An arrow, Home or End, with xterm's modifier number: 1, plus 1 for
    /// Shift, 2 for Alt and 4 for Ctrl.
    Cursor(Cursor, u8),
    PageUp,
    PageDown,
    Delete,
    /// F1 to F12.
    Function(u8),
    /// Ctrl and a letter: its control character.
    Control(char),
    /// Alt and a character: Escape, then the character.
    Alt(char),
}

/// The keys that move the cursor, by their final byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cursor {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
}

impl Cursor {
    const ALL: [(&str, Self); 6] = [
        ("up", Self::Up),
        ("down", Self::Down),
        ("right", Self::Right),
        ("left", Self::Left),
        ("home", Self::Home),
        ("end", Self::End),
    ];

    fn suffix(self) -> char {
        match self {
            Self::Up => 'A',
            Self::Down => 'B',
            Self::Right => 'C',
            Self::Left => 'D',
            Self::Home => 'H',
            Self::End => 'F',
        }
    }
}

impl Key {
    /// The keystrokes NSH reports for each key, as its keymap names them.
    /// Ctrl+Q and F11 stay the platform's quit and full-screen keys, and
    /// Ctrl+V the input's paste.
    pub(crate) fn bindings() -> Vec<(String, Self)> {
        let mut keys = vec![
            ("escape".to_owned(), Self::Escape),
            ("tab".into(), Self::Tab),
            ("shift+tab".into(), Self::BackTab),
            ("pageup".into(), Self::PageUp),
            ("pagedown".into(), Self::PageDown),
            ("delete".into(), Self::Delete),
        ];
        for modifier in 1..=8u8 {
            let held = modifier - 1;
            let prefix = [(4, "ctrl+"), (2, "alt+"), (1, "shift+")]
                .into_iter()
                .filter(|(bit, _)| held & bit != 0)
                .map(|(_, name)| name)
                .collect::<String>();
            keys.extend(
                Cursor::ALL
                    .map(|(name, key)| (format!("{prefix}{name}"), Self::Cursor(key, modifier))),
            );
        }
        keys.extend(
            (1..=12u8)
                .filter(|number| *number != 11)
                .map(|number| (format!("f{number}"), Self::Function(number))),
        );
        keys.extend(
            ('a'..='z')
                .filter(|letter| !matches!(letter, 'q' | 'v'))
                .map(|letter| (format!("ctrl+{letter}"), Self::Control(letter))),
        );
        keys.extend(
            ('a'..='z')
                .chain('0'..='9')
                .map(|character| (format!("alt+{character}"), Self::Alt(character))),
        );
        keys
    }

    /// The bytes a VT terminal reads for the key.
    pub(crate) fn bytes(self, mode: Mode) -> Vec<u8> {
        match self {
            Self::Backspace => b"\x7f".to_vec(),
            Self::Escape => b"\x1b".to_vec(),
            Self::Tab => b"\t".to_vec(),
            Self::BackTab => b"\x1b[Z".to_vec(),
            Self::Cursor(key, 1) if mode.app_cursor => {
                format!("\x1bO{}", key.suffix()).into_bytes()
            }
            Self::Cursor(key, 1) => format!("\x1b[{}", key.suffix()).into_bytes(),
            Self::Cursor(key, modifier) => {
                format!("\x1b[1;{modifier}{}", key.suffix()).into_bytes()
            }
            Self::PageUp => b"\x1b[5~".to_vec(),
            Self::PageDown => b"\x1b[6~".to_vec(),
            Self::Delete => b"\x1b[3~".to_vec(),
            Self::Function(number @ 1..=4) => {
                format!("\x1bO{}", char::from(b'P' + number - 1)).into_bytes()
            }
            Self::Function(number) => {
                let code = match number {
                    5 => 15,
                    6..=10 => number + 11,
                    _ => number + 12,
                };
                format!("\x1b[{code}~").into_bytes()
            }
            Self::Control(letter) => vec![(letter as u8) & 0x1f],
            Self::Alt(character) => format!("\x1b{character}").into_bytes(),
        }
    }
}

/// The cell under a point of the grid.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn cell(x: f32, y: f32) -> (usize, usize) {
    (
        (x / CELL_WIDTH).floor().max(0.0) as usize,
        (y / LINE_HEIGHT).floor().max(0.0) as usize,
    )
}

/// The pointer and the wheel: a program that listens to the mouse hears them
/// unless Shift is held; otherwise they select and scroll the screen's own
/// lines. Each returns what to send the program.
impl Screen {
    /// The left button went down on `cell`, the `clicks`th time in a row.
    pub(crate) fn press(&mut self, cell: (usize, usize), clicks: u32, held: Modifiers) -> Vec<u8> {
        if self.term.mode().intersects(TermMode::MOUSE_MODE) && !held.shift {
            self.reporting = true;
            return self.report(0, cell, held, false);
        }
        self.selecting = true;
        let kind = match clicks {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.selection = Some(Selection::new(kind, self.point(cell), Side::Left));
        Vec::new()
    }

    /// The pointer moved over `cell`.
    pub(crate) fn hover(&mut self, cell: (usize, usize), held: Modifiers) -> Vec<u8> {
        if self.selecting {
            let point = self.point(cell);
            if let Some(selection) = self.term.selection.as_mut() {
                selection.update(point, Side::Right);
            }
        }
        let mode = self.term.mode();
        if self.reporting && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
            self.report(32, cell, held, false)
        } else if !self.reporting && mode.contains(TermMode::MOUSE_MOTION) && !held.shift {
            self.report(35, cell, held, false)
        } else {
            Vec::new()
        }
    }

    /// The left button went up over `cell`.
    pub(crate) fn release(&mut self, cell: (usize, usize), held: Modifiers) -> Vec<u8> {
        self.selecting = false;
        if std::mem::take(&mut self.reporting) {
            self.report(0, cell, held, true)
        } else {
            Vec::new()
        }
    }

    /// The wheel moved `dy` points toward the end (down) over `cell`: wheel
    /// steps to a program that listens, arrows to a full-screen one, or the
    /// screen's own scrollback.
    pub(crate) fn wheel(&mut self, dy: f32, cell: (usize, usize), held: Modifiers) -> Vec<u8> {
        self.wheel_rest += dy / LINE_HEIGHT;
        let lines = self.wheel_rest.trunc();
        self.wheel_rest -= lines;
        let (up, steps) = (lines < 0.0, (lines.abs() as u32).min(WHEEL_STEPS));
        if steps == 0 {
            return Vec::new();
        }
        let mode = self.term.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !held.shift {
            let button = if up { 64 } else { 65 };
            (0..steps)
                .flat_map(|_| self.report(button, cell, held, false))
                .collect()
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !held.shift {
            let arrow = Key::Cursor(if up { Cursor::Up } else { Cursor::Down }, 1);
            arrow.bytes(self.mode()).repeat(steps as usize)
        } else {
            let steps = steps as i32;
            self.term
                .scroll_display(Scroll::Delta(if up { steps } else { -steps }));
            Vec::new()
        }
    }

    /// The selected text, if any.
    pub(crate) fn selection(&self) -> Option<String> {
        self.term
            .selection
            .as_ref()
            .filter(|selection| !selection.is_empty())
            .and_then(|_| self.term.selection_to_string())
    }

    /// Ends the selection; true when there was one.
    pub(crate) fn clear_selection(&mut self) -> bool {
        self.selecting = false;
        self.term.selection.take().is_some()
    }

    /// Back to the lines the program writes, as typing does.
    pub(crate) fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// The text the program copied since the last call.
    pub(crate) fn take_copied(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.copied.0.borrow_mut())
    }

    /// The grid point of a shown cell.
    fn point(&self, (column, row): (usize, usize)) -> Point {
        let row = row.min(self.term.screen_lines() - 1) as i32;
        Point::new(
            Line(row - self.term.grid().display_offset() as i32),
            Column(column.min(self.term.columns() - 1)),
        )
    }

    /// Mouse `button` (0 the left one, 32 a drag, 35 a move, 64 and 65 the
    /// wheel) at `cell`, as the program asked to hear it.
    fn report(
        &self,
        button: u8,
        (column, row): (usize, usize),
        held: Modifiers,
        released: bool,
    ) -> Vec<u8> {
        let column = column.min(self.term.columns() - 1) + 1;
        let row = row.min(self.term.screen_lines() - 1) + 1;
        let code =
            button + u8::from(held.shift) * 4 + u8::from(held.alt) * 8 + u8::from(held.ctrl) * 16;
        let mode = self.term.mode();
        if mode.contains(TermMode::SGR_MOUSE) {
            let end = if released { 'm' } else { 'M' };
            return format!("\x1b[<{code};{column};{row}{end}").into_bytes();
        }
        let code = if released { 3 } else { code };
        if mode.contains(TermMode::UTF8_MOUSE) {
            let position = |value: usize| char::from_u32(value as u32 + 32).unwrap_or(' ');
            return format!(
                "\x1b[M{}{}{}",
                char::from(code + 32),
                position(column),
                position(row)
            )
            .into_bytes();
        }
        // The classic encoding has a byte for each coordinate.
        if column > 223 || row > 223 {
            return Vec::new();
        }
        vec![
            0x1b,
            b'[',
            b'M',
            code + 32,
            column as u8 + 32,
            row as u8 + 32,
        ]
    }
}
/// Typed or pasted text as the terminal reads it: line breaks as returns, or
/// the whole text marked as a paste when the program asked for that. Escapes
/// cannot reach the program from pasted text.
pub(crate) fn text_bytes(text: &str, mode: Mode) -> Vec<u8> {
    let text = text.replace('\x1b', "");
    if mode.bracketed_paste && text.contains('\n') {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// The grid that fills `width` × `height` points, as the Host accepts it.
pub(crate) fn fit(width: f32, height: f32) -> Option<TerminalSize> {
    let cols = (width / CELL_WIDTH).floor();
    let rows = (height / LINE_HEIGHT).floor();
    (cols.is_finite() && rows.is_finite())
        .then(|| agent_core::presentation::terminals::terminal_grid(cols as i32, rows as i32))
        .flatten()
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) use draw::grid;

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
mod draw {
    use super::{CELL_WIDTH, FONT_SIZE, LINE_HEIGHT};
    use super::{Frame, Key, Mode, Run, Screen, cell, fit, text_bytes};
    use crate::{shared::Shared, terminal::Report, ui::colors};
    use agent_core::state::TerminalOutput;
    use agent_protocol::operations::TerminalSize;
    use nsh_core::{element::rgb, keymap::Modifiers, prelude::*};
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    fn row(runs: Vec<Run>, cursor: Option<usize>) -> AnyElement {
        let mut line = text("")
            .font_family(SharedString::from("monospace"))
            .text_size(px(FONT_SIZE))
            .line_height(nsh_core::LineHeight::Points(px(LINE_HEIGHT)))
            .number_of_lines(1)
            .h(px(LINE_HEIGHT));
        for run in runs {
            let mut span = text(run.text).text_color(rgb(run.foreground));
            if let Some(background) = run.background {
                span = span.bg(rgb(background));
            }
            if run.bold {
                span = span.font_weight(FontWeight::BOLD);
            }
            if run.italic {
                span = span.italic();
            }
            if run.underline {
                span = span.underline();
            }
            if run.strikethrough {
                span = span.line_through();
            }
            line = line.child(span);
        }
        view()
            .h(px(LINE_HEIGHT))
            .child(line)
            .child(cursor.map(|column| {
                view()
                    .absolute()
                    .top_0()
                    .left(px(column as f32 * CELL_WIDTH))
                    .w(px(2.0))
                    .h(px(LINE_HEIGHT))
                    .bg(colors::foreground())
            }))
            .into_element()
    }

    /// The grid of `output`, typing into the Host's terminal while `accepts`.
    pub(crate) fn grid(
        accepts: impl Fn() -> bool + Copy + 'static,
        output: impl Fn() -> Vec<TerminalOutput> + 'static,
        report: impl Fn(Report) + 'static,
    ) -> impl IntoElement {
        // Kept by the view, so each of its handlers can reach it.
        let report = stored(Rc::new(report) as Rc<dyn Fn(Report)>);
        let screen = Rc::new(RefCell::new(Screen::new(TerminalSize {
            cols: 80,
            rows: 24,
        })));
        let frame = signal(Shared(Arc::new(Frame::default())));
        let mode = signal(Mode::default());
        let focused = signal(false);
        let typed = signal(String::new());
        let field = element_ref();
        {
            let screen = screen.clone();
            effect(move || {
                let chunks = output();
                let mut screen = screen.borrow_mut();
                if let Some(sequence) = screen.feed(&chunks) {
                    frame.set(Shared(Arc::new(screen.frame())));
                    mode.set(screen.mode());
                    (report.get())(Report::Ack(sequence));
                }
                for text in screen.take_copied() {
                    set_clipboard(text);
                }
            });
        }
        let send = move |bytes: Vec<u8>| {
            if !bytes.is_empty() {
                (report.get())(Report::Input(bytes));
            }
        };
        // Runs `change` on the screen, draws what it shows then if that
        // changed (a selection, the scrollback) and sends what it returns.
        let update = Rc::new({
            let screen = screen.clone();
            move |change: &dyn Fn(&mut Screen) -> Vec<u8>| {
                let mut screen = screen.borrow_mut();
                let bytes = change(&mut screen);
                let shown = screen.frame();
                drop(screen);
                if frame.with_untracked(|frame| *frame.0 != shown) {
                    frame.set(Shared(Arc::new(shown)));
                }
                send(bytes);
            }
        });
        // What is typed goes where the program writes.
        let typing = Rc::new({
            let update = update.clone();
            move |bytes: Vec<u8>| {
                update(&move |screen| {
                    screen.scroll_to_bottom();
                    bytes.clone()
                })
            }
        });
        let copy = Rc::new({
            let update = update.clone();
            let screen = screen.clone();
            move || {
                let Some(text) = screen.borrow().selection() else {
                    return false;
                };
                set_clipboard(text);
                update(&|screen| {
                    screen.clear_selection();
                    Vec::new()
                });
                true
            }
        });
        let resize = {
            let screen = screen.clone();
            move |bounds: &Bounds<Points>| {
                let Some(size) = fit(bounds.size.width.0, bounds.size.height.0) else {
                    return;
                };
                let mut screen = screen.borrow_mut();
                if screen.size != size {
                    screen.resize(size);
                    frame.set(Shared(Arc::new(screen.frame())));
                    (report.get())(Report::Resize(size));
                }
            }
        };
        let pointer = {
            let (press, hover, release, wheel) = (
                update.clone(),
                update.clone(),
                update.clone(),
                update.clone(),
            );
            crate::native::wheel_area(move |scroll: crate::native::Scroll| {
                let held = Modifiers {
                    shift: scroll.shift,
                    ctrl: scroll.ctrl,
                    alt: scroll.alt,
                    ..Modifiers::default()
                };
                wheel(&|screen| screen.wheel(scroll.dy, cell(scroll.x, scroll.y), held));
            })
            .absolute()
            .inset_0()
            .on_press_down(move |event: &PressEvent| {
                field.focus();
                let at = cell(event.position.x.0, event.position.y.0);
                press(&|screen| screen.press(at, event.clicks, event.modifiers));
            })
            .on_mouse_move(move |event: &MouseEvent| {
                let at = cell(event.position.x.0, event.position.y.0);
                hover(&|screen| screen.hover(at, event.modifiers));
            })
            .on_press_up(move |event: &PressEvent| {
                let at = cell(event.position.x.0, event.position.y.0);
                release(&|screen| screen.release(at, event.modifiers));
            })
        };
        view()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .p_2()
            .bg(colors::BACKGROUND)
            .label(crate::l10n::tr("ターミナル"))
            .child(
                view()
                    .flex_1()
                    .min_h_0()
                    .on_layout(resize)
                    .child(each(
                        move || {
                            frame.with(|frame| {
                                frame
                                    .rows
                                    .iter()
                                    .cloned()
                                    .enumerate()
                                    .map(|(index, runs)| {
                                        let cursor = frame
                                            .cursor
                                            .filter(|(line, _)| *line == index && focused.get())
                                            .map(|(_, column)| column);
                                        (index, runs, cursor)
                                    })
                                    .collect::<Vec<_>>()
                            })
                        },
                        |(index, runs, cursor): &(usize, Vec<Run>, Option<usize>)| {
                            use std::hash::{Hash, Hasher};
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            (runs, cursor).hash(&mut hasher);
                            (*index, hasher.finish())
                        },
                        |row_data, _| {
                            let (_, runs, cursor) = row_data.get_untracked();
                            row(runs, cursor)
                        },
                    ))
                    .child(pointer),
            )
            // The keyboard's target: it takes typed and pasted text,
            // composition included. What it holds stays selected, so each
            // change is only the text just typed.
            .child({
                let typing = typing.clone();
                let erase = typing.clone();
                nsh_core::element::input()
                    .element_ref(&field)
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(1.0))
                    .h(px(1.0))
                    .opacity(0.0)
                    .caret(false)
                    .readonly(move || !accepts())
                    .multiline(true)
                    .auto_correct(false)
                    .auto_capitalize(AutoCapitalize::None)
                    .value(move || typed.get())
                    .on_focus(move || {
                        focused.set(true);
                        (report.get())(Report::Focus(true));
                    })
                    .on_blur(move || {
                        focused.set(false);
                        (report.get())(Report::Focus(false));
                    })
                    .on_change(move |text: &str| {
                        typed.set(text.to_owned());
                        if !text.is_empty() {
                            typing(text_bytes(text, mode.get_untracked()));
                            field.set_selection(0, text.encode_utf16().count() as u32);
                        }
                    })
                    .on_key_press(move |key: &str| {
                        if key == "Backspace" {
                            erase(Key::Backspace.bytes(mode.get_untracked()));
                        }
                    })
            })
            // Keys that do not type text, bound only while the grid has the keyboard.
            .child(when(
                move || focused.get(),
                move || {
                    let options = || KeyOptions {
                        when: nsh_core::keymap::When::Always,
                        repeat: true,
                    };
                    for (keystroke, key) in Key::bindings() {
                        let (typing, copy) = (typing.clone(), copy.clone());
                        on_key_with(&keystroke, options(), move |_| {
                            // Ctrl+C copies a selection, as Windows' terminals do.
                            if key == Key::Control('c') && copy() {
                                return;
                            }
                            typing(key.bytes(mode.get_untracked()));
                        });
                    }
                    let copy = copy.clone();
                    on_key_with("ctrl+shift+c", options(), move |_| {
                        copy();
                    });
                    view()
                },
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(sequence: u64, data: &[u8], reset: Option<TerminalSize>) -> TerminalOutput {
        TerminalOutput {
            sequence,
            data: data.to_vec(),
            reset_size: reset,
        }
    }

    fn text(frame: &Frame, row: usize) -> String {
        frame.rows[row]
            .iter()
            .map(|run| run.text.as_str())
            .collect()
    }

    #[test]
    fn a_screen_takes_each_chunk_once_and_acknowledges_the_last_one() {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 });
        let first = [output(1, b"ab", None), output(2, b"c\r\nnext", None)];
        assert_eq!(screen.feed(&first), Some(2));
        // The publication still holds the taken chunks; nothing repeats.
        assert_eq!(screen.feed(&first), None);
        let frame = screen.frame();
        assert_eq!(text(&frame, 0), "abc");
        assert_eq!(text(&frame, 1), "next");
        assert_eq!(frame.cursor, Some((1, 4)));
    }

    #[test]
    fn a_reset_replaces_the_screen_and_keeps_the_size_laid_out() {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 });
        screen.feed(&[output(1, b"old", None)]);
        let host = TerminalSize { cols: 10, rows: 2 };
        assert_eq!(screen.feed(&[output(2, b"new", Some(host))]), Some(2));
        let frame = screen.frame();
        assert_eq!(frame.rows.len(), 3);
        assert_eq!(text(&frame, 0), "new");
    }

    #[test]
    fn styles_split_runs_and_colors_resolve_through_the_shared_palette() {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 1 });
        screen.feed(&[output(1, b"a\x1b[1;31mbc\x1b[0m\x1b[44md", None)]);
        let runs = &screen.frame().rows[0];
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].text, "bc");
        assert!(runs[1].bold);
        assert_eq!(runs[1].foreground, terminal_color(1));
        assert_eq!(runs[2].background, Some(terminal_color(4)));
        assert_eq!(runs[0].background, None);
    }

    #[test]
    fn keys_follow_the_cursor_mode_the_program_selected() {
        let up = Key::Cursor(Cursor::Up, 1);
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 1 });
        assert_eq!(up.bytes(screen.mode()), b"\x1b[A");
        screen.feed(&[output(1, b"\x1b[?1h", None)]);
        assert_eq!(up.bytes(screen.mode()), b"\x1bOA");
        assert_eq!(Key::Control('c').bytes(screen.mode()), [3]);
        // Modified cursor keys stay in xterm's CSI form in either mode.
        assert_eq!(
            Key::Cursor(Cursor::Left, 5).bytes(screen.mode()),
            b"\x1b[1;5D"
        );
        let keystrokes = Key::bindings()
            .into_iter()
            .map(|(keystroke, _)| keystroke)
            .collect::<Vec<_>>();
        for kept in ["ctrl+q", "ctrl+v", "f11"] {
            assert!(!keystrokes.iter().any(|keystroke| keystroke == kept));
        }
        let unique = keystrokes.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            keystrokes.len(),
            "each keystroke sends one key"
        );
        // The grid binds each of them on Windows, which panics on one it
        // cannot parse or the platform keeps.
        use nsh_core::{keymap::Keystroke, platform::Os};
        for keystroke in keystrokes
            .iter()
            .map(String::as_str)
            .chain(["ctrl+shift+c"])
        {
            let parsed = Keystroke::parse(keystroke, Os::Windows).unwrap();
            assert_eq!(parsed.reserved(Os::Windows), None, "{keystroke}");
        }
    }

    #[test]
    fn modified_function_and_alt_keys_send_xterm_sequences() {
        let mode = Mode::default();
        let key = |keystroke: &str| {
            Key::bindings()
                .into_iter()
                .find(|(bound, _)| bound == keystroke)
                .map(|(_, key)| key.bytes(mode))
        };
        assert_eq!(key("shift+up").unwrap(), b"\x1b[1;2A");
        assert_eq!(key("alt+right").unwrap(), b"\x1b[1;3C");
        assert_eq!(key("ctrl+shift+home").unwrap(), b"\x1b[1;6H");
        assert_eq!(key("ctrl+alt+shift+end").unwrap(), b"\x1b[1;8F");
        assert_eq!(key("f1").unwrap(), b"\x1bOP");
        assert_eq!(key("f4").unwrap(), b"\x1bOS");
        assert_eq!(key("f5").unwrap(), b"\x1b[15~");
        assert_eq!(key("f6").unwrap(), b"\x1b[17~");
        assert_eq!(key("f10").unwrap(), b"\x1b[21~");
        assert_eq!(key("f12").unwrap(), b"\x1b[24~");
        assert_eq!(key("alt+b").unwrap(), b"\x1bb");
        assert_eq!(key("alt+7").unwrap(), b"\x1b7");
    }

    #[test]
    fn the_wheel_scrolls_back_and_typing_returns_to_the_program() {
        let mut screen = Screen::new(TerminalSize { cols: 10, rows: 2 });
        screen.feed(&[output(1, b"1\r\n2\r\n3\r\n4", None)]);
        let none = Modifiers::default();
        // Half a line waits for the rest of it.
        assert!(screen.wheel(-LINE_HEIGHT / 2.0, (0, 0), none).is_empty());
        assert_eq!(text(&screen.frame(), 0), "3");
        assert!(screen.wheel(-LINE_HEIGHT / 2.0, (0, 0), none).is_empty());
        let frame = screen.frame();
        assert_eq!((text(&frame, 0), text(&frame, 1)), ("2".into(), "3".into()));
        assert_eq!(
            frame.cursor, None,
            "the cursor stays with the program's lines"
        );
        screen.scroll_to_bottom();
        assert_eq!(text(&screen.frame(), 1), "4");
        // A full-screen program that asks for arrows gets them instead.
        screen.feed(&[output(2, b"\x1b[?1049h\x1b[?1007h", None)]);
        assert_eq!(
            screen.wheel(-LINE_HEIGHT * 2.0, (0, 0), none),
            b"\x1b[A\x1b[A"
        );
        assert_eq!(screen.wheel(LINE_HEIGHT, (0, 0), none), b"\x1b[B");
    }

    #[test]
    fn a_program_that_listens_hears_the_mouse_unless_shift_is_held() {
        let mut screen = Screen::new(TerminalSize { cols: 10, rows: 3 });
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        // Clicks and drags, reported as SGR.
        screen.feed(&[output(1, b"\x1b[?1002h\x1b[?1006h", None)]);
        assert_eq!(screen.press((2, 1), 1, none), b"\x1b[<0;3;2M");
        assert_eq!(screen.hover((4, 1), none), b"\x1b[<32;5;2M");
        assert_eq!(screen.release((4, 1), none), b"\x1b[<0;5;2m");
        assert!(
            screen.hover((5, 1), none).is_empty(),
            "a drag ends with its release"
        );
        assert_eq!(screen.wheel(-LINE_HEIGHT, (0, 0), none), b"\x1b[<64;1;1M");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(screen.wheel(LINE_HEIGHT, (0, 0), ctrl), b"\x1b[<81;1;1M");
        // Shift selects the screen's own text instead.
        assert!(screen.press((0, 0), 1, shift).is_empty());
        assert!(screen.release((0, 0), shift).is_empty());
        // The classic encoding: a byte per value, offset by 32.
        let mut classic = Screen::new(TerminalSize { cols: 10, rows: 3 });
        classic.feed(&[output(1, b"\x1b[?1000h", None)]);
        assert_eq!(
            classic.press((0, 2), 1, none),
            [0x1b, b'[', b'M', 32, 33, 35]
        );
        assert_eq!(
            classic.release((0, 2), none),
            [0x1b, b'[', b'M', 35, 33, 35]
        );
    }

    #[test]
    fn a_drag_selects_text_to_copy_and_programs_copy_through_osc_52() {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 2 });
        screen.feed(&[output(1, b"hello world\r\nnext", None)]);
        let none = Modifiers::default();
        assert!(screen.press((0, 0), 1, none).is_empty());
        assert_eq!(screen.selection(), None, "a click selects nothing");
        screen.hover((4, 0), none);
        screen.release((4, 0), none);
        assert_eq!(screen.selection().as_deref(), Some("hello"));
        let frame = screen.frame();
        assert_eq!(frame.rows[0][0].text, "hello");
        assert_eq!(frame.rows[0][0].background, Some(SELECTION));
        screen.hover((10, 0), none);
        assert_eq!(
            screen.selection().as_deref(),
            Some("hello"),
            "the release ended it"
        );
        assert!(screen.clear_selection());
        assert_eq!(screen.selection(), None);
        // A double click takes the word.
        screen.press((7, 0), 2, none);
        screen.release((7, 0), none);
        assert_eq!(screen.selection().as_deref(), Some("world"));
        // OSC 52 with "copy" in base64.
        screen.feed(&[output(2, b"\x1b]52;c;Y29weQ==\x07", None)]);
        assert_eq!(screen.take_copied(), ["copy"]);
        assert!(screen.take_copied().is_empty());
    }

    #[test]
    fn pasted_text_cannot_inject_escapes_and_is_marked_when_asked() {
        let plain = Mode::default();
        assert_eq!(text_bytes("a\nb\x1b[2J", plain), b"a\rb[2J");
        let marked = Mode {
            bracketed_paste: true,
            ..Mode::default()
        };
        assert_eq!(text_bytes("a\nb", marked), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(text_bytes("x", marked), b"x");
    }

    #[test]
    fn a_resized_screen_draws_its_new_grid() {
        let mut screen = Screen::new(TerminalSize { cols: 20, rows: 3 });
        screen.feed(&[output(1, b"abc", None)]);
        screen.resize(TerminalSize { cols: 10, rows: 5 });
        let frame = screen.frame();
        assert_eq!(frame.rows.len(), 5);
        assert_eq!(text(&frame, 0), "abc");
        assert_eq!(Key::Backspace.bytes(screen.mode()), [0x7f]);
    }

    #[test]
    fn the_grid_fills_whole_cells_within_the_host_limits() {
        let size = fit(CELL_WIDTH * 80.5, LINE_HEIGHT * 24.9).unwrap();
        assert_eq!((size.cols, size.rows), (80, 24));
        assert_eq!(fit(0.0, 0.0), None);
    }
}
