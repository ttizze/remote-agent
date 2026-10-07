//! Native transcript locations and the result prepared by the SDK worker.
/// The project directory name for a working directory, after the caller has
/// resolved its real path (NFC-normalized on macOS).
pub fn claude_project_key(dir: &str) -> String {
    let key = dir
        .encode_utf16()
        .map(|unit| {
            if unit < 128 && (unit as u8).is_ascii_alphanumeric() {
                unit as u8 as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    if key.len() <= 200 {
        return key;
    }
    let hash = dir
        .encode_utf16()
        .fold(0i32, |hash, unit| {
            hash.wrapping_shl(5)
                .wrapping_sub(hash)
                .wrapping_add(unit as i32)
        })
        .unsigned_abs();
    format!("{}-{}", &key[..200], radix36(hash as u64))
}
fn radix36(mut value: u64) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut digits = vec![];
    while value > 0 {
        digits.push(char::from_digit((value % 36) as u32, 36).unwrap());
        value /= 36;
    }
    digits.iter().rev().collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_keys_replace_non_alphanumerics_and_hash_long_paths() {
        assert_eq!(
            claude_project_key("/tmp/claude-replay"),
            "-tmp-claude-replay"
        );
        let long = format!("/{}", "a".repeat(250));
        let key = claude_project_key(&long);
        assert!(key.starts_with(&format!("-{}", "a".repeat(199))));
        assert!(key.len() > 201);
        assert_eq!(&key[200..201], "-");
    }
}
