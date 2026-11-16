//! Paint effects over the OS control's own rendered pixels. Rich Edit still
//! owns text layout, native selection and all character positions.
use crate::native::effects::{Shimmer, tint};
use std::sync::OnceLock;
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};
use windows::core::BOOL;

pub(super) const TIMER: usize = 2;

#[derive(Default)]
pub(super) struct Effects {
    pub shimmer: Option<Shimmer>,
    pub fade: bool,
    pub background: u32,
    surface: Option<Surface>,
}

impl Effects {
    pub fn configure(&mut self, window: HWND, shimmer: Option<Shimmer>) {
        if self.shimmer == shimmer {
            return;
        }
        self.shimmer = shimmer;
        unsafe {
            if self.shimmer.is_some() {
                SetTimer(Some(window), TIMER, 33, None);
            } else {
                let _ = KillTimer(Some(window), TIMER);
            }
        }
    }

    pub fn tick(window: HWND) {
        unsafe {
            if IsWindowVisible(window).as_bool()
                && !IsIconic(GetAncestor(window, GA_ROOT)).as_bool()
            {
                let _ = InvalidateRect(Some(window), None, false);
            }
        }
    }

    pub fn paint(&mut self, window: HWND) {
        if !self.fade && self.shimmer.is_none() {
            return;
        }
        let mut rect = RECT::default();
        let mut animate = BOOL(1);
        unsafe {
            let _ = GetClientRect(window, &mut rect);
            let _ = SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some((&mut animate as *mut BOOL).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let size = SIZE {
            cx: rect.right,
            cy: rect.bottom,
        };
        if size.cx <= 0 || size.cy <= 0 {
            return;
        }
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.size != size)
        {
            self.surface = Surface::new(size);
        }
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        super::control::message(
            window,
            WM_PRINTCLIENT,
            surface.dc.0 as usize,
            (PRF_CLIENT | PRF_ERASEBKGND) as isize,
        );
        unsafe {
            GdiFlush();
        }
        let scale = unsafe { GetDpiForWindow(window) }.max(96) as f64 / 96.0;
        let shimmer = self.shimmer.as_ref().filter(|_| animate.as_bool());
        surface.apply(shimmer, self.fade, self.background, scale);
        unsafe {
            let dc = GetDC(Some(window));
            let _ = BitBlt(dc, 0, 0, size.cx, size.cy, Some(surface.dc), 0, 0, SRCCOPY);
            ReleaseDC(Some(window), dc);
        }
    }
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u8,
    size: SIZE,
}

impl Surface {
    fn new(size: SIZE) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size.cx,
                    biHeight: -size.cy,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = std::ptr::null_mut();
            let bitmap =
                match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0) {
                    Ok(bitmap) => bitmap,
                    Err(_) => {
                        DeleteDC(dc);
                        return None;
                    }
                };
            let previous = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                previous,
                pixels: pixels.cast(),
                size,
            })
        }
    }

    fn apply(&mut self, shimmer: Option<&Shimmer>, fade: bool, background: u32, scale: f64) {
        static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
        let seconds = EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64();
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                self.pixels,
                self.size.cx as usize * self.size.cy as usize * 4,
            )
        };
        let columns: Vec<_> = (0..self.size.cx)
            .map(|x| {
                shimmer.map_or(0.0, |shimmer| {
                    shimmer.opacity(x as f64, self.size.cx as f64, seconds, scale)
                })
            })
            .collect();
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let y = (index / self.size.cx as usize) as f64;
            let fade = if fade {
                ((y / self.size.cy as f64 - 0.8) / 0.2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            tint(
                pixel,
                columns[index % self.size.cx as usize],
                fade,
                background,
            );
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            DeleteDC(self.dc);
        }
    }
}
