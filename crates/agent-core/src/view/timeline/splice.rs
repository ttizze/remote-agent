//! The smallest replacement that turns one row list into another, matched by
//! row id, so a virtual list keeps its scroll anchor and measured rows.
use std::ops::Range;

/// Replace `range` of the old rows with `count` rows taken from the new list
/// at `range.start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub range: Range<usize>,
    pub count: usize,
}

impl Splice {
    pub fn is_empty(&self) -> bool {
        self.range.is_empty() && self.count == 0
    }
}

pub fn splice<T, I: PartialEq>(old: &[T], new: &[T], id: impl Fn(&T) -> &I) -> Splice {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| id(a) == id(b))
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| id(a) == id(b))
        .count();
    Splice {
        range: prefix..old.len() - suffix,
        count: new.len() - prefix - suffix,
    }
}

/// Indexes in the new list of rows kept by id whose content changed.
pub fn changed<T: PartialEq, I: PartialEq>(
    old: &[T],
    new: &[T],
    id: impl Fn(&T) -> &I,
) -> Vec<usize> {
    let Splice { range, count } = splice(old, new, &id);
    let kept = (0..range.start).chain(range.start + count..new.len());
    kept.filter(|&index| {
        let old_index = if index < range.start {
            index
        } else {
            index - count + range.len()
        };
        old[old_index] != new[index]
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ids(rows: &[(&'static str, u8)]) -> Splice {
        splice(rows, &[], |row| &row.0)
    }

    #[test]
    fn prepending_history_inserts_before_the_kept_rows() {
        let old = [("a", 0), ("b", 0)];
        let new = [("history", 0), ("older", 0), ("a", 0), ("b", 0)];
        assert_eq!(
            splice(&old, &new, |row| &row.0),
            Splice {
                range: 0..0,
                count: 2
            }
        );
    }

    #[test]
    fn a_streaming_row_is_kept_and_reported_as_changed() {
        let old = [("a", 0), ("b", 0)];
        let new = [("a", 0), ("b", 1)];
        assert!(splice(&old, &new, |row| &row.0).is_empty());
        assert_eq!(changed(&old, &new, |row| &row.0), [1]);
    }

    #[test]
    fn clearing_removes_every_row() {
        assert_eq!(
            ids(&[("a", 0), ("b", 0)]),
            Splice {
                range: 0..2,
                count: 0
            }
        );
    }

    proptest! {
        #[test]
        fn applying_the_splice_yields_the_new_ids(
            old in proptest::collection::vec(0u8..6, 0..12),
            new in proptest::collection::vec(0u8..6, 0..12),
        ) {
            let Splice { range, count } = splice(&old, &new, |id| id);
            let mut applied = old.clone();
            applied.splice(range.clone(), new[range.start..range.start + count].iter().copied());
            prop_assert_eq!(applied, new);
        }
    }
}
