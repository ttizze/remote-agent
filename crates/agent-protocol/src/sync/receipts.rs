use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt<C, T> {
    pub target: C,
    pub outcome: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission<T> {
    Execute,
    Replay(T),
    WrongStore,
    Conflict,
}

/// The receipt lookup is by operation ID, owned and persisted by the Host in
/// the same transaction as the effect. It survives deletion of the target.
pub fn admit<C: Eq, T: Clone>(
    store: uuid::Uuid,
    requested_store: uuid::Uuid,
    target: &C,
    receipt: Option<&Receipt<C, T>>,
) -> Admission<T> {
    if store != requested_store {
        return Admission::WrongStore;
    }
    match receipt {
        Some(receipt) if &receipt.target != target => Admission::Conflict,
        Some(receipt) => Admission::Replay(receipt.outcome.clone()),
        None => Admission::Execute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_preserve_acceptance_and_rejection_after_target_deletion() {
        let store = uuid::Uuid::from_u128(1);
        for outcome in [Ok(()), Err("refused")] {
            let receipt = Receipt {
                target: "thread",
                outcome,
            };
            assert_eq!(
                admit(store, store, &"thread", Some(&receipt)),
                Admission::Replay(outcome)
            );
            assert_eq!(
                admit(store, store, &"other", Some(&receipt)),
                Admission::Conflict
            );
            assert_eq!(
                admit(store, uuid::Uuid::from_u128(2), &"thread", Some(&receipt)),
                Admission::WrongStore
            );
        }
        assert_eq!(
            admit::<_, ()>(store, store, &"thread", None),
            Admission::Execute
        );
    }
}
