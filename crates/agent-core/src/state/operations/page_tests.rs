use super::page_rows;
use crate::models::Thread;
use proptest::prelude::*;

fn assert_page(timestamps: Vec<Option<i64>>, limit: u32) {
    let rows: Vec<_> = timestamps
        .into_iter()
        .enumerate()
        .map(|(index, last_input_at)| Thread {
            name: Some(index.to_string()),
            preview: Some(format!("Preview {index}")),
            cwd: Some(format!("/work/{index}")),
            last_input_at,
            updated_at: Some(-(index as f64)),
            ..Default::default()
        })
        .collect();
    let (page, more) = page_rows(rows.clone(), limit);
    assert_eq!(page.len(), rows.len().min(limit as usize));
    assert_eq!(more, rows.len() > limit as usize);
    let compare = |a: usize, b: usize| {
        rows[b]
            .last_input_at
            .cmp(&rows[a].last_input_at)
            .then_with(|| a.cmp(&b))
    };
    let mut included = vec![false; rows.len()];
    let mut last = None;
    for row in page {
        let index = row.name.as_ref().unwrap().parse::<usize>().unwrap();
        assert!(!included[index], "a source row can only appear once");
        included[index] = true;
        assert_eq!(row, rows[index]);
        if let Some(previous) = last {
            assert!(compare(previous, index).is_lt());
        }
        last = Some(index);
    }
    if let Some(last) = last {
        for (index, included) in included.into_iter().enumerate() {
            if !included {
                assert!(compare(last, index).is_lt(), "a newer row was omitted");
            }
        }
    }
}

#[test]
fn page_limits_preserve_human_input_order_ties_and_all_row_metadata() {
    let values = [
        None,
        Some(i64::MIN),
        Some(-1),
        Some(0),
        Some(1),
        Some(i64::MAX),
    ];
    for count in [0usize, 1, 16, 127, 128, 129, 512] {
        for limit in [0, 1, 25, 31, 32, 33, 127, 128, 129, 512, 513] {
            assert_page(
                (0..count)
                    .map(|index| values[index % values.len()])
                    .collect(),
                limit,
            );
            assert_page(vec![Some(1); count], limit);
            assert_page((0..count).map(|index| Some(index as i64)).collect(), limit);
            assert_page(
                (0..count).rev().map(|index| Some(index as i64)).collect(),
                limit,
            );
        }
    }
}

proptest! {
    #[test]
    fn pages_keep_the_latest_human_input_prefix(
        timestamps in prop::collection::vec(prop::option::of(any::<i64>()), 0..1024),
        limit in prop_oneof![Just(0u32), 0u32..32, 32u32..1100],
    ) {
        assert_page(timestamps, limit);
    }
}
