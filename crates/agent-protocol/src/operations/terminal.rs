//! Host-owned PTY request records and the shared xterm palette.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWrite {
    pub process_handle: String,
    #[serde(rename = "deltaBase64", with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalKill {
    pub process_handle: String,
}
/// Shared xterm palette, also used for Host-owned terminal-query replies.
pub fn terminal_color(index: u16) -> u32 {
    const PALETTE: [u32; 16] = [
        0x181818, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let index = u32::from(index);
    match index {
        0..=15 => PALETTE[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            (v << 16) | (v << 8) | v
        }
        257 => 0x181818,
        _ => 0xe5e5e5,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
}
