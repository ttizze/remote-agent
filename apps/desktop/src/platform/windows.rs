use std::{os::windows::process::CommandExt, process::Command};

use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM},
    Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ},
    System::{
        Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        Threading::GetCurrentProcessId,
    },
    UI::{
        Shell::{ITaskbarList3, TaskbarList},
        WindowsAndMessaging::{
            CreateIconIndirect, DestroyIcon, EnumWindows, GetForegroundWindow,
            GetWindowThreadProcessId, IsWindowVisible, ICONINFO, HICON,
        },
    },
};

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
    let Ok(taskbar) = (unsafe {
        CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
    }) else {
        return;
    };
    if unsafe { taskbar.HrInit() }.is_err() {
        return;
    }
    let icon = (count > 0).then(|| create_badge_icon(count));
    let description = badge_description(count);
    for window in windows {
        let icon_handle = icon.as_ref().copied().unwrap_or_default();
        let _ = unsafe {
            taskbar.SetOverlayIcon(window, icon_handle, PCWSTR(description.as_ptr()))
        };
    }
    if let Some(icon) = icon {
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
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
    let label = if count > 9 { "9+".to_owned() } else { count.to_string() };
    format!("{label} threads with new notifications\0")
        .encode_utf16()
        .collect()
}

fn create_badge_icon(count: u32) -> HICON {
    const SIZE: i32 = 16;
    let mut pixels = vec![0_u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = ((y * SIZE + x) * 4) as usize;
            let inside = (x - 7).pow(2) + (y - 7).pow(2) <= 49;
            if inside {
                pixels[index..index + 4].copy_from_slice(&[0xe5, 0x48, 0x4d, 0xff]);
            }
        }
    }
    let glyph = match count {
        1 => "00100\n01100\n00100\n00100\n01110",
        2 => "01110\n00001\n00110\n01000\n01111",
        3 => "01110\n00001\n00110\n00001\n01110",
        4 => "01010\n01010\n01111\n00010\n00010",
        5 => "01111\n01000\n01110\n00001\n01110",
        6 => "00110\n01000\n01110\n01001\n00110",
        7 => "01111\n00001\n00010\n00100\n00100",
        8 => "00110\n01001\n00110\n01001\n00110",
        9 => "00110\n01001\n00111\n00001\n00110",
        _ => "01110\n00001\n00110\n00000\n00100",
    };
    for (row, line) in glyph.lines().enumerate() {
        for (column, value) in line.bytes().enumerate() {
            if value != b'1' {
                continue;
            }
            let x = column as i32 + 5;
            let y = row as i32 + 5;
            let index = ((y * SIZE + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        }
    }
    let mask_bits = vec![0_u8; (SIZE * SIZE) as usize];
    let mask = unsafe { CreateBitmap(SIZE, SIZE, 1, 1, Some(mask_bits.as_ptr().cast())) };
    let color = unsafe {
        CreateBitmap(SIZE, SIZE, 1, 32, Some(pixels.as_ptr().cast()))
    };
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let icon = unsafe { CreateIconIndirect(&info).unwrap_or_default() };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
    }
    icon
}
