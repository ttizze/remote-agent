use crate::Runtime;
use agent_core::state::{Intent, Snapshot, TerminalPhase};
use agent_core::store::Store;
use agent_protocol::operations::TerminalSize;
use alacritty_terminal::{
    Term,
    event::{Event as TerminalEvent, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as CellPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags},
    vte::ansi::{Color as TerminalColor, Processor},
};
use gpui_kit::{
    component::{h_flex, v_flex},
    *,
};
use std::{ops::Range, sync::Arc};

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 18.;
#[derive(Clone, Copy)]
struct GridSize(TerminalSize);
impl Dimensions for GridSize {
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
#[derive(Clone)]
struct TerminalEvents(async_channel::Sender<Event>);
impl EventListener for TerminalEvents {
    fn send_event(&self, event: TerminalEvent) {
        if let TerminalEvent::ClipboardStore(_, text) = event {
            let _ = self.0.try_send(Event::Clipboard(text));
        }
    }
}
enum Event {
    Snapshot(Arc<Snapshot>),
    Clipboard(String),
    Error(String),
}

pub(crate) struct Terminal {
    store: Arc<Store>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    events: async_channel::Sender<Event>,
    handle: String,
    cwd: String,
    started: bool,
    sequence: u64,
    size: TerminalSize,
    term: Term<TerminalEvents>,
    parser: Processor,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    composition: String,
    composition_selection: Range<usize>,
    selecting: bool,
    mouse_pressed: bool,
    error: Option<String>,
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let store = self.store.clone();
        let handle = self.handle.clone();
        self.runtime.handle.spawn(async move {
            let _ = store.dispatch(Intent::DetachTerminal { handle }).await;
        });
    }
}
impl Terminal {
    pub(crate) fn new(
        store: Arc<Store>,
        cwd: String,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let (events, incoming) = async_channel::unbounded();
        let runtime = cx.global::<Runtime>().clone();
        let updates = events.clone();
        let mut snapshots = store.subscribe();
        runtime.handle.spawn(async move {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                if updates.send(Event::Snapshot(snapshot)).await.is_err()
                    || snapshots.changed().await.is_err()
                {
                    break;
                }
            }
        });
        let size = TerminalSize { cols: 80, rows: 24 };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        cx.new(|cx: &mut Context<Self>| {
            cx.spawn_in(window, async move |view, cx| {
                while let Ok(event) = incoming.recv().await {
                    if view
                        .update_in(cx, |view, _, cx| view.event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Self {
                store,
                snapshot: Arc::default(),
                runtime,
                handle: agent_core::client::terminal_handle(cwd.clone()),
                cwd,
                started: false,
                sequence: 0,
                size,
                term: Term::new(
                    Config::default(),
                    &GridSize(size),
                    TerminalEvents(events.clone()),
                ),
                parser: Processor::new(),
                events,
                focus,
                bounds: Bounds::default(),
                cell_width: px(8.),
                composition: String::new(),
                composition_selection: 0..0,
                selecting: false,
                mouse_pressed: false,
                error: None,
            }
        })
    }
    fn dispatch(&self, intent: Intent) {
        {
            let receipt = self.store.dispatch(intent);
            let events = self.events.clone();
            self.runtime.handle.spawn(async move {
                if let Err(error) = receipt
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()))
                {
                    let _ = events.send(Event::Error(error.to_string())).await;
                }
            });
        }
    }
    fn input(&mut self, data: Vec<u8>, cx: &mut Context<Self>) {
        if data.is_empty() {
            return;
        }
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.dispatch(Intent::WriteTerminal {
            handle: self.handle.clone(),
            data,
        });
        cx.notify();
    }
    fn event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Error(error) => self.error = Some(error),
            Event::Snapshot(snapshot) => self.snapshot = snapshot,
            Event::Clipboard(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        }
        if self.snapshot.connected && !self.started {
            self.started = true;
            self.dispatch(Intent::StartTerminal {
                handle: self.handle.clone(),
                cwd: self.cwd.clone(),
                cols: self.size.cols,
                rows: self.size.rows,
            });
        }
        if let Some(terminal) = self.snapshot.terminals.get(&self.handle) {
            let chunks: Vec<_> = terminal
                .output
                .iter()
                .filter(|chunk| chunk.sequence > self.sequence)
                .cloned()
                .collect();
            for chunk in chunks {
                if let Some(size) = chunk.reset_size {
                    self.parser = Processor::new();
                    self.term = Term::new(
                        Config::default(),
                        &GridSize(size),
                        TerminalEvents(self.events.clone()),
                    );
                }
                self.parser.advance(&mut self.term, &chunk.data);
                self.sequence = chunk.sequence;
            }
            if self.term.columns() != self.size.cols as usize
                || self.term.screen_lines() != self.size.rows as usize
            {
                self.resize(self.size);
            }
        }
        cx.notify();
    }
    fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        self.term.resize(GridSize(size));
        if self
            .snapshot
            .terminals
            .get(&self.handle)
            .is_some_and(|terminal| terminal.phase == TerminalPhase::Running)
        {
            self.dispatch(Intent::ResizeTerminal {
                handle: self.handle.clone(),
                cols: size.cols,
                rows: size.rows,
            });
        }
    }
    fn paste(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.replace('\x1b', "");
        let text = if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r")
        };
        self.input(text.into_bytes(), cx);
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let mods = key.modifiers;
        if mods.platform && key.key == "c" {
            if let Some(text) = self.term.selection_to_string() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        if mods.platform && key.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste(text, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.composition.is_empty() {
            return;
        }
        let arrow = if self.term.mode().contains(TermMode::APP_CURSOR) {
            "\x1bO"
        } else {
            "\x1b["
        };
        let data = match key.key.as_str() {
            "enter" => Some("\r".into()),
            "backspace" => Some("\x7f".into()),
            "escape" => Some("\x1b".into()),
            "tab" => Some(if mods.shift { "\x1b[Z" } else { "\t" }.into()),
            "up" | "down" | "right" | "left" | "home" | "end" => {
                let suffix = match key.key.as_str() {
                    "up" => 'A',
                    "down" => 'B',
                    "right" => 'C',
                    "left" => 'D',
                    "home" => 'H',
                    _ => 'F',
                };
                let modifier = 1
                    + usize::from(mods.shift)
                    + 2 * usize::from(mods.alt)
                    + 4 * usize::from(mods.control);
                Some(if modifier == 1 {
                    format!("{arrow}{suffix}")
                } else {
                    format!("\x1b[1;{modifier}{suffix}")
                })
            }
            "f1" | "f2" | "f3" | "f4" => Some(format!(
                "\x1bO{}",
                match key.key.as_str() {
                    "f1" => 'P',
                    "f2" => 'Q',
                    "f3" => 'R',
                    _ => 'S',
                }
            )),
            "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => Some(format!(
                "\x1b[{}~",
                match key.key.as_str() {
                    "f5" => 15,
                    "f6" => 17,
                    "f7" => 18,
                    "f8" => 19,
                    "f9" => 20,
                    "f10" => 21,
                    "f11" => 23,
                    _ => 24,
                }
            )),
            "delete" => Some("\x1b[3~".into()),
            "pageup" => Some("\x1b[5~".into()),
            "pagedown" => Some("\x1b[6~".into()),
            value if mods.control && value.len() == 1 => {
                Some(char::from(value.as_bytes()[0] & 0x1f).to_string())
            }
            value if mods.alt && !mods.platform && value.len() == 1 => Some(format!("\x1b{value}")),
            _ => None,
        };
        if let Some(data) = data {
            self.input(data.into_bytes(), cx);
            cx.stop_propagation();
        }
        let _ = window;
    }
    fn mouse_report(
        &self,
        button: u8,
        position: Point<Pixels>,
        modifiers: Modifiers,
        released: bool,
    ) {
        let col = ((position.x - self.bounds.left()) / self.cell_width)
            .floor()
            .max(0.) as usize
            + 1;
        let row = ((position.y - self.bounds.top()) / px(LINE_HEIGHT))
            .floor()
            .max(0.) as usize
            + 1;
        let col = col.min(self.term.columns());
        let row = row.min(self.term.screen_lines());
        let code = button
            + u8::from(modifiers.shift) * 4
            + u8::from(modifiers.alt) * 8
            + u8::from(modifiers.control) * 16;
        let data = if self.term.mode().contains(TermMode::SGR_MOUSE) {
            format!(
                "\x1b[<{code};{col};{row}{}",
                if released { 'm' } else { 'M' }
            )
            .into_bytes()
        } else {
            let code = if released { 3 } else { code };
            if self.term.mode().contains(TermMode::UTF8_MOUSE) {
                format!(
                    "\x1b[M{}{}{}",
                    char::from(code + 32),
                    char::from_u32((col + 32) as u32).unwrap(),
                    char::from_u32((row + 32) as u32).unwrap()
                )
                .into_bytes()
            } else {
                if col > 223 || row > 223 {
                    return;
                }
                vec![27, b'[', b'M', code + 32, col as u8 + 32, row as u8 + 32]
            }
        };
        self.dispatch(Intent::WriteTerminal {
            handle: self.handle.clone(),
            data,
        });
    }
    fn position(&self, position: Point<Pixels>) -> CellPoint {
        let row = ((position.y - self.bounds.top()) / px(LINE_HEIGHT))
            .floor()
            .max(0.) as i32;
        let column = ((position.x - self.bounds.left()) / self.cell_width)
            .floor()
            .max(0.) as usize;
        CellPoint::new(
            Line(
                row.min(self.term.screen_lines() as i32 - 1)
                    - self.term.grid().display_offset() as i32,
            ),
            Column(column.min(self.term.columns() - 1)),
        )
    }
}
impl Render for Terminal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.error.clone().unwrap_or_else(|| {
            self.snapshot
                .terminals
                .get(&self.handle)
                .and_then(|_| {
                    self.snapshot
                        .terminal_view(&self.handle, self.sequence)
                        .status
                })
                .unwrap_or_else(|| "接続中…".into())
        });
        v_flex()
            .size_full()
            .min_h_0()
            .bg(crate::app::color("terminalBackground"))
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(crate::app::color("textMuted"))
                    .child(div().flex_1().child(self.cwd.clone()))
                    .child(status)
                    .child(
                        div()
                            .id("terminal-stop")
                            .ml_3()
                            .cursor_pointer()
                            .child("終了")
                            .on_click(cx.listener(|s, _, _, _| {
                                s.dispatch(Intent::KillTerminal {
                                    handle: s.handle.clone(),
                                })
                            })),
                    ),
            )
            .child(
                div()
                    .id("native-terminal")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::key))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, event: &MouseDownEvent, w, cx| {
                            s.focus.focus(w, cx);
                            if s.term.mode().intersects(TermMode::MOUSE_MODE)
                                && !event.modifiers.shift
                            {
                                s.mouse_pressed = true;
                                s.mouse_report(0, event.position, event.modifiers, false);
                                return;
                            }
                            s.selecting = true;
                            s.term.selection = Some(Selection::new(
                                if event.click_count >= 2 {
                                    SelectionType::Semantic
                                } else {
                                    SelectionType::Simple
                                },
                                s.position(event.position),
                                Side::Left,
                            ));
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|s, event: &MouseMoveEvent, _, cx| {
                        if s.mouse_pressed
                            && s.term
                                .mode()
                                .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                        {
                            s.mouse_report(32, event.position, event.modifiers, false);
                        } else if s.term.mode().contains(TermMode::MOUSE_MOTION)
                            && !event.modifiers.shift
                        {
                            s.mouse_report(35, event.position, event.modifiers, false);
                        }
                        if s.selecting {
                            let point = s.position(event.position);
                            if let Some(selection) = s.term.selection.as_mut() {
                                selection.update(point, Side::Right);
                            }
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|s, event: &MouseUpEvent, _, _| {
                            if s.mouse_pressed {
                                s.mouse_report(0, event.position, event.modifiers, true);
                                s.mouse_pressed = false;
                            }
                            s.selecting = false;
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|s, event: &ScrollWheelEvent, _, cx| {
                        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
                        let lines = (delta.y / px(LINE_HEIGHT)).round() as i32;
                        if s.term.mode().intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift
                        {
                            for _ in 0..lines.unsigned_abs().min(50) {
                                s.mouse_report(
                                    if lines > 0 { 64 } else { 65 },
                                    event.position,
                                    event.modifiers,
                                    false,
                                );
                            }
                        } else if s
                            .term
                            .mode()
                            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
                            && !event.modifiers.shift
                        {
                            let arrow = if s.term.mode().contains(TermMode::APP_CURSOR) {
                                "\x1bO"
                            } else {
                                "\x1b["
                            };
                            s.input(
                                format!("{arrow}{}", if lines > 0 { 'A' } else { 'B' })
                                    .repeat(lines.unsigned_abs().min(50) as usize)
                                    .into_bytes(),
                                cx,
                            );
                        } else {
                            s.term.scroll_display(Scroll::Delta(lines));
                        }
                        cx.notify();
                    }))
                    .child(TerminalCanvas {
                        terminal: cx.entity(),
                    }),
            )
    }
}
struct TerminalCanvas {
    terminal: Entity<Terminal>,
}
impl IntoElement for TerminalCanvas {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for TerminalCanvas {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let run = TextRun {
            len: 1,
            font: font("Menlo"),
            color: crate::app::color("terminalForeground").into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = window
            .text_system()
            .shape_line("M".into(), px(FONT_SIZE), &[run], None)
            .width;
        self.terminal.update(cx, |view, _| {
            view.bounds = bounds;
            view.cell_width = width;
            let size = TerminalSize {
                cols: ((bounds.size.width / width).floor() as u16).clamp(2, 500),
                rows: ((bounds.size.height / px(LINE_HEIGHT)).floor() as u16).clamp(1, 250),
            };
            if size != view.size {
                view.resize(size);
            }
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let entity = self.terminal.clone();
        entity.update(cx, |view, cx| {
            window.handle_input(
                &view.focus,
                ElementInputHandler::new(bounds, self.terminal.clone()),
                cx,
            );
            let content = view.term.renderable_content();
            let offset = content.display_offset as i32;
            for indexed in content.display_iter {
                let cell = indexed.cell;
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let cell_width = view.cell_width
                    * if cell.flags.contains(Flags::WIDE_CHAR) {
                        2.
                    } else {
                        1.
                    };
                let position = point(
                    bounds.left() + view.cell_width * indexed.point.column.0 as f32,
                    bounds.top() + px(LINE_HEIGHT) * (indexed.point.line.0 + offset) as f32,
                );
                let mut fg = resolve_color(cell.fg, &view.term);
                let mut bg = resolve_color(cell.bg, &view.term);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if content
                    .selection
                    .is_some_and(|selection| selection.contains(indexed.point))
                {
                    bg = crate::app::color("terminalSelection").into();
                }
                window.paint_quad(fill(
                    Bounds::new(position, size(cell_width, px(LINE_HEIGHT))),
                    bg,
                ));
                if cell.flags.contains(Flags::HIDDEN)
                    || (cell.c == ' '
                        && cell.zerowidth().is_none()
                        && !cell
                            .flags
                            .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT))
                {
                    continue;
                }
                let mut text = cell.c.to_string();
                if let Some(chars) = cell.zerowidth() {
                    text.extend(chars);
                }
                let mut face = font("Menlo");
                if cell.flags.contains(Flags::BOLD) {
                    face.weight = FontWeight::BOLD;
                }
                if cell.flags.contains(Flags::ITALIC) {
                    face.style = FontStyle::Italic;
                }
                let run = TextRun {
                    len: text.len(),
                    font: face,
                    color: fg,
                    background_color: None,
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES).then_some(
                        UnderlineStyle {
                            color: Some(
                                cell.underline_color()
                                    .map_or(fg, |color| resolve_color(color, &view.term)),
                            ),
                            thickness: px(1.),
                            wavy: cell.flags.contains(Flags::UNDERCURL),
                        },
                    ),
                    strikethrough: cell.flags.contains(Flags::STRIKEOUT).then_some(
                        StrikethroughStyle {
                            color: Some(fg),
                            thickness: px(1.),
                        },
                    ),
                };
                let line =
                    window
                        .text_system()
                        .shape_line(text.into(), px(FONT_SIZE), &[run], None);
                let _ = line.paint(position, px(LINE_HEIGHT), TextAlign::Left, None, window, cx);
            }
            if view.focus.is_focused(window)
                && view.term.mode().contains(TermMode::SHOW_CURSOR)
                && offset == 0
            {
                let cursor = content.cursor.point;
                let position = point(
                    bounds.left() + view.cell_width * cursor.column.0 as f32,
                    bounds.top() + px(LINE_HEIGHT) * cursor.line.0 as f32,
                );
                window.paint_quad(fill(
                    Bounds::new(position, size(px(2.), px(LINE_HEIGHT))),
                    crate::app::color("terminalForeground"),
                ));
                if !view.composition.is_empty() {
                    let run = TextRun {
                        len: view.composition.len(),
                        font: font("Menlo"),
                        color: crate::app::color("terminalForeground").into(),
                        background_color: Some(crate::app::color("terminalSelection").into()),
                        underline: Some(UnderlineStyle {
                            color: None,
                            thickness: px(1.),
                            wavy: false,
                        }),
                        strikethrough: None,
                    };
                    let line = window.text_system().shape_line(
                        view.composition.clone().into(),
                        px(FONT_SIZE),
                        &[run],
                        None,
                    );
                    let _ =
                        line.paint(position, px(LINE_HEIGHT), TextAlign::Left, None, window, cx);
                }
            }
        });
    }
}
fn resolve_color(color: TerminalColor, term: &Term<TerminalEvents>) -> Hsla {
    let index = match color {
        TerminalColor::Spec(c) => {
            return rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into();
        }
        TerminalColor::Indexed(i) => i as usize,
        TerminalColor::Named(n) => n as usize,
    };
    resolve_color_index(index, term)
}
fn resolve_color_index(index: usize, term: &Term<TerminalEvents>) -> Hsla {
    if let Some(c) = term.colors()[index] {
        return rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into();
    }
    rgb(agent_protocol::operations::terminal_color(index as u16)).into()
}
impl EntityInputHandler for Terminal {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let chars: Vec<u16> = self.composition.encode_utf16().collect();
        let end = range.end.min(chars.len());
        let start = range.start.min(end);
        *actual = Some(start..end);
        Some(String::from_utf16_lossy(&chars[start..end]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition_selection.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.is_empty()).then(|| 0..self.composition.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        self.composition_selection = 0..0;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.clear();
        self.composition_selection = 0..0;
        self.input(text.as_bytes().to_vec(), cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition = text.into();
        let end = text.encode_utf16().count();
        self.composition_selection = selected.unwrap_or(end..end);
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let p = self.term.grid().cursor.point;
        Some(Bounds::new(
            point(
                self.bounds.left() + self.cell_width * p.column.0 as f32,
                self.bounds.top() + px(LINE_HEIGHT) * p.line.0 as f32,
            ),
            size(self.cell_width, px(LINE_HEIGHT)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}
