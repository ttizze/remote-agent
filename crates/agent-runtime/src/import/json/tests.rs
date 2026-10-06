use super::*;

fn everything(_: &[Seg]) -> bool {
    true
}

fn read_with(
    json: &str,
    size: usize,
    select: fn(&[Seg]) -> bool,
) -> Result<Option<Value>, LimitExceeded> {
    let mut reader = RecordReader::new(select);
    let mut reserve = |_| Ok(());
    for chunk in json.as_bytes().chunks(size) {
        reader.write(chunk, &mut reserve)?;
    }
    reader.finish(&mut reserve)
}

fn read(json: &str, size: usize) -> Option<Value> {
    read_with(json, size, everything).unwrap()
}

#[test]
fn matches_json_parse_across_chunk_boundaries() {
    for size in [1, 2, 7, 64, 1024] {
        for text in [
            r#"{"a":1,"a":2,"b":"before","b":"after"}"#,
            r#"{"a":{"x":1},"a":{"y":2},"b":[],"c":{}}"#,
            r#"{"a":[null,true,false,1,-2.3e4,"😀a\\\"",{},[],[1,2]]}"#,
            r#"{"a":"s","a":null,"b":null,"b":"s","c":0,"c":false}"#,
            r#"{"__proto__":{"polluted":true},"constructor":1,"__proto__":2}"#,
        ] {
            assert_eq!(
                read(text, size),
                Some(serde_json::from_str::<Value>(text).unwrap()),
                "{text} at {size}"
            );
        }
    }
}

#[test]
fn projects_siblings_and_array_elements_without_merging_repeated_parents() {
    fn select(path: &[Seg]) -> bool {
        let drop = Seg::Key("drop".into());
        let content = Seg::Key("content".into());
        path.first() != Some(&drop) && !path.contains(&content) && !path.contains(&drop)
    }
    let text = r#"{"message":{"usage":{"input":100},"content":"large"},"message":{"usage":{"output":5},"content":[1,2]},"rows":[{"keep":1,"drop":2},{"keep":3}],"drop":{"keep":4}}"#;
    assert_eq!(
        read_with(text, 1, select).unwrap(),
        Some(serde_json::json!({
            "message": { "usage": { "output": 5 } },
            "rows": [{ "keep": 1 }, { "keep": 3 }],
        }))
    );
}

#[test]
fn rejects_malformed_input() {
    for text in [
        r#"{"a":"#,
        r#"{"a":1} trailing"#,
        r#"{"a":1}{"a":2}"#,
        r#"{"a":"bad\x"}"#,
        r#"{"a":[1,]}"#,
    ] {
        assert_eq!(read(text, 1), None, "{text}");
    }
}

#[test]
fn retains_the_import_allocation_and_depth_limits() {
    let mut reader = RecordReader::new(everything);
    let mut exhausted = |_| Err(LimitExceeded);
    assert_eq!(
        reader.write(br#"{"a":1}"#, &mut exhausted),
        Err(LimitExceeded)
    );
    let nested = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    assert_eq!(read_with(&nested, 10, everything), Err(LimitExceeded));
}

#[test]
fn charges_selected_numbers_while_they_are_read() {
    const BUDGET: usize = 1024 * 1024;
    let mut reader = RecordReader::new(everything);
    let mut used = 0;
    let mut reserve = |bytes: usize| {
        used += bytes;
        if used > BUDGET {
            Err(LimitExceeded)
        } else {
            Ok(())
        }
    };
    reader.write(br#"{"a":1"#, &mut reserve).unwrap();
    let digits = [b'7'; 4096];
    let mut written = 0;
    let outcome = loop {
        if let Err(limit) = reader.write(&digits, &mut reserve) {
            break Err(limit);
        }
        written += digits.len();
        if written > 4 * BUDGET {
            break Ok(());
        }
    };
    assert_eq!(outcome, Err(LimitExceeded));
    assert!(reader.text.len() <= BUDGET / 2 + CHARGE_STEP);
}

#[test]
fn charges_a_long_number_in_full_exactly_once() {
    let digits = "9".repeat(3 * CHARGE_STEP + 17);
    let text = format!(r#"{{"a":0.{digits}}}"#);
    let mut reader = RecordReader::new(everything);
    let mut used = 0;
    let mut reserve = |bytes: usize| {
        used += bytes;
        Ok(())
    };
    reader.write(text.as_bytes(), &mut reserve).unwrap();
    let value = reader.finish(&mut reserve).unwrap();
    assert_eq!(value, Some(serde_json::from_str::<Value>(&text).unwrap()));
    let number = digits.len() + 2;
    assert!(
        (2 * number..2 * number + 1024).contains(&used),
        "{used} for {number}"
    );
}
