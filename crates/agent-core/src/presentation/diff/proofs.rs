use super::quoted_path;

#[kani::proof]
#[kani::unwind(5)]
fn bounded_quoted_paths_are_safe() {
    let bytes: [u8; 4] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= bytes.len());
    if let Ok(path) = std::str::from_utf8(&bytes[..len]) {
        let result = quoted_path(path);
        kani::cover!(result.is_some(), "a quoted path can be decoded");
        kani::cover!(result.is_none(), "malformed paths can be rejected");
        if let Some((decoded, rest)) = result {
            assert!(rest.len() + 2 <= path.len());
            assert!(decoded.len() <= path.len() - rest.len() - 2);
            assert_eq!(rest, &path[path.len() - rest.len()..]);
        }
    }
}

#[kani::proof]
#[kani::unwind(7)]
fn octal_escape_matches_one_byte() {
    let value: u16 = kani::any();
    kani::assume(value < 512);
    let bytes = [
        b'"',
        b'\\',
        b'0' + (value / 64) as u8,
        b'0' + ((value / 8) % 8) as u8,
        b'0' + (value % 8) as u8,
        b'"',
    ];
    let result = quoted_path(std::str::from_utf8(&bytes).unwrap());
    if value < 128 {
        let (decoded, rest) = result.unwrap();
        assert_eq!(decoded.as_bytes(), &[value as u8]);
        assert!(rest.is_empty());
    } else {
        // 128..255 is not valid UTF-8 alone; 256..511 exceeds one byte.
        assert!(result.is_none());
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn named_escapes_decode_and_preserve_suffix() {
    let index: u8 = kani::any();
    let cases = [
        ("\"\\a\"\tx", 7),
        ("\"\\b\"\tx", 8),
        ("\"\\t\"\tx", 9),
        ("\"\\n\"\tx", 10),
        ("\"\\v\"\tx", 11),
        ("\"\\f\"\tx", 12),
        ("\"\\r\"\tx", 13),
        ("\"\\\\\"\tx", b'\\'),
        ("\"\\\"\"\tx", b'"'),
    ];
    kani::assume(usize::from(index) < cases.len());
    let (path, expected) = cases[usize::from(index)];
    let (decoded, rest) = quoted_path(path).unwrap();
    assert_eq!(decoded.as_bytes(), &[expected]);
    assert_eq!(rest, "\tx");
}
