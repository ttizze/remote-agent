// T3 AgentSessionJson.test.ts.
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
