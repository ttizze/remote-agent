//! T3 threadSort.ts: fractional pinned keys and hidden-row reservations.
use std::collections::{BTreeMap, BTreeSet};
fn valid(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase()) && !key.ends_with('a')
}
fn between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    let mut a = before.unwrap_or("").as_bytes();
    let mut b = after.unwrap_or("").as_bytes();
    if before.is_some_and(|k| !valid(k))
        || after.is_some_and(|k| !valid(k))
        || (!b.is_empty() && a >= b)
    {
        return None;
    }
    let mut key = Vec::new();
    loop {
        if !b.is_empty() {
            let n = b
                .iter()
                .enumerate()
                .take_while(|(i, digit)| a.get(*i).copied().unwrap_or(b'a') == **digit)
                .count();
            if n > 0 {
                key.extend_from_slice(&b[..n]);
                a = &a[n.min(a.len())..];
                b = &b[n..];
                continue;
            }
        }
        let digit_a = a.first().copied().unwrap_or(b'a') - b'a';
        let digit_b = b.first().map_or(26, |c| c - b'a');
        if digit_b - digit_a > 1 {
            key.push(b'a' + (digit_a + digit_b).div_ceil(2));
            break;
        }
        if b.len() > 1 {
            key.push(b[0]);
            break;
        }
        key.push(b'a' + digit_a);
        a = &a[1.min(a.len())..];
        b = &[];
    }
    String::from_utf8(key).ok()
}
fn spread(count: usize) -> Vec<String> {
    let mut width = 2;
    let mut space = 26_u128.pow(width);
    while space <= ((count as u128) + 1) * 2 {
        width += 1;
        space *= 26;
    }
    (0..count)
        .map(|index| {
            let divisor = (count as u128) + 1;
            let mut value = (space * ((index as u128) + 1) + divisor / 2) / divisor;
            if value.is_multiple_of(26) {
                value += 1;
            }
            let mut key = vec![b'a'; width as usize];
            for digit in key.iter_mut().rev() {
                *digit = b'a' + (value % 26) as u8;
                value /= 26;
            }
            String::from_utf8(key).expect("lowercase digits")
        })
        .collect()
}
pub(crate) fn reorder(
    ordered: &[String],
    keys: &BTreeMap<String, Option<String>>,
    moved: &str,
) -> Vec<(String, String)> {
    let Some(index) = ordered.iter().position(|id| id == moved) else {
        return vec![];
    };
    let visible = ordered.iter().collect::<BTreeSet<_>>();
    let reserved = keys
        .iter()
        .filter(|(id, _)| !visible.contains(id))
        .filter_map(|(_, key)| key.as_deref())
        .collect::<BTreeSet<_>>();
    let before = index.checked_sub(1).and_then(|i| ordered.get(i));
    let after = ordered.get(index + 1);
    let before_key = before
        .and_then(|id| keys.get(id))
        .and_then(Option::as_deref);
    let after_key = after.and_then(|id| keys.get(id)).and_then(Option::as_deref);
    if (before.is_none() || before_key.is_some()) && (after.is_none() || after_key.is_some()) {
        let mut key = between(before_key, after_key);
        while key
            .as_ref()
            .is_some_and(|key| reserved.contains(key.as_str()))
        {
            key = between(key.as_deref(), after_key);
        }
        if let Some(key) = key {
            return vec![(moved.into(), key)];
        }
    }
    let generated = spread(ordered.len() + reserved.len())
        .into_iter()
        .filter(|key| !reserved.contains(key.as_str()));
    ordered
        .iter()
        .zip(generated)
        .filter(|(id, key)| keys.get(*id).and_then(Option::as_ref) != Some(key))
        .map(|(id, key)| (id.clone(), key))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_keys_are_reserved_and_keyless_neighbors_get_materialized() {
        let order = vec!["c".into(), "a".into(), "b".into()];
        let keys = BTreeMap::from([
            ("a".into(), None),
            ("b".into(), None),
            ("c".into(), None),
            ("hidden".into(), Some("n".into())),
        ]);
        let writes = reorder(&order, &keys, "c");
        assert_eq!(writes.len(), 3);
        assert!(writes.windows(2).all(|w| w[0].1 < w[1].1));
        assert!(writes.iter().all(|(_, key)| key != "n"));
        let keys = keys
            .into_iter()
            .chain(writes.into_iter().map(|(id, key)| (id, Some(key))))
            .collect();
        assert_eq!(
            reorder(&["a".into(), "c".into(), "b".into()], &keys, "c").len(),
            1
        );
    }
    proptest::proptest! {
        #[test]
        fn each_move_preserves_a_strict_order(count in 2usize..100,from in 0usize..100,to in 0usize..100) {
            let mut order=(0..count).map(|i|i.to_string()).collect::<Vec<_>>();
            let keys=order.iter().cloned().zip(spread(count).into_iter().map(Some)).collect::<BTreeMap<_,_>>();
            let moved=order.remove(from%count);order.insert(to%count,moved.clone());
            let mut keys=keys;for (id,key) in reorder(&order,&keys,&moved){keys.insert(id,Some(key));}
            let ordered_keys=order.iter().map(|id|keys[id].as_deref().unwrap()).collect::<Vec<_>>();
            proptest::prop_assert!(ordered_keys.windows(2).all(|w|w[0]<w[1]));
            proptest::prop_assert!(ordered_keys.into_iter().all(valid));
        }
    }
}
