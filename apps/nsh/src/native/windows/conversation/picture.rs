//! Pixels drawn inside a real native image button. The transcript retains the file.
use base64::Engine;
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::Controls::DRAWITEMSTRUCT};

pub(super) struct Picture {
    pub width: i32,
    pub height: i32,
    pixels: Vec<u8>,
}

impl Picture {
    pub fn decode(data: &str) -> Option<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let (width, height) = image.dimensions();
        let mut pixels = image.into_raw();
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = u32::from(pixel[3]);
            for value in &mut pixel[..3] {
                *value = ((u32::from(*value) * alpha + 8 * (255 - alpha)) / 255) as u8;
            }
            pixel.swap(0, 2);
        }
        Some(Self {
            width: i32::try_from(width).ok()?,
            height: i32::try_from(height).ok()?,
            pixels,
        })
    }

    pub unsafe fn draw(&self, item: &DRAWITEMSTRUCT, cover: bool) {
        let width = item.rcItem.right - item.rcItem.left;
        let height = item.rcItem.bottom - item.rcItem.top;
        let x = width as f32 / self.width.max(1) as f32;
        let y = height as f32 / self.height.max(1) as f32;
        let scale = if cover { x.max(y) } else { x.min(y) };
        let drawn_width = (self.width as f32 * scale) as i32;
        let drawn_height = (self.height as f32 * scale) as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.width,
                biHeight: -self.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            StretchDIBits(
                item.hDC,
                (width - drawn_width) / 2,
                (height - drawn_height) / 2,
                drawn_width,
                drawn_height,
                0,
                0,
                self.width,
                self.height,
                Some(self.pixels.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    }
}

pub(super) unsafe fn background(item: &DRAWITEMSTRUCT) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(0x080808));
        FillRect(item.hDC, &item.rcItem, brush);
        let _ = DeleteObject(brush.into());
    }
}
