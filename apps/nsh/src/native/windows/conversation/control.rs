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
        System::LibraryLoader::*,
        UI::{Controls::RichEdit::*, Shell::*, WindowsAndMessaging::*},
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
            id,
            index,
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
    for button in row["buttons"].as_array().into_iter().flatten() {
        result.push(Descriptor::Button(
            button["title"].as_str().unwrap_or_default().into(),
            json!({"action":button["action"],"row":id}),
        ));
    }
    if let Some(time) = row["time"].as_str() {
        result.push(Descriptor::Text(
            rich_document::paragraphs(&[json!({"runs":[{"text":time}]})]),
            true,
        ));
    }
    result
}

fn append_part(
    output: &mut Vec<Descriptor>,
    part: &Value,
    row: &str,
    index: usize,
    wrapping: bool,
    width: i32,
    media: &Value,
    labels: &Value,
) {
    if let Some(blocks) = part["Text"].as_array() {
        output.push(Descriptor::Text(rich_document::paragraphs(blocks), true));
    } else if let Some(code) = part.get("Code") {
        let label = code["label"].as_str().unwrap_or_default();
        output.push(Descriptor::Button(
            format!(
                "{label} · {}",
                labels["copyCode"].as_str().unwrap_or_default()
            ),
            json!({"action":"copyText","text":code["source"]}),
        ));
        output.push(Descriptor::Button(
            if wrapping {
                labels["unwrap"].as_str().unwrap_or_default()
            } else {
                labels["wrap"].as_str().unwrap_or_default()
            }
            .into(),
            json!({"action":"wrapCode","row":row,"part":index}),
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
        }
        let mut item = Self {
            edit: Edit { window, context },
            button,
        };
        item.update(&descriptor, scope, row);
        item
    }
    pub fn update(&mut self, descriptor: &Descriptor, scope: &str, row: &Value) -> bool {
        if self.button != matches!(descriptor, Descriptor::Button(..)) {
            return false;
        }
        let base = json!({"scope":scope,"row":row["id"],"target":row["document"]["target"],"menu":row["document"]["menu"],"sideChat":row["document"]["sideChat"]});
        match descriptor {
            Descriptor::Button(title, action) => {
                let title: Vec<_> = title.encode_utf16().chain([0]).collect();
                unsafe {
                    let _ = SetWindowTextW(self.edit.window, PCWSTR(title.as_ptr()));
                }
                let mut base = base;
                for (key, value) in action.as_object().into_iter().flatten() {
                    base[key] = value.clone();
                }
                *self.edit.context.payload.borrow_mut() = base;
            }
            Descriptor::Text(document, wrap) => {
                *self.edit.context.payload.borrow_mut() = base;
                if self.edit.context.wrap.replace(*wrap) != *wrap {
                    self.edit.context.width.set(0);
                }
                if self.edit.context.document.borrow().rtf != document.rtf {
                    self.edit.set_document(document);
                }
            }
        }
        true
    }
    pub fn place(&mut self, x: i32, y: i32, width: i32, scale: f32) -> i32 {
        let context = &self.edit.context;
        let changed = context.width.replace(width) != width;
        let height = if self.button {
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
