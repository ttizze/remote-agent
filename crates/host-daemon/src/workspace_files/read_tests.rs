use super::*;

#[test]
fn text_at_the_limit_is_editable_and_larger_text_is_a_read_only_prefix() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("large.txt");
    let files = WorkspaceFiles::new(directory.path().join("uploads"));
    for length in [
        EDIT_LIMIT as usize - 1,
        EDIT_LIMIT as usize,
        EDIT_LIMIT as usize + 1,
    ] {
        let bytes = vec![b'x'; length];
        fs::write(&path, &bytes).unwrap();
        let content = read_file(&path, &path).unwrap();
        assert_eq!(content.truncated, length as u64 > EDIT_LIMIT);
        assert_eq!(content.size, length as u64);
        assert_eq!(content.text.len(), length.min(EDIT_LIMIT as usize));
        assert_eq!(content.text.as_bytes(), &bytes[..content.text.len()]);
        let result = files.dispatch(
            1,
            Call::WriteFile(agent_protocol::operations::WriteFile {
                path: path.to_str().unwrap().into(),
                revision: content.revision,
                text: "replacement".into(),
            }),
        );
        if length as u64 > EDIT_LIMIT {
            assert!(result.is_err());
            assert_eq!(
                fs::read(&path).unwrap(),
                bytes,
                "a preview must never replace the full file"
            );
        } else {
            assert!(result.is_ok());
            assert_eq!(fs::read(&path).unwrap(), b"replacement");
        }
    }
}

#[test]
fn preview_drops_only_a_utf8_character_split_by_the_byte_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unicode.txt");
    for character in ["é", "あ", "🦀"] {
        for remaining in 1..character.len() {
            let prefix = "a".repeat(EDIT_LIMIT as usize - remaining);
            fs::write(&path, format!("{prefix}{character}tail")).unwrap();
            let content = read_file(&path, &path).unwrap();
            assert!(content.truncated);
            assert_eq!(content.text, prefix);
            assert_eq!(content.size as usize, prefix.len() + character.len() + 4);
        }
    }
    for bytes in [vec![0xff], vec![b'a', 0], vec![0xe3, 0x81]] {
        fs::write(&path, bytes).unwrap();
        assert!(read_file(&path, &path).is_err());
    }
    let mut invalid_prefix = vec![b'x'; EDIT_LIMIT as usize + 1];
    invalid_prefix[100] = 0xff;
    fs::write(&path, invalid_prefix).unwrap();
    assert!(read_file(&path, &path).is_err());
}
