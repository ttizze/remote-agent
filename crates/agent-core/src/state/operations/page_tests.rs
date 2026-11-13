use super::page_rows;
use crate::models::Thread;
use proptest::prelude::*;

fn assert_page(timestamps: Vec<Option<f64>>, limit: u32) {
    let rows: Vec<_> = timestamps
        .into_iter()
        .enumerate()
        .map(|(index, updated_at)| Thread {
            name: Some(index.to_string()),
            preview: Some(format!("Preview {index}")),
            cwd: Some(format!("/work/{index}")),
            updated_at,
            ..Default::default()
        })
        .collect();
    let (page, more) = page_rows(rows.clone(), limit);
    assert_eq!(page.len(), rows.len().min(limit as usize));
    assert_eq!(more, rows.len() > limit as usize);
    let compare = |a: usize, b: usize| {
        rows[b]
            .updated_at
            .unwrap_or_default()
            .total_cmp(&rows[a].updated_at.unwrap_or_default())
            .then_with(|| a.cmp(&b))
    };
    let mut included = vec![false; rows.len()];
    let mut last = None;
    for row in page {
        let index = row.name.as_ref().unwrap().parse::<usize>().unwrap();
        assert!(!included[index], "a source row can only appear once");
        included[index] = true;
        assert_eq!(
            row.updated_at.map(f64::to_bits),
            rows[index].updated_at.map(f64::to_bits)
        );
        // NaNs have no reflexive equality; verify their bits above and compare
        // all remaining row metadata independently.
        let mut actual = row;
        actual.updated_at = None;
        let mut expected = rows[index].clone();
        expected.updated_at = None;
        assert_eq!(actual, expected);
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
fn page_limits_preserve_timestamp_order_ties_and_all_row_metadata() {
    let values = [
        None,
        Some(-0.),
        Some(0.),
        Some(f64::INFINITY),
        Some(f64::NEG_INFINITY),
        Some(f64::from_bits(0x7ff8_0000_0000_0001)),
        Some(f64::from_bits(0xfff8_0000_0000_0001)),
        Some(1.),
    ];
    for count in [0usize, 1, 16, 127, 128, 129, 512] {
        for limit in [0, 1, 25, 31, 32, 33, 127, 128, 129, 512, 513] {
            assert_page(
                (0..count)
                    .map(|index| values[index % values.len()])
                    .collect(),
                limit,
            );
            assert_page(vec![Some(1.); count], limit);
            assert_page((0..count).map(|index| Some(index as f64)).collect(), limit);
            assert_page(
                (0..count).rev().map(|index| Some(index as f64)).collect(),
                limit,
            );
        }
    }
}

proptest! {
    #[test]
    fn pages_keep_the_newest_prefix_for_arbitrary_timestamp_bits(
        timestamps in prop::collection::vec(prop::option::of(any::<u64>()), 0..1024),
        limit in prop_oneof![Just(0u32), 0u32..32, 32u32..1100],
    ) {
        assert_page(timestamps.into_iter().map(|time| time.map(f64::from_bits)).collect(), limit);
    }
}
