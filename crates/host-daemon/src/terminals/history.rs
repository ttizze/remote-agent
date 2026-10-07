//! A terminal's history: its output as text, bounded to the newest lines and
//! bytes and without the query and reply traffic that a replay would answer
//! again, kept in one file per thread terminal.
use agent_domain::ThreadId;
use std::{collections::VecDeque, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) lines: usize,
    pub(crate) bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            lines: 5_000,
            bytes: 8 * 1024 * 1024,
        }
    }
}

/// Stored in small pieces, so dropping the oldest text copies little.
const CHUNK_BYTES: usize = 16 * 1024;

/// An unfinished control sequence longer than this is kept as output, so an
/// introducer that is never terminated cannot hold text back without bound.
const MAX_UNFINISHED_CONTROL_BYTES: usize = 64 * 1024;

struct Chunk {
    data: String,
    line_breaks: usize,
}

/// Text that keeps only its newest lines, then only its newest bytes on a
/// character boundary.
pub(crate) struct Bounded {
    limits: Limits,
    chunks: VecDeque<Chunk>,
    bytes: usize,
    line_breaks: usize,
}
impl Bounded {
    pub(crate) fn new(limits: Limits, initial: &str) -> Self {
        let mut text = Self {
            limits,
            chunks: VecDeque::new(),
            bytes: 0,
            line_breaks: 0,
        };
        text.append(initial);
        text
    }

    pub(crate) fn append(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.limits.bytes == 0 || self.limits.lines == 0 {
            self.clear();
            if self.limits.bytes > 0 && text.ends_with('\n') {
                self.push("\n");
            }
            return;
        }
        let mut rest = text;
        while !rest.is_empty() {
            let mut end = rest.len().min(CHUNK_BYTES);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            self.push(&rest[..end]);
            self.trim();
            rest = &rest[end..];
        }
    }

    fn push(&mut self, data: &str) {
        let line_breaks = data.matches('\n').count();
        match self.chunks.back_mut() {
            Some(last) if last.data.len() + data.len() <= CHUNK_BYTES => {
                last.data.push_str(data);
                last.line_breaks += line_breaks;
            }
            _ => self.chunks.push_back(Chunk {
                data: data.to_owned(),
                line_breaks,
            }),
        }
        self.bytes += data.len();
        self.line_breaks += line_breaks;
    }

    /// Drops the first `offset` bytes, holding `line_breaks` newlines, of the
    /// oldest chunk.
    fn drop_front(&mut self, offset: usize, line_breaks: usize) {
        let first = self.chunks.front_mut().expect("text to drop");
        self.bytes -= offset;
        self.line_breaks -= line_breaks;
        if offset == first.data.len() {
            self.chunks.pop_front();
        } else {
            first.data.drain(..offset);
            first.line_breaks -= line_breaks;
        }
    }

    fn trim(&mut self) {
        let terminated = self
            .chunks
            .back()
            .is_some_and(|last| last.data.ends_with('\n'));
        let lines = self.line_breaks + usize::from(!terminated);
        let mut excess = lines.saturating_sub(self.limits.lines);
        while excess > 0 {
            let first = self.chunks.front().expect("lines to drop");
            if first.line_breaks < excess {
                excess -= first.line_breaks;
                let all = first.data.len();
                let breaks = first.line_breaks;
                self.drop_front(all, breaks);
                continue;
            }
            let offset = first
                .data
                .match_indices('\n')
                .nth(excess - 1)
                .map_or(first.data.len(), |(index, _)| index + 1);
            self.drop_front(offset, excess);
            excess = 0;
        }
        while self.bytes > self.limits.bytes {
            let excess = self.bytes - self.limits.bytes;
            let first = self.chunks.front().expect("bytes to drop");
            let mut offset = excess.min(first.data.len());
            while !first.data.is_char_boundary(offset) {
                offset += 1;
            }
            let breaks = first.data[..offset].matches('\n').count();
            self.drop_front(offset, breaks);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
        self.line_breaks = 0;
    }

    pub(crate) fn value(&self) -> String {
        let mut value = String::with_capacity(self.bytes);
        for chunk in &self.chunks {
            value.push_str(&chunk.data);
        }
        value
    }
}

/// A terminal's kept output and the unfinished input its next chunk continues.
pub(crate) struct History {
    text: Bounded,
    control: String,
    utf8: Vec<u8>,
}
impl History {
    pub(crate) fn new(limits: Limits, initial: &str) -> Self {
        Self {
            text: Bounded::new(limits, initial),
            control: String::new(),
            utf8: vec![],
        }
    }

    /// Adds one output chunk; whether the kept text changed.
    pub(crate) fn record(&mut self, data: &[u8]) -> bool {
        let text = decode(&mut self.utf8, data);
        let (mut visible, control) = sanitize(&self.control, &text);
        if control.len() > MAX_UNFINISHED_CONTROL_BYTES {
            visible.push_str(&control);
            self.control.clear();
        } else {
            self.control = control;
        }
        self.text.append(&visible);
        !visible.is_empty()
    }

    /// The process that wrote the unfinished input is gone.
    pub(crate) fn end(&mut self) {
        self.control.clear();
        self.utf8.clear();
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.end();
    }

    pub(crate) fn value(&self) -> String {
        self.text.value()
    }
}

/// Decodes UTF-8 as a stream: a code point split between chunks waits for its
/// end, and invalid bytes become U+FFFD.
fn decode(pending: &mut Vec<u8>, data: &[u8]) -> String {
    let mut bytes = std::mem::take(pending);
    bytes.extend_from_slice(data);
    let mut text = String::with_capacity(bytes.len());
    let mut rest = bytes.as_slice();
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                return text;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                text.push_str(std::str::from_utf8(valid).expect("validated prefix"));
                match error.error_len() {
                    Some(length) => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        rest = &after[length..];
                    }
                    None => {
                        *pending = after.to_vec();
                        return text;
                    }
                }
            }
        }
    }
}

fn strip_csi(body: &str, last: char) -> bool {
    let all = |text: &str, allowed: &str| text.chars().all(|c| allowed.contains(c));
    match last {
        'n' => true,
        'R' => all(body, "0123456789;?"),
        'c' => all(body, ">0123456789;?"),
        // DECRQM queries and DECRPM replies; DECSTR (!p) and DECSCL ("p) stay.
        'p' | 'y' => body
            .strip_suffix('$')
            .is_some_and(|rest| all(rest, "0123456789;?")),
        // XTVERSION; DECSCUSR ( q) stays.
        'q' => body
            .strip_prefix('>')
            .is_some_and(|rest| all(rest, "0123456789;")),
        // Kitty keyboard query and reply; restore-cursor (bare u) stays.
        'u' => body.starts_with('?'),
        _ => false,
    }
}

/// DECRQSS and XTGETTCAP queries and their replies.
fn strip_dcs(content: &str) -> bool {
    let rest = content
        .strip_prefix(['0', '1'])
        .unwrap_or(content)
        .as_bytes();
    matches!(rest, [b'$' | b'+', b'q' | b'r', ..])
}

/// Foreground, background and cursor color queries and replies.
fn strip_osc(content: &str) -> bool {
    ["10;", "11;", "12;"].iter().any(|prefix| {
        content
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('?') || rest.starts_with("rgb:"))
    })
}

fn without_terminator(value: &str) -> &str {
    value
        .strip_suffix("\x1b\\")
        .or_else(|| value.strip_suffix(['\x07', '\u{9c}']))
        .unwrap_or(value)
}

/// The end of a string sequence whose content starts at `start`.
fn string_end(input: &str, start: usize) -> Option<usize> {
    let mut chars = input[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        match c {
            '\x07' | '\u{9c}' => return Some(start + offset + c.len_utf8()),
            '\x1b' if chars.peek().is_some_and(|(_, next)| *next == '\\') => {
                return Some(start + offset + 2);
            }
            _ => {}
        }
    }
    None
}

/// The end of the CSI sequence whose parameters start at `start`, and its
/// final byte.
fn csi_end(input: &str, start: usize) -> Option<(usize, char)> {
    input[start..]
        .char_indices()
        .find(|(_, c)| ('\x40'..='\x7e').contains(c))
        .map(|(offset, c)| (start + offset, c))
}

/// The end of an escape sequence whose first byte after ESC is at `start`.
fn escape_end(input: &str, start: usize) -> Option<usize> {
    let rest = &input[start..];
    let intermediates = rest
        .bytes()
        .take_while(|byte| (0x20..=0x2f).contains(byte))
        .count();
    let next = rest[intermediates..].chars().next()?;
    Some(if ('\x30'..='\x7e').contains(&next) {
        start + intermediates + 1
    } else {
        start + rest.chars().next().map_or(1, char::len_utf8)
    })
}

/// Output without terminal queries and replies, and the unfinished sequence the
/// next chunk continues.
fn sanitize(pending: &str, data: &str) -> (String, String) {
    let input = format!("{pending}{data}");
    let mut visible = String::with_capacity(input.len());
    let mut index = 0;
    let unfinished = |visible: String, index: usize| (visible, input[index..].to_owned());
    while let Some(c) = input[index..].chars().next() {
        let introducer = c.len_utf8();
        let (csi, string) = match c {
            '\x1b' => match input[index + 1..].chars().next() {
                None => return unfinished(visible, index),
                Some('[') => (Some(index + 2), None),
                Some(kind @ (']' | 'P' | '^' | '_')) => (None, Some((kind, index + 2))),
                Some(_) => {
                    let Some(end) = escape_end(&input, index + 1) else {
                        return unfinished(visible, index);
                    };
                    visible.push_str(&input[index..end]);
                    index = end;
                    continue;
                }
            },
            '\u{9b}' => (Some(index + introducer), None),
            '\u{9d}' => (None, Some((']', index + introducer))),
            '\u{90}' => (None, Some(('P', index + introducer))),
            '\u{9e}' | '\u{9f}' => (None, Some(('^', index + introducer))),
            _ => {
                visible.push(c);
                index += introducer;
                continue;
            }
        };
        if let Some(start) = csi {
            let Some((last, final_byte)) = csi_end(&input, start) else {
                return unfinished(visible, index);
            };
            if !strip_csi(&input[start..last], final_byte) {
                visible.push_str(&input[index..=last]);
            }
            index = last + 1;
        } else if let Some((kind, start)) = string {
            let Some(end) = string_end(&input, start) else {
                return unfinished(visible, index);
            };
            let content = without_terminator(&input[start..end]);
            let strip = match kind {
                ']' => strip_osc(content),
                'P' => strip_dcs(content),
                _ => false,
            };
            if !strip {
                visible.push_str(&input[index..end]);
            }
            index = end;
        }
    }
    (visible, String::new())
}

/// The history files, one per thread terminal, named from both IDs.
#[derive(Clone)]
pub(crate) struct HistoryFiles {
    directory: PathBuf,
    limits: Limits,
}
impl HistoryFiles {
    pub(crate) fn new(directory: PathBuf, limits: Limits) -> Self {
        Self { directory, limits }
    }
    fn encoded(text: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text)
    }
    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }
    pub(crate) fn path(&self, thread: &ThreadId, terminal_id: &str) -> PathBuf {
        self.directory.join(format!(
            "{}.{}.log",
            Self::encoded(thread.as_str()),
            Self::encoded(terminal_id)
        ))
    }

    /// The kept history, read from at most the byte limit at the file's end
    /// and starting on a whole character. A file over the limits is rewritten
    /// to what is kept.
    pub(crate) async fn read(
        &self,
        thread: &ThreadId,
        terminal_id: &str,
    ) -> Result<History, String> {
        let (path, limits) = (self.path(thread, terminal_id), self.limits);
        let failed = |operation: &str| {
            format!(
                "Failed to {operation} terminal history for thread: {thread}, terminal: {terminal_id}"
            )
        };
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek};
            let mut file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(History::new(limits, ""));
                }
                Err(_) => return Err("read"),
            };
            let size = file.metadata().map_err(|_| "read")?.len();
            let offset = size.saturating_sub(limits.bytes as u64);
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|_| "read")?;
            let mut bytes = vec![];
            file.read_to_end(&mut bytes).map_err(|_| "read")?;
            let start = if offset > 0 {
                bytes
                    .iter()
                    .position(|byte| byte & 0xc0 != 0x80)
                    .unwrap_or(bytes.len())
            } else {
                0
            };
            let raw = String::from_utf8_lossy(&bytes[start..]);
            let history = History::new(limits, &raw);
            let kept = history.value();
            if offset > 0 || kept != raw {
                crate::platform::save_private_bytes(&path, kept.as_bytes())
                    .map_err(|_| "truncate")?;
            }
            Ok(history)
        })
        .await
        .map_err(|_| failed("read"))?
        .map_err(failed)
    }

    pub(crate) async fn write(path: PathBuf, text: String) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = crate::platform::save_private_bytes(&path, text.as_bytes()) {
                tracing::warn!(operation = "host.terminal.history", message = %error);
            }
        })
        .await;
    }

    pub(crate) async fn delete(path: PathBuf) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(operation = "host.terminal.history", message = %error);
            }
        })
        .await;
    }

    /// Deletes the history of every terminal the thread had.
    pub(crate) async fn delete_thread(&self, thread: &ThreadId) {
        let (directory, prefix) = (
            self.directory.clone(),
            format!("{}.", Self::encoded(thread.as_str())),
        );
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                return;
            };
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix)
                    && let Err(error) = std::fs::remove_file(entry.path())
                {
                    tracing::warn!(operation = "host.terminal.history", message = %error);
                }
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests;
