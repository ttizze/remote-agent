//! Check the real Host terminal's cursor response without an interpreter.
use crate::Result;
use nix::{
    poll::{PollFd, PollFlags, poll},
    sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr},
};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};

fn cursor_response(bytes: &[u8]) -> bool {
    let Some(position) = bytes
        .strip_prefix(b"\x1b[")
        .and_then(|bytes| bytes.strip_suffix(b"R"))
    else {
        return false;
    };
    let mut coordinates = position.split(|byte| *byte == b';');
    let decimal = |value: Option<&[u8]>| {
        value.is_some_and(|bytes| !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit))
    };
    decimal(coordinates.next()) && decimal(coordinates.next()) && coordinates.next().is_none()
}

pub fn run() -> Result<()> {
    let mut input = std::io::stdin();
    let saved = tcgetattr(&input)?;
    let mut raw = saved.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&input, SetArg::TCSANOW, &raw)?;
    let result = (|| -> Result<()> {
        fn ready(descriptor: BorrowedFd<'_>, milliseconds: u16) -> Result<bool> {
            let mut descriptors = [PollFd::new(descriptor, PollFlags::POLLIN)];
            poll(&mut descriptors, milliseconds)?;
            Ok(descriptors[0]
                .revents()
                .is_some_and(|events| events.contains(PollFlags::POLLIN)))
        }
        std::io::stdout().write_all(b"\x1b[6n")?;
        std::io::stdout().flush()?;
        if !ready(input.as_fd(), 5000)? {
            return Err("Host did not answer the cursor query".into());
        }
        let mut bytes = [0; 128];
        let length = input.read(&mut bytes)?;
        if !cursor_response(&bytes[..length]) || ready(input.as_fd(), 1000)? {
            return Err("Host cursor query response was invalid or duplicated".into());
        }
        Ok(())
    })();
    let restored = tcsetattr(&input, SetArg::TCSANOW, &saved);
    result?;
    restored?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_complete_cursor_response_is_accepted() {
        assert!(cursor_response(b"\x1b[1;80R"));
        for bytes in [
            b"\x1b[1;80R\x1b[1;80R".as_slice(),
            b"\x1b[;1R",
            b"\x1b[1;R",
            b"\x1b[1;2;3R",
            b"\x1b[1;2Rextra",
            b"1;2R",
        ] {
            assert!(!cursor_response(bytes));
        }
    }
}
