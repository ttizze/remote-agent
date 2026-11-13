use crate::native::rich_document::{self, Document};
use nsh_platform_windows::ComponentContext;
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    io::Read,
    rc::Rc,
    sync::OnceLock,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::*,
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, RichEdit::*},
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL},
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{Interface, PCWSTR, PWSTR, w},
};

const SUBCLASS: usize = 0x424559;
static LIBRARY: OnceLock<Result<(), String>> = OnceLock::new();

pub(super) fn message(hwnd: HWND, id: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, id, Some(WPARAM(w)), Some(LPARAM(l))) }
}

pub(super) enum Descriptor {
    Text(Document, bool),
    Button(String, Value),
    Caption(String),
}
pub(super) struct Item {
    edit: Edit,
    button: bool,
}
pub(super) struct Edit {
    pub window: HWND,
    context: Rc<EditContext>,
}
pub(super) struct EditContext {
    pub height: Cell<i32>,
    width: Cell<i32>,
    owner: ComponentContext,
    parent: HWND,
    payload: RefCell<Value>,
    document: RefCell<Document>,
    wrap: Cell<bool>,
    button: bool,
    compact: Cell<bool>,
    caption: bool,
    symbol: Cell<Option<u16>>,
    icon_only: Cell<bool>,
}

pub(super) fn items(
    row: &Value,
    wrapping: &std::collections::HashMap<(String, usize), bool>,
    width: i32,
) -> Vec<Descriptor> {
    let mut result = Vec::new();
    let id = row["id"].as_str().unwrap_or_default();
    let labels = &row["document"]["labels"];
    for (index, part) in row["parts"].as_array().into_iter().flatten().enumerate() {
        append_part(
            &mut result,
            part,
            (id, index),
            wrapping.get(&(id.into(), index)).copied().unwrap_or(true),
            width,
            &row["media"],
            &row["document"]["labels"],
        );
    }
    for image in row["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(data) = row["media"][image]
            .as_str()
            .and_then(|data| rich_document::picture(data, width.max(1) as u32))
        {
            result.push(Descriptor::Text(data, true));
        }
        result.push(Descriptor::Button(
            labels["openImage"].as_str().unwrap_or_default().into(),
            json!({"action":"image","text":image}),
        ));
    }
    let time = row["time"].as_str().filter(|time| !time.is_empty());
    if row["user"] == true
        && let Some(time) = time
    {
        result.push(Descriptor::Caption(time.into()));
    }
    for button in row["buttons"].as_array().into_iter().flatten() {
        result.push(Descriptor::Button(
            button["title"].as_str().unwrap_or_default().into(),
            button.clone(),
        ));
    }
    if row["user"] != true
        && let Some(time) = time
    {
        result.push(Descriptor::Caption(time.into()));
    }

    result
}

fn append_part(
    output: &mut Vec<Descriptor>,
    part: &Value,
    key: (&str, usize),
    wrapping: bool,
    width: i32,
    media: &Value,
    labels: &Value,
) {
    let (row, index) = key;
    if let Some(blocks) = part["Text"].as_array() {
        output.push(Descriptor::Text(rich_document::paragraphs(blocks), true));
    } else if let Some(code) = part.get("Code") {
        let label = code["label"].as_str().unwrap_or_default();
        if !label.is_empty() {
            output.push(Descriptor::Text(
                rich_document::paragraphs(&[
                    json!({"size":11,"runs":[{"text":label,"code":true}]}),
                ]),
                true,
            ));
        }
        output.push(Descriptor::Button(
            labels["copyCode"].as_str().unwrap_or_default().to_owned(),
            json!({"action":"copyText","text":code["source"],"iconOnly":true,"icon":"copy"}),
        ));
        output.push(Descriptor::Button(
            if wrapping {
                labels["unwrap"].as_str().unwrap_or_default()
            } else {
                labels["wrap"].as_str().unwrap_or_default()
            }
            .into(),
            json!({"action":"wrapCode","row":row,"part":index,"iconOnly":true,"icon":"wrap"}),
        ));
        let blocks = code["blocks"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if let Some(data) = code["diagram"]["png"]
            .as_str()
            .and_then(|data| rich_document::picture(data, width.max(1) as u32))
        {
            output.push(Descriptor::Text(data, true));
        }
        output.push(Descriptor::Text(
            rich_document::paragraphs(blocks),
            wrapping,
        ));
    } else if let Some(table) = part.get("Table") {
        let rows = table["rows"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        output.push(Descriptor::Text(
            rich_document::table(rows, width.max(160) as u32),
            true,
        ));
        for (title, key) in [
            (
                labels["copyMarkdown"].as_str().unwrap_or_default(),
                "source",
            ),
            (labels["copyCsv"].as_str().unwrap_or_default(), "csv"),
        ] {
            output.push(Descriptor::Button(
                title.into(),
                json!({"action":"copyText","text":table[key]}),
            ));
        }
    } else if let Some(image) = part.get("Image") {
        if let Some(data) = image["source"]
            .as_str()
            .and_then(|source| media[source].as_str())
            .and_then(|data| rich_document::picture(data, width.max(1) as u32))
        {
            output.push(Descriptor::Text(data, true));
        }
        output.push(Descriptor::Button(
            labels["openImage"].as_str().unwrap_or_default().into(),
            json!({"action":"image","text":image["source"]}),
        ));
    } else if let Some(link) = part.get("Link") {
        output.push(Descriptor::Button(
            labels["openFile"].as_str().unwrap_or_default().into(),
            json!({"action":"link","text":link["url"]}),
        ));
    }
}

impl Item {
    pub fn new(
        descriptor: Descriptor,
        parent: HWND,
        owner: &ComponentContext,
        scope: &str,
        row: &Value,
    ) -> Self {
        let button = matches!(descriptor, Descriptor::Button(..));
        let context = Rc::new(EditContext {
            height: Cell::new(28),
            width: Cell::new(0),
            owner: owner.clone(),
            parent,
            payload: RefCell::new(Value::Null),
            document: RefCell::new(Document::default()),
            wrap: Cell::new(true),
            button,
            caption: matches!(descriptor, Descriptor::Caption(_)),
            compact: Cell::new(false),
            symbol: Cell::new(None),
            icon_only: Cell::new(false),
        });
        let class = if button {
            w!("BUTTON")
        } else {
            LIBRARY
                .get_or_init(|| {
                    unsafe {
                        LoadLibraryExW(w!("Msftedit.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                    }
                    .map(|_| ())
                    .map_err(|e| e.to_string())
                })
                .as_ref()
                .expect("load system Rich Edit");
            w!("RICHEDIT50W")
        };
        let style = WS_CHILD
            | WS_VISIBLE
            | WS_CLIPSIBLINGS
            | if button {
                WINDOW_STYLE(0)
            } else {
                WINDOW_STYLE(0x4 | 0x800 | 0x100) | WS_HSCROLL
            };
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!(""),
                style,
                0,
                0,
                100,
                30,
                Some(parent),
                None,
                None,
                None,
            )
        }
        .expect("create native transcript control");
        unsafe {
            let _ = SetWindowSubclass(
                window,
                Some(procedure),
                SUBCLASS,
                Rc::as_ptr(&context) as usize,
            );
        }
        if !button {
            message(window, EM_EXLIMITTEXT, 0, i32::MAX as isize);
            message(window, EM_SETBKGNDCOLOR, 0, 0x080808);
            message(
                window,
                EM_SETEVENTMASK,
                0,
                (ENM_REQUESTRESIZE | ENM_LINK) as isize,
            );
            message(window, EM_AUTOURLDETECT, 0, 0);
            // Match the shared UI point sizes at Windows' 96-DPI logical scale.
            message(window, EM_SETZOOM, 3, 4);
        }
        let mut item = Self {
            edit: Edit { window, context },
            button,
        };
        item.update(&descriptor, scope, row);
        item
    }
    pub fn update(&mut self, descriptor: &Descriptor, scope: &str, row: &Value) -> bool {
        if self.button != matches!(descriptor, Descriptor::Button(..))
            || self.edit.context.caption != matches!(descriptor, Descriptor::Caption(_))
        {
            return false;
        }
        let base = json!({"scope":scope,"row":row["id"],"target":row["document"]["target"],"menu":row["document"]["menu"],"sideChat":row["document"]["sideChat"]});
        match descriptor {
            Descriptor::Button(title, action) => {
                let title: Vec<_> = title.encode_utf16().chain([0]).collect();
                unsafe {
                    let _ = SetWindowTextW(self.edit.window, PCWSTR(title.as_ptr()));
                }
                unsafe {
                    let _ = EnableWindow(self.edit.window, action["enabled"] != false);
                }
                self.edit.context.compact.set(action["iconOnly"] == true);
                self.edit
                    .icon(action["icon"].as_str(), action["iconOnly"] == true);
                let mut base = base;
                for key in ["action", "text", "part", "menuItems"] {
                    if let Some(value) = action.get(key) {
                        base[key] = value.clone();
                    }
                }
                *self.edit.context.payload.borrow_mut() = base;
            }
            Descriptor::Text(document, wrap) => {
                *self.edit.context.payload.borrow_mut() = base;
                self.text(document, *wrap);
            }
            Descriptor::Caption(text) => {
                *self.edit.context.payload.borrow_mut() = base;
                self.text(
                    &rich_document::paragraphs(&[json!({"size":12,"runs":[{"text":text}]})]),
                    true,
                );
            }
        }
        true
    }
    fn text(&self, document: &Document, wrap: bool) {
        if self.edit.context.wrap.replace(wrap) != wrap {
            self.edit.context.width.set(0);
        }
        if self.edit.context.document.borrow().rtf != document.rtf {
            self.edit.set_document(document);
        }
    }
    pub fn inline_width(&self, scale: f32) -> Option<i32> {
        if self.edit.context.caption {
            Some(
                ((self.edit.context.document.borrow().text.chars().count() as f32 * 7.0 + 8.0)
                    * scale) as i32,
            )
        } else if self.edit.context.compact.get() {
            Some((24.0 * scale) as i32)
        } else {
            None
        }
    }
    pub fn visibility(&self, hovered: bool, trailing: bool) {
        let payload = self.edit.context.payload.borrow();
        let meta = self.edit.context.caption
            || matches!(payload["action"].as_str(), Some("copy" | "fork" | "edit"));
        unsafe {
            let _ = ShowWindow(
                self.edit.window,
                if !meta || hovered || trailing {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_HIDE
                },
            );
        }
    }
    pub fn place(&mut self, x: i32, y: i32, width: i32, scale: f32) -> i32 {
        let context = &self.edit.context;
        let changed = context.width.replace(width) != width;
        let height = if self.button || context.caption {
            (26.0 * scale) as i32
        } else {
            context.height.get()
                + if context.wrap.get() {
                    0
                } else {
                    (17.0 * scale) as i32
                }
        };
        unsafe {
            let _ = SetWindowPos(
                self.edit.window,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if !self.button && changed {
            message(
                self.edit.window,
                EM_SETTARGETDEVICE,
                0,
                if context.wrap.get() { 0 } else { 1 },
            );
            message(self.edit.window, EM_REQUESTRESIZE, 0, 0);
        }
        height
    }
}

impl Edit {
    fn icon(&self, name: Option<&str>, only: bool) {
        // OS glyphs are independent of the semantic WindowText accessibility name.
        self.context.symbol.set(match name {
            Some("copy") => Some(0xe8c8),
            Some("edit") => Some(0xe70f),
            Some("fork") => Some(0xef90),
            Some("more") => Some(0xe712),
            Some("chevronDown") => Some(0xe70d),
            Some("chevronRight") => Some(0xe76c),
            Some("check") => Some(0xe8fb),
            Some("wrap") => Some(0xe751),
            _ => None,
        });
        self.context.icon_only.set(only);
        unsafe {
            let style = GetWindowLongPtrW(self.window, GWL_STYLE);
            let _ = SetWindowLongPtrW(
                self.window,
                GWL_STYLE,
                (style & !(BS_TYPEMASK as isize))
                    | if self.context.symbol.get().is_some() {
                        BS_OWNERDRAW as isize
                    } else {
                        BS_PUSHBUTTON as isize
                    }
                    | BS_FLAT as isize,
            );
        }
    }
    pub unsafe fn draw(item: &DRAWITEMSTRUCT) -> bool {
        let Some(context) = (unsafe { Self::context(item.hwndItem) }) else {
            return false;
        };
        let Some(symbol) = context.symbol.get() else {
            return false;
        };
        let scale = unsafe { GetDpiForWindow(item.hwndItem) }.max(96) as f32 / 96.0;
        unsafe {
            let saved = SaveDC(item.hDC);
            let brush = CreateSolidBrush(COLORREF(if item.itemState.contains(ODS_SELECTED) {
                0x282828
            } else {
                0x080808
            }));
            FillRect(item.hDC, &item.rcItem, brush);
            let _ = DeleteObject(brush.into());
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(
                item.hDC,
                COLORREF(if item.itemState.contains(ODS_DISABLED) {
                    0x7e7e7e
                } else {
                    0xaaaaaa
                }),
            );
            let font = CreateFontW(
                -(12.0 * scale) as i32,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe Fluent Icons"),
            );
            SelectObject(item.hDC, font.into());
            let mut rect = item.rcItem;
            if !context.icon_only.get() {
                rect.left = (rect.right - (20.0 * scale) as i32).max(rect.left);
            }
            DrawTextW(
                item.hDC,
                &mut [symbol],
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if !context.icon_only.get() {
                let text_font = CreateFontW(
                    -(14.0 * scale) as i32,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                );
                SelectObject(item.hDC, text_font.into());
                let mut title =
                    vec![0_u16; GetWindowTextLengthW(item.hwndItem).max(0) as usize + 1];
                let length = GetWindowTextW(item.hwndItem, &mut title).max(0) as usize;
                let mut rect = item.rcItem;
                rect.left += (4.0 * scale) as i32;
                rect.right -= (20.0 * scale) as i32;
                DrawTextW(
                    item.hDC,
                    &mut title[..length],
                    &mut rect,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                SelectObject(item.hDC, font.into());
                let _ = DeleteObject(text_font.into());
            }
            let _ = RestoreDC(item.hDC, saved);
            let _ = DeleteObject(font.into());
            if item.itemState.contains(ODS_FOCUS) {
                DrawFocusRect(item.hDC, &item.rcItem);
            }
        }
        true
    }
    pub unsafe fn row(window: HWND) -> Option<String> {
        let context = unsafe { Self::context(window) }?;
        context.payload.borrow()["row"].as_str().map(str::to_owned)
    }

    fn set_document(&self, next: &Document) {
        let range = self.range();
        let selected = self.selected(range);
        let retained =
            utf16(&next.text, range.cpMin, range.cpMax).is_some_and(|text| text == selected);
        message(self.window, WM_SETREDRAW, 0, 0);
        let mut input = std::io::Cursor::new(next.rtf.as_slice());
        let mut stream = EDITSTREAM {
            dwCookie: (&mut input as *mut std::io::Cursor<&[u8]>) as usize,
            dwError: 0,
            pfnCallback: Some(read_stream),
        };
        message(
            self.window,
            EM_STREAMIN,
            SF_RTF as usize,
            (&mut stream as *mut EDITSTREAM) as isize,
        );
        *self.context.document.borrow_mut() = next.clone();
        let mut restored = if retained {
            range
        } else {
            CHARRANGE { cpMin: 0, cpMax: 0 }
        };
        message(
            self.window,
            EM_EXSETSEL,
            0,
            (&mut restored as *mut CHARRANGE) as isize,
        );
        message(self.window, WM_SETREDRAW, 1, 0);
        message(self.window, EM_REQUESTRESIZE, 0, 0);
    }
    fn range(&self) -> CHARRANGE {
        let mut range = CHARRANGE::default();
        message(
            self.window,
            EM_EXGETSEL,
            0,
            (&mut range as *mut CHARRANGE) as isize,
        );
        range
    }
    fn selected(&self, range: CHARRANGE) -> String {
        selected(self.window, range)
    }
    pub unsafe fn context(window: HWND) -> Option<Rc<EditContext>> {
        let mut data = 0;
        unsafe { GetWindowSubclass(window, Some(procedure), SUBCLASS, Some(&mut data)).as_bool() }
            .then(|| unsafe {
                Rc::increment_strong_count(data as *const EditContext);
                Rc::from_raw(data as *const EditContext)
            })
    }
    pub unsafe fn button(window: HWND) -> Option<Value> {
        let context = unsafe { Self::context(window) }?;
        context.button.then(|| context.payload.borrow().clone())
    }
    pub unsafe fn link(notification: &ENLINK) {
        if notification.msg != WM_LBUTTONUP {
            return;
        }
        let window = notification.nmhdr.hwndFrom;
        let Some(context) = (unsafe { Self::context(window) }) else {
            return;
        };
        let range = notification.chrg;
        let destination = url(window, range).or_else(|| {
            context
                .document
                .borrow()
                .links
                .iter()
                .find(|link| link.start == range.cpMin as usize && link.end == range.cpMax as usize)
                .map(|link| link.destination.clone())
        });
        if let Some(destination) = destination {
            let mut action = context.payload.borrow().clone();
            action["action"] = json!("link");
            action["text"] = json!(destination);
            action["command"] = json!(unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0);
            context.owner.emit("action", action);
        }
    }
}

impl Drop for Edit {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveWindowSubclass(self.window, Some(procedure), SUBCLASS);
            let _ = DestroyWindow(self.window);
        }
    }
}

fn utf16(text: &str, start: i32, end: i32) -> Option<String> {
    let text: Vec<_> = text.encode_utf16().collect();
    String::from_utf16(text.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)?).ok()
}

fn selected(window: HWND, range: CHARRANGE) -> String {
    let length = range.cpMax.saturating_sub(range.cpMin).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let mut value = TEXTRANGEW {
        chrg: range,
        lpstrText: PWSTR(buffer.as_mut_ptr()),
    };
    let count = message(
        window,
        EM_GETTEXTRANGE,
        0,
        (&mut value as *mut TEXTRANGEW) as isize,
    )
    .0
    .max(0) as usize;
    String::from_utf16_lossy(&buffer[..count.min(length)]).replace(['\r', '\u{b}'], "\n")
}

fn url(window: HWND, range: CHARRANGE) -> Option<String> {
    let mut ole: Option<IRichEditOle> = None;
    message(
        window,
        EM_GETOLEINTERFACE,
        0,
        (&mut ole as *mut Option<IRichEditOle>) as isize,
    );
    let document: ITextDocument2 = ole?.cast().ok()?;
    unsafe {
        document
            .Range2(range.cpMin, range.cpMax)
            .ok()?
            .GetURL()
            .ok()
    }
    .map(|url| url.to_string())
}

unsafe extern "system" fn read_stream(
    cookie: usize,
    buffer: *mut u8,
    count: i32,
    copied: *mut i32,
) -> u32 {
    if count < 0 {
        return 1;
    }
    let input = unsafe { &mut *(cookie as *mut std::io::Cursor<&[u8]>) };
    let output = unsafe { std::slice::from_raw_parts_mut(buffer, count as usize) };
    match input.read(output) {
        Ok(count) => {
            unsafe {
                *copied = count as i32;
            }
            0
        }
        Err(_) => 1,
    }
}

fn popup(window: HWND, context: &EditContext) {
    let mut range = CHARRANGE::default();
    message(
        window,
        EM_EXGETSEL,
        0,
        (&mut range as *mut CHARRANGE) as isize,
    );
    let text = selected(window, range);
    if text.is_empty() && file_menu(window, context) {
        return;
    }
    if text.is_empty() {
        return;
    }
    let payload = context.payload.borrow().clone();
    let mut actions = vec![
        ("copy", "コピー".to_owned()),
        (
            "quote",
            payload["menu"]["quote"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        ),
        (
            "explain",
            payload["menu"]["explain"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        ),
    ];
    if payload["sideChat"] == true {
        actions.push((
            "sideChat",
            payload["menu"]["sideChat"]
                .as_str()
                .unwrap_or_default()
                .into(),
        ));
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (index, (_, title)) in actions.iter().enumerate() {
            let title: Vec<_> = title.encode_utf16().chain([0]).collect();
            let _ = AppendMenuW(menu, MF_STRING, index + 1, PCWSTR(title.as_ptr()));
        }
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            at.x,
            at.y,
            None,
            window,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        if chosen == 1 {
            message(window, WM_COPY, 0, 0);
        } else if let Some((action, _)) =
            chosen.checked_sub(1).and_then(|i| actions.get(i as usize))
        {
            let mut value = payload;
            value["action"] = json!(action);
            value["text"] = json!(text);
            context.owner.emit("action", value);
        }
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
    // Retain callback state across the native popup menu's nested event loop.
    let context = unsafe {
        Rc::increment_strong_count(data as *const EditContext);
        Rc::from_raw(data as *const EditContext)
    };
    if msg == WM_MOUSEMOVE {
        unsafe {
            let _ = PostMessageW(
                Some(context.parent),
                super::HOVER,
                WPARAM(0),
                LPARAM(hwnd.0 as isize),
            );
        }
    }
    if !context.button && msg == WM_MOUSEWHEEL {
        return message(context.parent, msg, w.0, l.0);
    }
    if !context.button && msg == WM_CONTEXTMENU {
        popup(hwnd, &context);
        return LRESULT(0);
    }
    let result = unsafe { DefSubclassProc(hwnd, msg, w, l) };
    if !context.button && msg == WM_LBUTTONUP {
        popup(hwnd, &context);
    }
    result
}

fn file_menu(window: HWND, context: &EditContext) -> bool {
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
        let _ = ScreenToClient(window, &mut point);
    }
    let at = message(window, EM_CHARFROMPOS, 0, (&point as *const POINT) as isize).0;
    let link = context
        .document
        .borrow()
        .links
        .iter()
        .find(|link| link.start <= at as usize && (at as usize) < link.end)
        .cloned();
    let Some(link) = link.filter(|link| link.menu.is_array()) else {
        return false;
    };
    let payload = context.payload.borrow().clone();
    let mut actions = Vec::new();
    unsafe fn append(menu: HMENU, items: &[Value], actions: &mut Vec<String>) {
        for item in items {
            let title: Vec<_> = item["title"]
                .as_str()
                .unwrap_or_default()
                .encode_utf16()
                .chain([0])
                .collect();
            if let Some(children) = item["children"].as_array() {
                if let Ok(child) = unsafe { CreatePopupMenu() } {
                    unsafe {
                        append(child, children, actions);
                        let _ =
                            AppendMenuW(menu, MF_POPUP, child.0 as usize, PCWSTR(title.as_ptr()));
                    }
                }
            } else if let Some(id) = item["id"].as_str() {
                actions.push(id.to_owned());
                unsafe {
                    let _ = AppendMenuW(menu, MF_STRING, actions.len(), PCWSTR(title.as_ptr()));
                }
            }
        }
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return false;
        };
        append(menu, link.menu.as_array().unwrap(), &mut actions);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            window,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        if let Some(id) = chosen
            .checked_sub(1)
            .and_then(|index| actions.get(index as usize))
        {
            let mut value = payload;
            value["action"] = json!(link.action);
            value["id"] = json!(id);
            value["text"] = json!(link.destination);
            context.owner.emit("action", value);
        }
    }
    true
}
