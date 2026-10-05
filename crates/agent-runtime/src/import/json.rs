//! One JSONL record read incrementally. Only selected fields are assembled; other
//! values are validated and skipped without allocation (T3 `AgentSessionJson`).
use serde_json::{Map, Value};

pub(crate) const MAX_DEPTH: usize = 128;
const CHARGE_STEP: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seg {
    Key(String),
    Index(usize),
}

/// The transcript's allocation or nesting budget ran out; the whole transcript is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LimitExceeded;

pub(crate) type Reserve<'a> = dyn FnMut(usize) -> Result<(), LimitExceeded> + 'a;

enum Fault {
    Malformed,
    Limit,
}
impl From<LimitExceeded> for Fault {
    fn from(_: LimitExceeded) -> Self {
        Self::Limit
    }
}

enum Container {
    Object(Map<String, Value>),
    Array(Vec<Value>),
    Skipped,
}
struct Frame {
    container: Container,
    object: bool,
    key: String,
    index: usize,
    has_seg: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Value,
    FirstValueOrEnd,
    FirstKeyOrEnd,
    Key,
    Colon,
    CommaOrEnd,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Num {
    Minus,
    Zero,
    Int,
    Dot,
    Frac,
    E,
    ESign,
    Exp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    Slash,
    Hex { digits: u8, code: u32 },
}

enum Token {
    None,
    Str {
        key: bool,
        keep: bool,
        escape: Escape,
    },
    Num {
        state: Num,
        keep: bool,
    },
    Lit {
        word: &'static [u8],
        pos: usize,
        keep: bool,
    },
}

pub(crate) struct RecordReader {
    select: fn(&[Seg]) -> bool,
    path: Vec<Seg>,
    frames: Vec<Frame>,
    expect: Expect,
    token: Token,
    text: Vec<u8>,
    charged: usize,
    high: Option<u32>,
    root: Option<Value>,
    malformed: bool,
}

impl RecordReader {
    pub(crate) fn new(select: fn(&[Seg]) -> bool) -> Self {
        Self {
            select,
            path: Vec::new(),
            frames: Vec::new(),
            expect: Expect::Value,
            token: Token::None,
            text: Vec::new(),
            charged: 0,
            high: None,
            root: None,
            malformed: false,
        }
    }

    pub(crate) fn write(
        &mut self,
        bytes: &[u8],
        reserve: &mut Reserve<'_>,
    ) -> Result<(), LimitExceeded> {
        for &byte in bytes {
            if self.malformed {
                return Ok(());
            }
            match self.byte(byte, reserve) {
                Ok(()) => {}
                Err(Fault::Malformed) => self.malformed = true,
                Err(Fault::Limit) => return Err(LimitExceeded),
            }
        }
        Ok(())
    }

    /// The selected projection of a complete object or array record, if well formed.
    pub(crate) fn finish(
        mut self,
        reserve: &mut Reserve<'_>,
    ) -> Result<Option<Value>, LimitExceeded> {
        if !self.malformed {
            let ended = match self.token {
                Token::None => Ok(()),
                Token::Num { state, keep } => self.end_number(state, keep, reserve),
                Token::Str { .. } | Token::Lit { .. } => Err(Fault::Malformed),
            };
            match ended {
                Ok(()) => {}
                Err(Fault::Malformed) => self.malformed = true,
                Err(Fault::Limit) => return Err(LimitExceeded),
            }
        }
        Ok(if self.malformed || self.expect != Expect::Done {
            None
        } else {
            self.root
        })
    }

    fn byte(&mut self, byte: u8, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        match self.token {
            Token::None => self.structural(byte, reserve),
            Token::Str { key, keep, escape } => self.string_byte(byte, key, keep, escape, reserve),
            Token::Num { state, keep } => match next_number(state, byte) {
                Some(next) => {
                    self.token = Token::Num { state: next, keep };
                    self.push_bytes(keep, &[byte], reserve)
                }
                None => {
                    self.end_number(state, keep, reserve)?;
                    self.structural(byte, reserve)
                }
            },
            Token::Lit { word, pos, keep } => {
                if word[pos] != byte {
                    return Err(Fault::Malformed);
                }
                if pos + 1 < word.len() {
                    self.token = Token::Lit {
                        word,
                        pos: pos + 1,
                        keep,
                    };
                    return Ok(());
                }
                self.token = Token::None;
                let value = keep.then_some(match word {
                    b"true" => Value::Bool(true),
                    b"false" => Value::Bool(false),
                    _ => Value::Null,
                });
                if keep {
                    reserve(64)?;
                }
                self.complete(value)
            }
        }
    }

    fn structural(&mut self, byte: u8, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            return Ok(());
        }
        let object = self.frames.last().is_some_and(|frame| frame.object);
        match (self.expect, byte) {
            (Expect::Done, _) => Err(Fault::Malformed),
            (Expect::Colon, b':') => {
                self.expect = Expect::Value;
                Ok(())
            }
            (Expect::FirstKeyOrEnd, b'}') | (Expect::FirstValueOrEnd, b']') => self.close(reserve),
            (Expect::FirstKeyOrEnd | Expect::Key, b'"') => {
                self.begin_string(true, true);
                Ok(())
            }
            (Expect::CommaOrEnd, b',') => {
                self.expect = if object { Expect::Key } else { Expect::Value };
                Ok(())
            }
            (Expect::CommaOrEnd, b'}') if object => self.close(reserve),
            (Expect::CommaOrEnd, b']') if !object => self.close(reserve),
            (Expect::Value | Expect::FirstValueOrEnd, _) => self.begin_value(byte, reserve),
            _ => Err(Fault::Malformed),
        }
    }

    fn begin_value(&mut self, byte: u8, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        match byte {
            b'{' | b'[' => {
                if self.frames.len() >= MAX_DEPTH {
                    return Err(Fault::Limit);
                }
                let (keep, seg) = self.child(reserve)?;
                let object = byte == b'{';
                let has_seg = keep && seg.is_some();
                if let Some(seg) = seg.filter(|_| keep) {
                    self.path.push(seg);
                }
                if keep {
                    reserve(64)?;
                }
                self.frames.push(Frame {
                    container: match (keep, object) {
                        (false, _) => Container::Skipped,
                        (true, true) => Container::Object(Map::new()),
                        (true, false) => Container::Array(Vec::new()),
                    },
                    object,
                    key: String::new(),
                    index: 0,
                    has_seg,
                });
                self.expect = if object {
                    Expect::FirstKeyOrEnd
                } else {
                    Expect::FirstValueOrEnd
                };
                Ok(())
            }
            b'"' => {
                let (keep, _) = self.child(reserve)?;
                self.begin_string(false, keep);
                Ok(())
            }
            b'-' | b'0'..=b'9' => {
                let (keep, _) = self.child(reserve)?;
                self.text.clear();
                self.charged = 0;
                self.high = None;
                self.push_bytes(keep, &[byte], reserve)?;
                let state = match byte {
                    b'-' => Num::Minus,
                    b'0' => Num::Zero,
                    _ => Num::Int,
                };
                self.token = Token::Num { state, keep };
                Ok(())
            }
            b't' | b'f' | b'n' => {
                let (keep, _) = self.child(reserve)?;
                let word: &'static [u8] = match byte {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => b"null",
                };
                self.token = Token::Lit { word, pos: 1, keep };
                Ok(())
            }
            _ => Err(Fault::Malformed),
        }
    }

    /// Whether the value starting now is selected, and its path segment.
    fn child(&mut self, reserve: &mut Reserve<'_>) -> Result<(bool, Option<Seg>), Fault> {
        let Some(parent) = self.frames.last() else {
            return Ok(((self.select)(&[]), None));
        };
        if matches!(parent.container, Container::Skipped) {
            return Ok((false, None));
        }
        let seg = if parent.object {
            Seg::Key(parent.key.clone())
        } else {
            Seg::Index(parent.index)
        };
        self.path.push(seg);
        let keep = (self.select)(&self.path);
        let seg = self.path.pop();
        if keep && parent.object {
            reserve(64 + 2 * parent.key.len())?;
        }
        Ok((keep, seg))
    }

    fn begin_string(&mut self, key: bool, keep: bool) {
        self.text.clear();
        self.charged = 0;
        self.high = None;
        self.token = Token::Str {
            key,
            keep,
            escape: Escape::None,
        };
    }

    fn string_byte(
        &mut self,
        byte: u8,
        key: bool,
        keep: bool,
        escape: Escape,
        reserve: &mut Reserve<'_>,
    ) -> Result<(), Fault> {
        let mut next = Escape::None;
        match escape {
            Escape::None => match byte {
                b'"' => return self.end_string(key, keep, reserve),
                b'\\' => next = Escape::Slash,
                0..=0x1f => return Err(Fault::Malformed),
                _ => self.push_bytes(keep, &[byte], reserve)?,
            },
            Escape::Slash => {
                let decoded = match byte {
                    b'"' | b'\\' | b'/' => byte,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'u' => {
                        next = Escape::Hex { digits: 0, code: 0 };
                        0
                    }
                    _ => return Err(Fault::Malformed),
                };
                if next == Escape::None {
                    self.push_bytes(keep, &[decoded], reserve)?;
                }
            }
            Escape::Hex { digits, code } => {
                let digit = (byte as char).to_digit(16).ok_or(Fault::Malformed)?;
                let code = code * 16 + digit;
                if digits < 3 {
                    next = Escape::Hex {
                        digits: digits + 1,
                        code,
                    };
                } else if keep {
                    self.push_code(code, reserve)?;
                }
            }
        }
        self.token = Token::Str {
            key,
            keep,
            escape: next,
        };
        Ok(())
    }

    fn push_code(&mut self, code: u32, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        match code {
            0xD800..=0xDBFF => {
                self.flush_high(reserve)?;
                self.high = Some(code);
                Ok(())
            }
            0xDC00..=0xDFFF => {
                let char = match self.high.take() {
                    Some(high) => {
                        char::from_u32(0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00))
                    }
                    None => None,
                };
                self.push_char(char.unwrap_or(char::REPLACEMENT_CHARACTER), reserve)
            }
            _ => self.push_char(
                char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER),
                reserve,
            ),
        }
    }

    fn push_char(&mut self, char: char, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        let mut buffer = [0; 4];
        self.push_bytes(true, char.encode_utf8(&mut buffer).as_bytes(), reserve)
    }

    fn flush_high(&mut self, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        if self.high.take().is_some() {
            self.push_char(char::REPLACEMENT_CHARACTER, reserve)?;
        }
        Ok(())
    }

    fn push_bytes(
        &mut self,
        keep: bool,
        bytes: &[u8],
        reserve: &mut Reserve<'_>,
    ) -> Result<(), Fault> {
        if !keep {
            return Ok(());
        }
        self.flush_high(reserve)?;
        self.text.extend_from_slice(bytes);
        if self.text.len() - self.charged >= CHARGE_STEP {
            reserve(2 * (self.text.len() - self.charged))?;
            self.charged = self.text.len();
        }
        Ok(())
    }

    fn take_text(&mut self, reserve: &mut Reserve<'_>, base: usize) -> Result<String, Fault> {
        self.flush_high(reserve)?;
        reserve(base + 2 * (self.text.len() - self.charged))?;
        self.charged = 0;
        let text = String::from_utf8_lossy(&self.text).into_owned();
        self.text.clear();
        Ok(text)
    }

    fn end_string(
        &mut self,
        key: bool,
        keep: bool,
        reserve: &mut Reserve<'_>,
    ) -> Result<(), Fault> {
        self.token = Token::None;
        if key {
            let text = self.take_text(reserve, 0)?;
            if let Some(frame) = self.frames.last_mut() {
                frame.key = text;
            }
            self.expect = Expect::Colon;
            return Ok(());
        }
        let value = if keep {
            Some(Value::String(self.take_text(reserve, 192)?))
        } else {
            None
        };
        self.complete(value)
    }

    fn end_number(
        &mut self,
        state: Num,
        keep: bool,
        reserve: &mut Reserve<'_>,
    ) -> Result<(), Fault> {
        if !matches!(state, Num::Zero | Num::Int | Num::Frac | Num::Exp) {
            return Err(Fault::Malformed);
        }
        self.token = Token::None;
        let value = if keep {
            reserve(192 + 2 * (self.text.len() - self.charged))?;
            self.charged = 0;
            let parsed =
                serde_json::from_slice::<Value>(&self.text).map_err(|_| Fault::Malformed)?;
            self.text.clear();
            Some(parsed)
        } else {
            None
        };
        self.complete(value)
    }

    fn complete(&mut self, value: Option<Value>) -> Result<(), Fault> {
        let Some(frame) = self.frames.last_mut() else {
            self.expect = Expect::Done;
            return Ok(());
        };
        if let Some(value) = value {
            match &mut frame.container {
                Container::Object(map) => {
                    map.insert(std::mem::take(&mut frame.key), value);
                }
                Container::Array(items) => items.push(value),
                Container::Skipped => {}
            }
        }
        if !frame.object {
            frame.index += 1;
        }
        self.expect = Expect::CommaOrEnd;
        Ok(())
    }

    fn close(&mut self, reserve: &mut Reserve<'_>) -> Result<(), Fault> {
        let frame = self.frames.pop().ok_or(Fault::Malformed)?;
        if frame.has_seg {
            self.path.pop();
        }
        let value = match frame.container {
            Container::Object(map) => Some(Value::Object(map)),
            Container::Array(items) => Some(Value::Array(items)),
            Container::Skipped => None,
        };
        if value.is_some() {
            reserve(64)?;
        }
        if self.frames.is_empty() {
            self.root = value;
            self.expect = Expect::Done;
            return Ok(());
        }
        self.complete(value)
    }
}

fn next_number(state: Num, byte: u8) -> Option<Num> {
    let digit = byte.is_ascii_digit();
    let exponent = byte == b'e' || byte == b'E';
    match state {
        Num::Minus if byte == b'0' => Some(Num::Zero),
        Num::Minus if digit => Some(Num::Int),
        Num::Zero | Num::Int if byte == b'.' => Some(Num::Dot),
        Num::Zero | Num::Int | Num::Frac if exponent => Some(Num::E),
        Num::Int if digit => Some(Num::Int),
        Num::Dot | Num::Frac if digit => Some(Num::Frac),
        Num::E if byte == b'+' || byte == b'-' => Some(Num::ESign),
        Num::E | Num::ESign | Num::Exp if digit => Some(Num::Exp),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
