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
            Controls::{
                DRAWITEMSTRUCT, EM_CHARFROMPOS, EM_SCROLLCARET, ODS_DISABLED, ODS_FOCUS,
                ODS_SELECTED, RichEdit::*,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_C, VK_CONTROL},
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

const SUBCLASS: usize = 0x424559;
const COPY_TIMER: usize = 1;
static LIBRARY: OnceLock<Result<(), String>> = OnceLock::new();

#[cfg(test)]
mod tests;

fn rich_edit_class() -> PCWSTR {
    LIBRARY
        .get_or_init(|| {
            unsafe { LoadLibraryExW(w!("Msftedit.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .expect("load system Rich Edit");
    w!("RICHEDIT50W")
}

pub(super) fn message(hwnd: HWND, id: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, id, Some(WPARAM(w)), Some(LPARAM(l))) }
}

pub(super) enum Descriptor {
    Text(Document, bool),
    Icon(String),
    Button(String, Value),
    Picture {
        source: String,
        data: Option<String>,
        error: Option<String>,
        label: String,
    },
    Caption(String, bool),
    Rule,
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
    rule: bool,
    metadata: Cell<bool>,
    symbol: Cell<Option<u16>>,
    icon_only: Cell<bool>,
    revealed: RefCell<Option<(String, (usize, usize))>>,
    copied: Cell<bool>,
    icon_kind: bool,
    picture_kind: bool,
    picture: RefCell<Option<super::picture::Picture>>,
    cover: Cell<bool>,
    tooltip: RefCell<Option<super::tooltip::Tooltip>>,
    effects: RefCell<super::effects::Effects>,
}

#[derive(Clone, Copy)]
pub(super) struct BlockState {
    pub wrap: bool,
    pub source: bool,
    pub large: bool,
}
impl Default for BlockState {
    fn default() -> Self {
        Self {
            wrap: true,
            source: false,
            large: false,
        }
    }
}

pub(super) fn items(
    row: &Value,
    states: &std::collections::HashMap<(String, usize), BlockState>,
    width: i32,
    expanded: bool,
) -> (Vec<Descriptor>, std::ops::Range<usize>) {
    let mut result = Vec::new();
    let id = row["id"].as_str().unwrap_or_default();
    let labels = &row["document"]["labels"];
    for key in ["queued", "steer"] {
        if row[key] == true {
            result.push(Descriptor::Caption(
                labels[key].as_str().unwrap_or_default().into(),
                false,
            ));
        }
    }
    if let Some(icon) = row["icon"].as_str() {
        result.push(Descriptor::Icon(icon.into()));
    }
    for button in row["buttons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|button| !row["agentStatus"].is_null() || button["header"] == true)
    {
        result.push(Descriptor::Button(
            button["title"].as_str().unwrap_or_default().into(),
            button.clone(),
        ));
    }
    if row["user"] == true {
        for image in row["images"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            picture_parts(&mut result, image, &row["media"], &row["document"], width);
        }
    }
    let body_start = result.len();
    for (index, part) in row["parts"].as_array().into_iter().flatten().enumerate() {
        append_part(
            &mut result,
            part,
            (id, index),
            states.get(&(id.into(), index)).copied().unwrap_or_default(),
            width,
            &row["media"],
            &row["document"],
        );
    }
    if row["preparation"].is_object() {
        preparation_parts(&mut result, &row["preparation"]);
    }
    let body_end = result.len();
    if row["user"] != true {
        for image in row["images"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            picture_parts(&mut result, image, &row["media"], &row["document"], width);
        }
    }
    if row["collapsible"] == true {
        result.push(Descriptor::Button(
            labels[if expanded { "collapse" } else { "expand" }]
                .as_str()
                .unwrap_or_default()
                .into(),
            json!({"action":"expandMessage"}),
        ));
    }
    let time = row["time"].as_str().filter(|time| !time.is_empty());
    if row["user"] == true
        && let Some(time) = time
    {
        result.push(Descriptor::Caption(time.into(), row["timeAlways"] != true));
    }
    for button in row["buttons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|button| row["agentStatus"].is_null() && button["header"] != true)
    {
        result.push(Descriptor::Button(
            button["title"].as_str().unwrap_or_default().into(),
            button.clone(),
        ));
    }
    if row["user"] != true
        && let Some(time) = time
    {
        result.push(Descriptor::Caption(time.into(), row["timeAlways"] != true));
    }

    for detail in row["preparation"]["details"]
        .as_array()
        .into_iter()
        .flatten()
    {
        result.push(Descriptor::Text(rich_document::paragraphs(&[json!({"size":12,"depth":2,"runs":[{"text":format!("{}  {}", detail["label"].as_str().unwrap_or_default(),detail["value"].as_str().unwrap_or_default())}]})]),true));
    }

    (result, body_start..body_end)
}

fn preparation_parts(output: &mut Vec<Descriptor>, preparation: &Value) {
    output.push(Descriptor::Rule);
    for step in preparation["steps"].as_array().into_iter().flatten() {
        output.push(Descriptor::Icon(
            step["icon"].as_str().unwrap_or("pending").into(),
        ));
        let color = if step["dim"] == true {
            0x666666
        } else {
            0xaaaaaa
        };
        let mut label = rich_document::paragraphs(&[json!({"runs":[
            {"text":step["label"],"darkColor":color},
            {"text":format!("  {}  {}",step["detail"].as_str().unwrap_or_default(),step["time"].as_str().unwrap_or_default()),"size":12,"darkColor":color}
        ]})]);
        label.shimmer = serde_json::from_value(step["shimmer"].clone()).ok();
        output.push(Descriptor::Text(label, true));
        if let Some(tail) = step["tail"].as_str().filter(|tail| !tail.is_empty()) {
            output.push(Descriptor::Text(
                rich_document::paragraphs(&[
                    json!({"code":true,"depth":2,"size":11,"runs":[{"text":tail}]}),
                ]),
                true,
            ));
        }
    }
    if let Some(error) = preparation["error"].as_str() {
        output.push(Descriptor::Text(
            rich_document::paragraphs(&[
                json!({"size":12,"runs":[{"text":error,"darkColor":0xff8e86u32}]}),
            ]),
            true,
        ));
    }
}

fn append_part(
    output: &mut Vec<Descriptor>,
    part: &Value,
    key: (&str, usize),
    state: BlockState,
    width: i32,
    media: &Value,
    document: &Value,
) {
    let labels = &document["labels"];
    if let Some(blocks) = part["Text"].as_array() {
        let mut text = rich_document::paragraphs(blocks);
        text.shimmer = serde_json::from_value(document["shimmer"].clone()).ok();
        output.push(Descriptor::Text(text, true));
    } else if let Some(code) = part.get("Code") {
        code_parts(output, code, key, state, width, labels);
    } else if let Some(table) = part.get("Table") {
        let rows = table["rows"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        output.push(Descriptor::Text(
            rich_document::table(
                rows,
                width.max(160) as u32,
                media,
                &document["mediaErrors"],
                labels,
            ),
            false,
        ));
        output.push(Descriptor::Button(
            labels["copyTable"].as_str().unwrap_or_default().into(),
            json!({"action":"tableMenu","iconOnly":true,"icon":"copy","menuItems":[
                {"id":"copyText","title":labels["copyMarkdown"],"text":table["source"]},
                {"id":"copyText","title":labels["copyCsv"],"text":table["csv"]}
            ]}),
        ));
    } else if let Some(source) = part["Image"]["source"].as_str() {
        picture_parts(output, source, media, document, width);
    } else if part.get("Rule").is_some() {
        output.push(Descriptor::Rule);
    } else if let Some(link) = part.get("Link") {
        output.push(Descriptor::Button(
            labels["openFile"].as_str().unwrap_or_default().into(),
            json!({"action":"link","text":link["url"]}),
        ));
    }
}

fn picture_parts(
    output: &mut Vec<Descriptor>,
    source: &str,
    media: &Value,
    document: &Value,
    width: i32,
) {
    let labels = &document["labels"];
    let reason = document["mediaErrors"][source].as_str();
    let data = media[source].as_str().map(str::to_owned);
    let label = labels[if reason.is_some() {
        "retry"
    } else if data.is_some() {
        "openImage"
    } else {
        "loadingImage"
    }]
    .as_str()
    .unwrap_or_default()
    .into();
    output.push(Descriptor::Picture {
        source: source.into(),
        data,
        error: reason.map(str::to_owned),
        label,
    });
}

fn code_parts(
    output: &mut Vec<Descriptor>,
    code: &Value,
    key: (&str, usize),
    state: BlockState,
    width: i32,
    labels: &Value,
) {
    let (row, index) = key;
    let blocks = code["blocks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let source = rich_document::paragraphs(blocks);
    let diagram = code["diagram"].is_object();
    let showing_source = !diagram || state.source || source.active.is_some();
    output.push(Descriptor::Caption(
        code["label"].as_str().unwrap_or_default().into(),
        false,
    ));
    if diagram {
        output.push(Descriptor::Button(
            labels[if showing_source { "diagram" } else { "source" }]
                .as_str()
                .unwrap_or_default()
                .into(),
            json!({"action":"diagramSource","row":row,"part":index}),
        ));
        if !showing_source {
            output.push(Descriptor::Button(
                labels[if state.large { "shrink" } else { "enlarge" }]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                json!({"action":"diagramSize","row":row,"part":index}),
            ));
        }
    }
    output.push(Descriptor::Button(
        labels[if diagram { "copyDiagram" } else { "copyCode" }]
            .as_str()
            .unwrap_or_default()
            .into(),
        json!({"action":"copyText","text":code["source"],"iconOnly":true,"icon":"copy"}),
    ));
    if showing_source {
        output.push(Descriptor::Button(
            labels[if state.wrap { "unwrap" } else { "wrap" }]
                .as_str()
                .unwrap_or_default()
                .into(),
            json!({"action":"wrapCode","row":row,"part":index,"iconOnly":true,"icon":"wrap"}),
        ));
        output.push(Descriptor::Text(source, state.wrap));
    } else {
        let ratio = code["diagram"]["width"].as_f64().unwrap_or(1.0)
            / code["diagram"]["height"].as_f64().unwrap_or(1.0).max(1.0);
        let maximum = if state.large { 640.0 } else { 300.0 };
        let width = width.min((ratio * maximum) as i32).max(1) as u32;
        if let Some(image) = code["diagram"]["png"]
            .as_str()
            .and_then(|data| rich_document::picture(data, width))
        {
            output.push(Descriptor::Text(image, true));
        }
    }
}

impl Item {
    pub fn is_icon(&self) -> bool {
        self.edit.context.icon_kind
    }
    pub fn is_picture(&self) -> bool {
        self.edit.context.picture_kind
    }
    pub fn picture_width(&self, maximum: i32, scale: f32) -> i32 {
        self.edit
            .context
            .picture
            .borrow()
            .as_ref()
            .map_or((256.0 * scale) as i32, |image| {
                ((image.width as f32 * 192.0 / image.height.max(1) as f32).max(96.0) * scale) as i32
            })
            .min(maximum)
    }
    pub fn cell(&self, x: i32, y: i32, width: i32, height: i32) {
        self.edit.context.cover.set(true);
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
    }
    pub fn picture_clip(&self, tile: RECT, group: SIZE, radius: i32) {
        super::picture::clip(self.edit.window, tile, group, radius);
    }
    pub fn new(
        descriptor: Descriptor,
        parent: HWND,
        owner: &ComponentContext,
        scope: &str,
        row: &Value,
    ) -> Self {
        let button = matches!(
            descriptor,
            Descriptor::Button(..) | Descriptor::Picture { .. }
        );
        let context = Rc::new(EditContext {
            height: Cell::new(28),
            width: Cell::new(0),
            owner: owner.clone(),
            parent,
            payload: RefCell::new(Value::Null),
            document: RefCell::new(Document::default()),
            wrap: Cell::new(true),
            button,
            caption: matches!(descriptor, Descriptor::Caption(..)),
            rule: matches!(descriptor, Descriptor::Rule),
            metadata: Cell::new(false),
            compact: Cell::new(false),
            symbol: Cell::new(None),
            icon_only: Cell::new(false),
            revealed: RefCell::new(None),
            copied: Cell::new(false),
            icon_kind: matches!(descriptor, Descriptor::Icon(_)),
            picture_kind: matches!(descriptor, Descriptor::Picture { .. }),
            picture: RefCell::new(None),
            cover: Cell::new(false),
            tooltip: RefCell::new(None),
            effects: RefCell::new(super::effects::Effects::default()),
        });
        let class = if button {
            w!("BUTTON")
        } else if matches!(descriptor, Descriptor::Rule | Descriptor::Icon(_)) {
            w!("STATIC")
        } else {
            rich_edit_class()
        };
        let style = WS_CHILD
            | WS_VISIBLE
            | WS_CLIPSIBLINGS
            | if button {
                WS_TABSTOP
            } else if matches!(descriptor, Descriptor::Icon(_)) {
                WINDOW_STYLE(0x0D)
            } else if matches!(descriptor, Descriptor::Rule) {
                WINDOW_STYLE(0x10)
            } else {
                WINDOW_STYLE(0x4 | 0x800 | 0x100) | WS_HSCROLL | WS_TABSTOP
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
        if !button && !context.rule && !context.icon_kind {
            message(window, EM_EXLIMITTEXT, 0, i32::MAX as isize);
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
        if self.button
            != matches!(
                descriptor,
                Descriptor::Button(..) | Descriptor::Picture { .. }
            )
            || self.edit.context.picture_kind != matches!(descriptor, Descriptor::Picture { .. })
            || self.edit.context.caption != matches!(descriptor, Descriptor::Caption(..))
            || self.edit.context.rule != matches!(descriptor, Descriptor::Rule)
            || self.edit.context.icon_kind != matches!(descriptor, Descriptor::Icon(_))
        {
            return false;
        }
        let base = json!({"bubble":row["user"] == true || row["plan"] == true,"scope":scope,"row":row["id"],"target":row["document"]["target"],"menu":row["document"]["menu"],"sideChat":row["document"]["sideChat"],"findKey":row["document"]["findKey"],"agentStatus":row["agentStatus"],"copiedLabel":row["document"]["labels"]["copied"],"saveImageLabel":row["document"]["labels"]["saveImage"]});
        if self.edit.context.payload.borrow()["row"] != row["id"] {
            self.edit.context.copied.set(false);
        }
        match descriptor {
            Descriptor::Icon(name) => {
                *self.edit.context.payload.borrow_mut() = base;
                self.edit.icon(Some(name), true);
                self.edit.context.effects.borrow_mut().configure(
                    self.edit.window,
                    serde_json::from_value(row["document"]["shimmer"].clone()).ok(),
                );
            }
            Descriptor::Button(title, action) => {
                let visible_title = title.clone();
                let title: Vec<_> = action["accessibilityLabel"]
                    .as_str()
                    .unwrap_or(title)
                    .encode_utf16()
                    .chain([0])
                    .collect();
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
                base["title"] = json!(visible_title);
                for key in ["action", "text", "part", "menuItems"] {
                    if let Some(value) = action.get(key) {
                        base[key] = value.clone();
                    }
                }
                *self.edit.context.payload.borrow_mut() = base;
                self.edit.context.effects.borrow_mut().configure(
                    self.edit.window,
                    serde_json::from_value(action["shimmer"].clone()).ok(),
                );
            }
            Descriptor::Picture {
                source,
                data,
                error,
                label,
            } => {
                let changed = self.edit.context.payload.borrow()["imageSource"].as_str()
                    != Some(source.as_str());
                if changed || data.is_none() {
                    self.edit.context.picture.borrow_mut().take();
                }
                if self.edit.context.picture.borrow().is_none() {
                    *self.edit.context.picture.borrow_mut() =
                        data.as_deref().and_then(super::picture::Picture::decode);
                }
                let mut base = base;
                base["action"] = json!(if error.is_some() {
                    "retryImage"
                } else {
                    "image"
                });
                base["text"] = json!(source);
                base["imageSource"] = json!(source);
                base["error"] = json!(error);
                base["title"] = json!(label);
                base["sent"] = json!(
                    row["user"] == true
                        && row["images"].as_array().is_some_and(|images| images
                            .iter()
                            .any(|image| image.as_str() == Some(source.as_str())))
                );
                *self.edit.context.payload.borrow_mut() = base;
                let title: Vec<_> = label.encode_utf16().chain([0]).collect();
                unsafe {
                    let _ = SetWindowTextW(self.edit.window, PCWSTR(title.as_ptr()));
                    let _ = EnableWindow(self.edit.window, data.is_some() || error.is_some());
                }
                self.edit.icon(None, false);
            }
            Descriptor::Text(document, wrap) => {
                *self.edit.context.payload.borrow_mut() = base;
                self.text(document, *wrap);
            }
            Descriptor::Caption(text, hover) => {
                self.edit.context.metadata.set(*hover);
                *self.edit.context.payload.borrow_mut() = base;
                self.text(
                    &rich_document::paragraphs(&[json!({"size":12,"runs":[{"text":text}]})]),
                    true,
                );
            }
            Descriptor::Rule => {}
        }
        true
    }
    fn text(&self, document: &Document, wrap: bool) {
        let background = document.background.unwrap_or(
            if self.edit.context.payload.borrow()["bubble"] == true {
                0x141414
            } else {
                0x080808
            },
        );
        message(self.edit.window, EM_SETBKGNDCOLOR, 0, background as isize);
        let mut effects = self.edit.context.effects.borrow_mut();
        effects.background = background;
        effects.configure(self.edit.window, document.shimmer.clone());
        drop(effects);
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
        let meta = self.edit.context.metadata.get()
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
    pub fn place(&mut self, x: i32, y: i32, width: i32, scale: f32, maximum: Option<i32>) -> i32 {
        let context = &self.edit.context;
        let changed = context.width.replace(width) != width;
        context.cover.set(false);
        let height = if context.picture_kind {
            if context.payload.borrow()["sent"] == true {
                context.cover.set(true);
                (192.0 * scale) as i32
            } else {
                context
                    .picture
                    .borrow()
                    .as_ref()
                    .map_or((270.0 * scale) as i32, |image| {
                        ((image.height as f32
                            * (width as f32 / image.width.max(1) as f32).min(scale))
                        .min(480.0 * scale)) as i32
                    })
            }
        } else if context.rule {
            (1.0 * scale) as i32
        } else if self.button || context.caption || context.icon_kind {
            (26.0 * scale) as i32
        } else {
            context.height.get()
                + if context.wrap.get() {
                    0
                } else {
                    (17.0 * scale) as i32
                }
        };
        let height = maximum.map_or(height, |maximum| height.min(maximum.max(0)));
        context.effects.borrow_mut().fade =
            maximum.is_some_and(|maximum| maximum > 0 && context.height.get() > maximum);
        if context.picture_kind && context.payload.borrow()["sent"] == true {
            self.picture_clip(
                RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
                SIZE {
                    cx: width,
                    cy: height,
                },
                (16.0 * scale) as i32,
            );
        }
        if maximum.is_some() {
            unsafe {
                let _ = ShowWindow(
                    self.edit.window,
                    if height > 0 {
                        SW_SHOWNOACTIVATE
                    } else {
                        SW_HIDE
                    },
                );
            }
        }
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
        if !self.button && !context.rule && !context.icon_kind && changed {
            message(
                self.edit.window,
                EM_SETTARGETDEVICE,
                0,
                if context.wrap.get() { 0 } else { 1 },
            );
            message(self.edit.window, EM_REQUESTRESIZE, 0, 0);
        }
        self.edit.reveal();
        height
    }
}

impl Edit {
    fn reveal(&self) {
        let found = self.context.payload.borrow()["findKey"]
            .as_str()
            .zip(self.context.document.borrow().active)
            .map(|(key, range)| (key.to_owned(), range));
        if *self.context.revealed.borrow() == found {
            return;
        }
        *self.context.revealed.borrow_mut() = found.clone();
        if let Some((_, (start, end))) = found {
            let mut selected = self.range();
            let mut active = CHARRANGE {
                cpMin: start as i32,
                cpMax: end as i32,
            };
            message(
                self.window,
                EM_EXSETSEL,
                0,
                (&mut active as *mut CHARRANGE) as isize,
            );
            message(self.window, EM_SCROLLCARET, 0, 0);
            message(
                self.window,
                EM_EXSETSEL,
                0,
                (&mut selected as *mut CHARRANGE) as isize,
            );
        }
    }
    fn icon(&self, name: Option<&str>, only: bool) {
        // OS glyphs are independent of the semantic WindowText accessibility name.
        self.context.symbol.set(match name {
            Some("agent") => Some(0xe716),
            Some("terminal") => Some(0xe756),
            Some("eye") => Some(0xe7b3),
            Some("zap") => Some(0xe945),
            Some("alert") => Some(0xe7ba),
            Some("stop") => Some(0xe71a),
            Some("compact") => Some(0xe8a0),
            Some("tools" | "tool") => Some(0xe90f),
            Some("thinking") => Some(0xe82f),
            Some("globe") => Some(0xe774),
            Some("copy") => Some(0xe8c8),
            Some("edit") => Some(0xe70f),
            Some("fork") => Some(0xef90),
            Some("more") => Some(0xe712),
            Some("chevronDown") => Some(0xe70d),
            Some("chevronRight") => Some(0xe76c),
            Some("check") => Some(0xe8fb),
            Some("close") => Some(0xe711),
            Some("minus") => Some(0xe738),
            Some("pending") => Some(0xea3a),
            Some("running") => Some(0xe895),
            Some("wrap") => Some(0xe751),
            _ => None,
        });
        self.context.icon_only.set(only);
        unsafe {
            let style = GetWindowLongPtrW(self.window, GWL_STYLE);
            let _ = SetWindowLongPtrW(
                self.window,
                GWL_STYLE,
                if self.context.icon_kind {
                    (style & !0x1F) | 0x0D
                } else {
                    (style & !(BS_TYPEMASK as isize))
                        | if self.context.symbol.get().is_some() || self.context.picture_kind {
                            BS_OWNERDRAW as isize
                        } else {
                            BS_PUSHBUTTON as isize
                        }
                        | BS_FLAT as isize
                },
            );
        }
    }
    pub unsafe fn draw(item: &DRAWITEMSTRUCT) -> bool {
        let Some(context) = (unsafe { Self::context(item.hwndItem) }) else {
            return false;
        };
        if context.picture_kind {
            unsafe {
                super::picture::background(item);
            }
            if let Some(image) = context.picture.borrow().as_ref() {
                unsafe {
                    image.draw(item, context.cover.get());
                }
            } else {
                let payload = context.payload.borrow();
                let text = [
                    payload["error"].as_str().unwrap_or_default(),
                    payload["title"].as_str().unwrap_or_default(),
                ]
                .join("\n");
                let mut text: Vec<_> = text.encode_utf16().collect();
                let mut rect = item.rcItem;
                unsafe {
                    SetBkMode(item.hDC, TRANSPARENT);
                    SetTextColor(item.hDC, COLORREF(0xaaaaaa));
                    DrawTextW(
                        item.hDC,
                        &mut text,
                        &mut rect,
                        DT_CENTER | DT_VCENTER | DT_WORDBREAK,
                    );
                }
            }
            if item.itemState.0 & ODS_FOCUS.0 != 0 {
                unsafe {
                    DrawFocusRect(item.hDC, &item.rcItem);
                }
            }
            return true;
        }
        let Some(mut symbol) = context.symbol.get() else {
            return false;
        };
        if context.copied.get() {
            symbol = 0xe8fb;
        }
        let scale = unsafe { GetDpiForWindow(item.hwndItem) }.max(96) as f32 / 96.0;
        unsafe {
            let saved = SaveDC(item.hDC);
            let brush = CreateSolidBrush(COLORREF(if item.itemState.0 & ODS_SELECTED.0 != 0 {
                0x282828
            } else {
                0x080808
            }));
            FillRect(item.hDC, &item.rcItem, brush);
            let _ = DeleteObject(brush.into());
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(
                item.hDC,
                COLORREF(if item.itemState.0 & ODS_DISABLED.0 != 0 {
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
            let agent = context.payload.borrow()["action"] == "agent";
            if !context.icon_only.get() {
                if agent {
                    rect.right = rect.left + (24.0 * scale) as i32;
                } else {
                    rect.left = (rect.right - (20.0 * scale) as i32).max(rect.left);
                }
            }
            DrawTextW(
                item.hDC,
                &mut [symbol],
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if agent {
                let status = context.payload.borrow()["agentStatus"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let color = match status.as_str() {
                    "completed" => 0x71ad27,
                    "running" => 0xffa560,
                    "failed" => 0x4545ef,
                    _ => 0x888888,
                };
                let brush = CreateSolidBrush(COLORREF(color));
                let old = SelectObject(item.hDC, brush.into());
                let x = (15.0 * scale) as i32;
                let y = (17.0 * scale) as i32;
                Ellipse(
                    item.hDC,
                    x,
                    y,
                    x + (8.0 * scale) as i32,
                    y + (8.0 * scale) as i32,
                );
                SelectObject(item.hDC, old);
                let _ = DeleteObject(brush.into());
            }
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
                let mut title: Vec<_> = context.payload.borrow()["title"]
                    .as_str()
                    .unwrap_or_default()
                    .encode_utf16()
                    .collect();
                let length = title.len();
                let mut rect = item.rcItem;
                rect.left += (if agent { 28.0 } else { 4.0 } * scale) as i32;
                if !agent {
                    rect.right -= (20.0 * scale) as i32;
                }
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
            if item.itemState.0 & ODS_FOCUS.0 != 0 {
                DrawFocusRect(item.hDC, &item.rcItem);
            }
        }
        true
    }
    pub unsafe fn row(window: HWND) -> Option<String> {
        let context = unsafe { Self::context(window) }?;
        context.payload.borrow()["row"].as_str().map(str::to_owned)
    }

    pub unsafe fn copied(window: HWND) {
        if let Some(context) = unsafe { Self::context(window) } {
            context.copied.set(true);
            let title: Vec<_> = context.payload.borrow()["copiedLabel"]
                .as_str()
                .unwrap_or_default()
                .encode_utf16()
                .chain([0])
                .collect();
            unsafe {
                let _ = SetWindowTextW(window, PCWSTR(title.as_ptr()));
                SetTimer(Some(window), COPY_TIMER, 2000, None);
                let _ = InvalidateRect(Some(window), None, false);
            }
        }
    }

    fn set_document(&self, next: &Document) {
        let range = self.range();
        let selected = self.selected(range);
        let retained = range.cpMax.max(0) as usize <= next.text.encode_utf16().count()
            && next.selected(range.cpMin.max(0) as usize, range.cpMax.max(0) as usize) == selected;
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
        self.context
            .document
            .borrow()
            .selected(range.cpMin.max(0) as usize, range.cpMax.max(0) as usize)
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
        let destination = context
            .document
            .borrow()
            .links
            .iter()
            .find(|link| {
                link.start <= range.cpMin.max(0) as usize
                    && (range.cpMin.max(0) as usize) < link.end
            })
            .map(|link| (link.destination.clone(), link.action));
        if let Some((destination, kind)) = destination {
            let mut action = context.payload.borrow().clone();
            action["action"] = json!(if matches!(kind, "image" | "retryImage") {
                kind
            } else {
                "link"
            });
            action["text"] = json!(destination);
            action["command"] = json!(unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0);
            context.owner.emit("action", action);
        }
    }
}

impl Drop for Edit {
    fn drop(&mut self) {
        self.context.tooltip.borrow_mut().take();
        unsafe {
            let _ = RemoveWindowSubclass(self.window, Some(procedure), SUBCLASS);
            let _ = DestroyWindow(self.window);
        }
    }
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
    let text = context
        .document
        .borrow()
        .selected(range.cpMin.max(0) as usize, range.cpMax.max(0) as usize);
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
    if msg == WM_TIMER && w.0 == COPY_TIMER {
        context.copied.set(false);
        let title: Vec<_> = context.payload.borrow()["title"]
            .as_str()
            .unwrap_or_default()
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            let _ = KillTimer(Some(hwnd), COPY_TIMER);
            let _ = SetWindowTextW(hwnd, PCWSTR(title.as_ptr()));
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        return LRESULT(0);
    }
    if msg == WM_TIMER && w.0 == super::effects::TIMER {
        super::effects::Effects::tick(hwnd);
        return LRESULT(0);
    }
    if msg == WM_MOUSEMOVE {
        unsafe {
            let _ = PostMessageW(
                Some(context.parent),
                super::HOVER,
                WPARAM(0),
                LPARAM(hwnd.0 as isize),
            );
        }
        if !context.button && !context.icon_kind && !context.rule {
            let point = POINT {
                x: l.0 as i16 as i32,
                y: (l.0 >> 16) as i16 as i32,
            };
            let at = message(hwnd, EM_CHARFROMPOS, 0, (&point as *const POINT) as isize)
                .0
                .max(0) as usize;
            let title = context
                .document
                .borrow()
                .links
                .iter()
                .find(|link| link.start <= at && at < link.end)
                .map(|link| link.tooltip.clone())
                .unwrap_or_default();
            let mut tooltip = context.tooltip.borrow_mut();
            if tooltip.is_none() && !title.is_empty() {
                *tooltip = super::tooltip::Tooltip::new(hwnd);
            }
            if let Some(tooltip) = tooltip.as_mut() {
                tooltip.update(&title);
            }
        }
    }
    let text_control = !context.button && !context.icon_kind && !context.rule;
    if text_control
        && (msg == WM_COPY
            || (msg == WM_KEYDOWN
                && w.0 == VK_C.0 as usize
                && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0))
    {
        let mut range = CHARRANGE::default();
        message(
            hwnd,
            EM_EXGETSEL,
            0,
            (&mut range as *mut CHARRANGE) as isize,
        );
        let text = context
            .document
            .borrow()
            .selected(range.cpMin.max(0) as usize, range.cpMax.max(0) as usize);
        if !text.is_empty() {
            let mut value = context.payload.borrow().clone();
            value["action"] = json!("copyText");
            value["text"] = json!(text);
            context.owner.emit("action", value);
        }
        return LRESULT(0);
    }
    if text_control && msg == WM_MOUSEWHEEL {
        return message(context.parent, msg, w.0, l.0);
    }
    if context.picture_kind && msg == WM_CONTEXTMENU {
        if context.picture.borrow().is_some() {
            let mut payload = context.payload.borrow().clone();
            if choose_menu(
                hwnd,
                &[json!({"id":"saveImage","title":payload["saveImageLabel"]})],
            )
            .is_some()
            {
                payload["action"] = json!("saveImage");
                context.owner.emit("action", payload);
            }
        }
        return LRESULT(0);
    }
    if text_control && msg == WM_CONTEXTMENU {
        popup(hwnd, &context);
        return LRESULT(0);
    }
    let result = unsafe { DefSubclassProc(hwnd, msg, w, l) };
    if msg == WM_PAINT {
        context.effects.borrow_mut().paint(hwnd);
    }
    if text_control && msg == WM_LBUTTONUP {
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
    if let Some(chosen) = choose_menu(window, link.menu.as_array().unwrap()) {
        let mut value = payload;
        value["action"] = if matches!(link.action, "image" | "retryImage") {
            chosen["id"].clone()
        } else {
            json!(link.action)
        };
        value["id"] = chosen["id"].clone();
        value["text"] = json!(link.destination);
        context.owner.emit("action", value);
    }
    true
}

pub(super) fn menu_action(window: HWND, mut action: Value) -> Option<Value> {
    let chosen = choose_menu(window, action["menuItems"].as_array()?)?;
    action.as_object_mut()?.remove("menuItems");
    action["action"] = chosen["id"].clone();
    if let Some(text) = chosen.get("text") {
        action["text"] = text.clone();
    }
    Some(action)
}

fn choose_menu(window: HWND, items: &[Value]) -> Option<Value> {
    let mut actions = Vec::new();
    unsafe fn append(menu: HMENU, items: &[Value], actions: &mut Vec<Value>) {
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
            } else if item["id"].is_string() {
                actions.push(item.clone());
                unsafe {
                    let _ = AppendMenuW(
                        menu,
                        MF_STRING
                            | if item["enabled"] == false {
                                MF_GRAYED
                            } else {
                                MF_ENABLED
                            },
                        actions.len(),
                        PCWSTR(title.as_ptr()),
                    );
                }
            }
        }
    }
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        append(menu, items, &mut actions);
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
        chosen
            .checked_sub(1)
            .and_then(|index| actions.get(index as usize))
            .cloned()
    }
}
