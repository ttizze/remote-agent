//! A native tooltip owns the UTF-16 storage retained by the OS control.
use windows::{
    Win32::{
        Foundation::*,
        UI::{Controls::*, WindowsAndMessaging::*},
    },
    core::{PWSTR, w},
};

pub(super) struct Tooltip {
    window: HWND,
    target: HWND,
    text: Vec<u16>,
}

impl Tooltip {
    pub fn new(target: HWND) -> Option<Self> {
        unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            });
        }
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                TOOLTIPS_CLASSW,
                w!(""),
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(target),
                None,
                None,
                None,
            )
        }
        .ok()?;
        let mut result = Self {
            window,
            target,
            text: vec![0],
        };
        let mut tool = result.tool();
        super::control::message(
            window,
            TTM_ADDTOOLW,
            0,
            (&mut tool as *mut TTTOOLINFOW) as isize,
        );
        super::control::message(window, TTM_SETMAXTIPWIDTH, 0, 600);
        Some(result)
    }
    fn tool(&mut self) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TOOLTIP_FLAGS(TTF_IDISHWND.0 | TTF_SUBCLASS.0),
            hwnd: self.target,
            uId: self.target.0 as usize,
            lpszText: PWSTR(self.text.as_mut_ptr()),
            ..Default::default()
        }
    }
    pub fn update(&mut self, text: &str) {
        let next: Vec<_> = text.encode_utf16().chain([0]).collect();
        if self.text == next {
            return;
        }
        self.text = next;
        let mut tool = self.tool();
        super::control::message(
            self.window,
            TTM_UPDATETIPTEXTW,
            0,
            (&mut tool as *mut TTTOOLINFOW) as isize,
        );
        if text.is_empty() {
            super::control::message(self.window, TTM_POP, 0, 0);
        }
    }
}
impl Drop for Tooltip {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}
