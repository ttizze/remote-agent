//! Deterministic queue operations. The Host owns persistence and delivery.
use crate::{ids::ClientInputId, session::SessionRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub submission: crate::operations::Submission,
    pub delivery: crate::session::SubmissionDelivery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum QueueAction {
    Pause,
    Resume,
    Cancel {
        id: ClientInputId,
    },
    Move {
        id: ClientInputId,
        before: Option<ClientInputId>,
    },
    Edit {
        id: ClientInputId,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueControl {
    pub session: SessionRef,
    pub action: QueueAction,
}

/// Moving before itself is a no-op. Missing targets fail rather than moving a
/// different message when the queue changes while the client is editing it.
pub fn move_before(
    current: &[ClientInputId],
    id: &ClientInputId,
    before: Option<&ClientInputId>,
) -> Result<Vec<ClientInputId>, &'static str> {
    let from = current
        .iter()
        .position(|value| value == id)
        .ok_or("queued input is no longer available")?;
    if before == Some(id) {
        return Ok(current.to_vec());
    }
    let mut next = current.to_vec();
    let moved = next.remove(from);
    let to = match before {
        Some(before) => next
            .iter()
            .position(|value| value == before)
            .ok_or("queue destination is no longer available")?,
        None => next.len(),
    };
    next.insert(to, moved);
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moving_uses_the_remaining_order_and_rejects_stale_destinations() {
        let ids: Vec<ClientInputId> = ["a", "b", "c"].map(Into::into).into();
        assert_eq!(
            move_before(&ids, &ids[0], Some(&ids[2])).unwrap(),
            ["b", "a", "c"].map(Into::into)
        );
        assert_eq!(
            move_before(&ids, &ids[2], Some(&ids[0])).unwrap(),
            ["c", "a", "b"].map(Into::into)
        );
        assert_eq!(move_before(&ids, &ids[0], Some(&ids[0])).unwrap(), ids);
        assert!(move_before(&ids, &"missing".into(), None).is_err());
        assert!(move_before(&ids, &ids[0], Some(&"missing".into())).is_err());
    }
    proptest::proptest! {
        #[test]
        fn reorder_keeps_each_message_once(count in 1usize..64, from in 0usize..64, destination in 0usize..65) {
            let ids: Vec<ClientInputId> = (0..count).map(|n| n.to_string().into()).collect();
            let from = from % count;
            let before = ids.get(destination % (count + 1));
            let next = move_before(&ids, &ids[from], before).unwrap();
            proptest::prop_assert_eq!(next.len(), ids.len());
            let mut original = ids.clone(); original.sort();
            let mut sorted = next.clone(); sorted.sort();
            proptest::prop_assert_eq!(sorted, original);
            if before != Some(&ids[from]) {
                let position = next.iter().position(|id| id == &ids[from]).unwrap();
                if let Some(before) = before {
                    proptest::prop_assert_eq!(next.get(position + 1), Some(before));
                } else { proptest::prop_assert_eq!(position, count - 1); }
            }
        }
    }
}
