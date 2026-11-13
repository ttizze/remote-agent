//! One Win32 viewport owns its native Rich Edit controls, scrolling and selection.
mod control;
use control::{Edit, Item, items};
use nsh_platform_windows::{ComponentContext, Components, NativeComponent};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
            Controls::{DRAWITEMSTRUCT, NMHDR, RichEdit::*, SetScrollInfo},
            HiDpi::GetDpiForWindow,
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const HOVER: u32 = WM_APP + 144;
const LAYOUT: u32 = WM_APP + 143;
const SUBCLASS: usize = 0x424558;

pub(super) fn register(components: &Components) {
    components.register(
        "bex.conversation",
        Rc::new(|context| Box::new(Conversation::new(context))),
    );
}

struct Context {
    owner: ComponentContext,
    pending: Cell<bool>,
    state: RefCell<Viewport>,
}

struct Viewport {
    window: HWND,
    scope: String,
    rows: Vec<Value>,
    controls: HashMap<String, Row>,
    heights: HashMap<String, i32>,
    offset: i32,
    width: i32,
    height: i32,
    total: i32,
    following: bool,
    inset: i32,
    lane: f32,
    padding: f32,
    latest: Option<Item>,
    hovered: Option<String>,
    wrap: HashMap<(String, usize), bool>,
}

struct Row {
    items: Vec<Item>,
    value: Value,
    width: i32,
}

struct Conversation {
    window: HWND,
    context: Box<Context>,
}

impl Conversation {
    fn new(owner: ComponentContext) -> Self {
        // NativeComponent::window reparents this viewport into its NSH surface.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WS_POPUP | WS_CLIPCHILDREN | WS_VSCROLL,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
        }
        .expect("create native transcript viewport");
        let context = Box::new(Context {
            owner,
            pending: Cell::new(false),
            state: RefCell::new(Viewport {
                window,
                scope: String::new(),
                rows: Vec::new(),
                controls: HashMap::new(),
                heights: HashMap::new(),
                offset: 0,
                width: 640,
                height: 480,
                total: 0,
                following: true,
                wrap: HashMap::new(),
                inset: 0,
                lane: 736.0,
                padding: 24.0,
                latest: None,
                hovered: None,
            }),
        });
        unsafe {
            let _ = SetWindowSubclass(
                window,
                Some(procedure),
                SUBCLASS,
                (&*context as *const Context) as usize,
            );
        }
        Self { window, context }
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.context.state.get_mut().controls.clear();
        self.context.state.get_mut().latest = None;
        unsafe {
            let _ = RemoveWindowSubclass(self.window, Some(procedure), SUBCLASS);
            let _ = DestroyWindow(self.window);
        }
    }
}

impl NativeComponent for Conversation {
    fn window(&self) -> Option<HWND> {
        Some(self.window)
    }
    fn set_prop(&mut self, name: &str, value: Option<&Value>) {
        let Some(value) = value.filter(|_| name == "data") else {
            return;
        };
        let mut state = self.context.state.borrow_mut();
        let scope = value["scope"].as_str().unwrap_or_default();
        let anchor = state.anchor();
        if state.scope != scope {
            state.controls.clear();
            state.heights.clear();
            state.wrap.clear();
            state.offset = 0;
            state.scope = scope.into();
        }
        state.rows = value["rows"].as_array().cloned().unwrap_or_default();
        let mut parts = Vec::new();
        if let Some(text) = value["opening"]["text"].as_str() {
            parts.push(json!(crate::rich_text::plain(text.into(), None)));
        }
        let mut buttons = Vec::new();
        if let Some(title) = value["opening"]["retry"].as_str() {
            buttons.push(json!({"action":"retryOpen","title":title}));
        }
        if let Some(notice) = value["notice"].as_str() {
            parts.push(json!({"Text":[{"runs":[{"text":notice}]}]}));
        }
        if value["history"].is_object() && value["history"]["enabled"] == true {
            buttons.push(json!({"title":value["history"]["title"],"action":"history"}));
        }
        if !parts.is_empty() || !buttons.is_empty() {
            state.rows.insert(
                0,
                json!({"id":"native.history","parts":parts,"buttons":buttons}),
            );
        }
        let scale = unsafe { GetDpiForWindow(state.window) }.max(96) as f32 / 96.0;
        state.inset = (value["inset"].as_f64().unwrap_or_default() as f32 * scale) as i32;
        state.lane = value["lane"].as_f64().unwrap_or(736.0) as f32;
        state.padding = value["padding"].as_f64().unwrap_or(24.0) as f32;
        state.latest = None;
        if value["following"] == false {
            let data = json!({"id":"native.latest"});
            state.latest = Some(Item::new(
                control::Descriptor::Button(
                    value["latest"].as_str().unwrap_or_default().into(),
                    json!({"action":"latest"}),
                ),
                state.window,
                &self.context.owner,
                scope,
                &data,
            ));
        }
        state.following = value["following"].as_bool().unwrap_or(true);
        if !state.following {
            state.restore(anchor);
        }
        state.layout(&self.context.owner);
    }
    fn command(&mut self, name: &str, args: &Value) {
        let mut state = self.context.state.borrow_mut();
        if name == "reveal" {
            let index = args["row"].as_u64().unwrap_or_default() as usize;
            state.offset = state.before(index);
            state.following = false;
        } else if name == "scroll" {
            state.offset = args["offset"].as_f64().unwrap_or_default() as i32;
        }
        state.layout(&self.context.owner);
    }
}

impl Viewport {
    fn before(&self, index: usize) -> i32 {
        self.rows
            .iter()
            .take(index)
            .map(|row| {
                self.heights
                    .get(row["id"].as_str().unwrap_or_default())
                    .copied()
                    .unwrap_or(160)
            })
            .sum::<i32>()
            + 16
    }
    fn anchor(&self) -> Option<(String, i32)> {
        let mut y = 16;
        for row in &self.rows {
            let id = row["id"].as_str()?;
            let height = self.heights.get(id).copied().unwrap_or(160);
            if y + height > self.offset {
                return Some((id.into(), self.offset - y));
            }
            y += height;
        }
        None
    }
    fn restore(&mut self, anchor: Option<(String, i32)>) {
        if let Some((id, delta)) = anchor
            && let Some(index) = self.rows.iter().position(|row| row["id"] == id)
        {
            self.offset = self.before(index) + delta;
        }
    }
    fn layout(&mut self, owner: &ComponentContext) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.window, &mut rect);
        }
        let width = rect.right.max(1);
        self.height = rect.bottom.max(1);
        if self.width != width {
            self.heights.clear();
        }
        self.width = width;
        let scale = unsafe { GetDpiForWindow(self.window) }.max(96) as f32 / 96.0;
        let lane = (self.lane * scale) as i32;
        let available = (width - (self.padding * 2.0 * scale) as i32)
            .max(40)
            .min(lane);
        self.total = self.before(self.rows.len()) + 16 + self.inset;
        if self.following {
            self.offset = (self.total - self.height).max(0);
        }
        self.offset = self.offset.clamp(0, (self.total - self.height).max(0));
        let anchor = self.anchor();
        let mut y = 16;
        let mut visible = Vec::new();
        for value in &self.rows {
            let id = value["id"].as_str().unwrap_or_default().to_owned();
            let estimated = self.heights.get(&id).copied().unwrap_or(160);
            if y + estimated >= self.offset - self.height && y < self.offset + self.height * 2 {
                visible.push(id.clone());
                let row = self.controls.entry(id.clone()).or_insert_with(|| Row {
                    items: Vec::new(),
                    value: Value::Null,
                    width: 0,
                });
                if row.value != *value || row.width != available {
                    let descriptors = items(value, &self.wrap, (available as f32 / scale) as i32);
                    row.update(descriptors, self.window, owner, &self.scope, value);
                    row.value = value.clone();
                    row.width = available;
                }
                for item in &row.items {
                    item.visibility(
                        self.hovered.as_ref() == Some(&id),
                        value["trailing"] == true,
                    );
                }
                let height = row.layout((width - available) / 2, y - self.offset, available, scale);
                self.heights.insert(id, height);
                y += height;
            } else {
                y += estimated;
            }
        }
        self.controls.retain(|id, _| visible.contains(id));
        self.total = y + 16 + self.inset;
        if self.following {
            self.offset = (self.total - self.height).max(0);
        } else {
            self.restore(anchor);
        }
        let mut actual_y = 16;
        let mut visible_range = None;
        for (index, value) in self.rows.iter().enumerate() {
            let id = value["id"].as_str().unwrap_or_default();
            let height = self.heights.get(id).copied().unwrap_or(160);
            if actual_y + height >= self.offset && actual_y < self.offset + self.height {
                let first = visible_range.map_or(index, |(first, _)| first);
                visible_range = Some((first, index + 1));
                if !self.controls.contains_key(id) {
                    unsafe {
                        let _ = PostMessageW(Some(self.window), LAYOUT, WPARAM(0), LPARAM(0));
                    }
                }
            }
            if let Some(row) = self.controls.get_mut(id) {
                row.layout(
                    (width - available) / 2,
                    actual_y - self.offset,
                    available,
                    scale,
                );
            }
            actual_y += height;
        }
        if let Some((start, end)) = visible_range {
            let header = usize::from(
                self.rows
                    .first()
                    .is_some_and(|row| row["id"] == "native.history"),
            );
            let start = start.saturating_sub(header);
            let end = end.saturating_sub(header);
            owner.emit(
                "action",
                json!({"action":"visible","scope":self.scope,"start":start,"end":end.saturating_sub(1)}),
            );
        }
        if let Some(latest) = &mut self.latest {
            latest.place(
                (width - available) / 2,
                self.height - (40.0 * scale) as i32,
                available,
                scale,
            );
        }
        let mut info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: self.total.max(1) - 1,
            nPage: self.height as u32,
            nPos: self.offset,
            ..Default::default()
        };
        unsafe {
            SetScrollInfo(self.window, SB_VERT, &mut info, true);
        }
    }
    fn hover(&mut self, id: Option<String>) {
        self.hovered = id;
        for (id, row) in &self.controls {
            for item in &row.items {
                item.visibility(
                    self.hovered.as_ref() == Some(id),
                    row.value["trailing"] == true,
                );
            }
        }
    }
    fn button(&mut self, mut action: Value, owner: &ComponentContext) {
        if action["action"] == "wrapCode" {
            let id = action["row"].as_str().unwrap_or_default();
            let key = (
                id.to_owned(),
                action["part"].as_u64().unwrap_or_default() as usize,
            );
            let wrap = self.wrap.entry(key).or_insert(true);
            *wrap = !*wrap;
            if let Some(row) = self.controls.get_mut(id) {
                row.value = Value::Null;
            }
            self.heights.clear();
            self.layout(owner);
        } else {
            action["scope"] = json!(self.scope);
            owner.emit("action", action);
        }
    }
    fn scroll(&mut self, delta: i32, owner: &ComponentContext) {
        self.offset = (self.offset + delta).clamp(0, (self.total - self.height).max(0));
        let scale = unsafe { GetDpiForWindow(self.window) }.max(96) as f32 / 96.0;
        let position = agent_core::presentation::conversation::ScrollPosition {
            offset: self.offset as f32 / scale,
            viewport: self.height as f32 / scale,
            content: self.total as f32 / scale,
        };
        self.following =
            agent_core::presentation::conversation::follows_end(self.following, None, position);
        owner.emit("action", json!({"action":"scroll", "scope":self.scope,"offset":position.offset,"viewport":position.viewport,"content":position.content}));
        self.layout(owner);
    }
}

impl Row {
    fn update(
        &mut self,
        descriptors: Vec<control::Descriptor>,
        parent: HWND,
        owner: &ComponentContext,
        scope: &str,
        row: &Value,
    ) {
        while self.items.len() > descriptors.len() {
            self.items.pop();
        }
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            if let Some(item) = self.items.get_mut(index) {
                if item.update(&descriptor, scope, row) {
                    continue;
                }
                self.items[index] = Item::new(descriptor, parent, owner, scope, row);
            } else {
                self.items
                    .push(Item::new(descriptor, parent, owner, scope, row));
            }
        }
    }
    fn layout(&mut self, x: i32, y: i32, width: i32, scale: f32) -> i32 {
        let mut index = 0;
        let mut offset = 0;
        while index < self.items.len() {
            if self.items[index].inline_width(scale).is_some() {
                let end = (index..self.items.len())
                    .find(|at| self.items[*at].inline_width(scale).is_none())
                    .unwrap_or(self.items.len());
                let total: i32 = self.items[index..end]
                    .iter()
                    .map(|item| item.inline_width(scale).unwrap() + (4.0 * scale) as i32)
                    .sum();
                let mut left = if self.value["user"] == true {
                    (width - total).max(0)
                } else {
                    0
                };
                for item in &mut self.items[index..end] {
                    let size = item.inline_width(scale).unwrap();
                    item.place(x + left, y + offset, size, scale);
                    left += size + (4.0 * scale) as i32;
                }
                offset += (30.0 * scale) as i32;
                index = end;
            } else {
                offset += self.items[index].place(x, y + offset, width, scale) + 6;
                index += 1;
            }
        }
        offset + (self.value["gap"].as_f64().unwrap_or(8.0) as f32 * scale) as i32
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let context = unsafe { &*(data as *const Context) };
    if msg == WM_NOTIFY && l.0 != 0 {
        let notification = unsafe { &*(l.0 as *const NMHDR) };
        if notification.code == EN_REQUESTRESIZE {
            let size = unsafe { &*(l.0 as *const REQRESIZE) };
            let rect = size.rc;
            if let Some(edit) = unsafe { Edit::context(notification.hwndFrom) } {
                edit.height.set((rect.bottom - rect.top).max(20));
                if !context.pending.replace(true) {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), LAYOUT, WPARAM(0), LPARAM(0));
                    }
                }
            }
            return LRESULT(0);
        }
        if notification.code == EN_LINK {
            unsafe {
                Edit::link(&*(l.0 as *const ENLINK));
            }
            return LRESULT(0);
        }
    }
    if msg == WM_DRAWITEM && l.0 != 0 && unsafe { Edit::draw(&*(l.0 as *const DRAWITEMSTRUCT)) } {
        return LRESULT(1);
    }
    if msg == WM_ERASEBKGND {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rect);
            let brush = CreateSolidBrush(COLORREF(0x080808));
            FillRect(HDC(w.0 as *mut _), &rect, brush);
            let _ = DeleteObject(brush.into());
        }
        return LRESULT(1);
    }
    if let Ok(mut state) = context.state.try_borrow_mut() {
        match msg {
            HOVER => state.hover(unsafe { Edit::row(HWND(l.0 as *mut _)) }),
            WM_MOUSEMOVE | WM_MOUSELEAVE => state.hover(None),
            WM_SIZE => state.layout(&context.owner),
            LAYOUT => {
                context.pending.set(false);
                state.layout(&context.owner);
            }
            WM_MOUSEWHEEL => state.scroll(-((w.0 >> 16) as i16 as i32) / 120 * 60, &context.owner),
            WM_VSCROLL => {
                let mut info = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_ALL,
                    ..Default::default()
                };
                unsafe {
                    let _ = GetScrollInfo(hwnd, SB_VERT, &mut info);
                }
                let delta = match w.0 as u32 & 0xffff {
                    0 => -30,
                    1 => 30,
                    2 => -state.height,
                    3 => state.height,
                    4 | 5 => info.nTrackPos - state.offset,
                    6 => -state.offset,
                    7 => state.total - state.offset,
                    _ => 0,
                };
                state.scroll(delta, &context.owner);
            }
            WM_COMMAND if l.0 != 0 => {
                if let Some(action) = unsafe { Edit::button(HWND(l.0 as *mut _)) } {
                    state.button(action, &context.owner);
                }
            }
            _ => {}
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, w, l) }
}
