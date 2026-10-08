use std::{os::windows::process::CommandExt, process::Command};

use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM},
    Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ},
    System::{
        Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        Threading::GetCurrentProcessId,
    },
    UI::{
        Shell::{ITaskbarList3, TaskbarList},
        WindowsAndMessaging::{
            CreateIconIndirect, DestroyIcon, EnumWindows, GetForegroundWindow,
            GetWindowThreadProcessId, HICON, ICONINFO, IsWindowVisible,
        },
    },
};
use windows::core::PCWSTR;

const BADGE_RED_BGRA: [u8; 4] = [0x4d, 0x48, 0xe5, 0xff];

pub(super) fn prepare_host(command: &mut Command) {
    // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW: daemon outlives its view.
    command.creation_flags(0x00000200 | 0x08000000);
}

pub(super) fn set_notification_badge(count: u32) {
    let mut windows = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_process_window),
            LPARAM((&mut windows as *mut Vec<HWND>) as isize),
        );
    }
    if windows.is_empty() {
        return;
    }
    let focused = unsafe { GetForegroundWindow() };
    let count = if windows.iter().any(|window| *window == focused) {
        0
    } else {
        count
    };
    let Ok(taskbar) =
        (unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) })
    else {
        return;
    };
    if unsafe { taskbar.HrInit() }.is_err() {
        return;
    }
    let icon = if count > 0 {
        create_badge_icon(count)
    } else {
        None
    };
    if count > 0 && icon.is_none() {
        return;
    }
    let description = badge_description(count);
    for window in windows {
        let icon_handle = icon.as_ref().copied().unwrap_or_default();
        let _ =
            unsafe { taskbar.SetOverlayIcon(window, icon_handle, PCWSTR(description.as_ptr())) };
    }
    if let Some(icon) = icon {
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
}

pub(super) fn shutdown_notification_badge() {
    set_notification_badge(0);
}

unsafe extern "system" fn collect_process_window(window: HWND, data: LPARAM) -> BOOL {
    if !IsWindowVisible(window).as_bool() {
        return BOOL(1);
    }
    let mut process_id = 0;
    GetWindowThreadProcessId(window, Some(&mut process_id));
    if process_id == GetCurrentProcessId() {
        (&mut *(data.0 as *mut Vec<HWND>)).push(window);
    }
    BOOL(1)
}

fn badge_description(count: u32) -> Vec<u16> {
    let label = if count > 9 {
        "9+".to_owned()
    } else {
        count.to_string()
    };
    format!("{label} threads with new notifications\0")
        .encode_utf16()
        .collect()
}

fn create_badge_icon(count: u32) -> Option<HICON> {
    const SIZE: i32 = 64;
    let mut pixels = vec![0_u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = ((y * SIZE + x) * 4) as usize;
            let inside = (x - 32).pow(2) + (y - 32).pow(2) <= 28_i32.pow(2);
            if inside {
                pixels[index..index + 4].copy_from_slice(&BADGE_RED_BGRA);
            }
        }
    }
    let glyph = badge_glyph(count);
    let glyph_width = glyph.lines().map(str::len).max().unwrap_or_default() as i32;
    let scale = if count > 9 { 5 } else { 8 };
    let start_x = (SIZE - glyph_width * scale) / 2;
    let start_y = (SIZE - 5 * scale) / 2;
    for (row, line) in glyph.lines().enumerate() {
        for (column, value) in line.bytes().enumerate() {
            if value != b'1' {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = start_x + column as i32 * scale + dx;
                    let y = start_y + row as i32 * scale + dy;
                    let index = ((y * SIZE + x) * 4) as usize;
                    pixels[index..index + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
                }
            }
        }
    }
    let mask_bits = vec![0_u8; (SIZE * SIZE) as usize];
    let mask = unsafe { CreateBitmap(SIZE, SIZE, 1, 1, Some(mask_bits.as_ptr().cast())) };
    let color = unsafe { CreateBitmap(SIZE, SIZE, 1, 32, Some(pixels.as_ptr().cast())) };
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let icon = unsafe { CreateIconIndirect(&info).ok() };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
    }
    icon
}

fn badge_glyph(count: u32) -> &'static str {
    match count {
        1 => "00100\n01100\n00100\n00100\n01110",
        2 => "01110\n00001\n00110\n01000\n01111",
        3 => "01110\n00001\n00110\n00001\n01110",
        4 => "01010\n01010\n01111\n00010\n00010",
        5 => "01111\n01000\n01110\n00001\n01110",
        6 => "00110\n01000\n01110\n01001\n00110",
        7 => "01111\n00001\n00010\n00100\n00100",
        8 => "00110\n01001\n00110\n01001\n00110",
        9 => "00110\n01001\n00111\n00001\n00110",
        _ => "1110000\n1010010\n1111111\n0010010\n1110010",
    }
}

#[cfg(test)]
mod tests {
    use super::{BADGE_RED_BGRA, badge_glyph};

    #[test]
    fn large_counts_use_the_source_9_plus_glyph_and_red_bgra() {
        assert!(badge_glyph(10).contains("1111111"));
        assert!(!badge_glyph(10).contains('?'));
        assert_eq!(BADGE_RED_BGRA, [0x4d, 0x48, 0xe5, 0xff]);
    }
}
