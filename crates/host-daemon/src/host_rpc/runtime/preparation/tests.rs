use super::*;

fn preparation() -> Preparation {
    Preparation {
        io: Default::default(),
        active: Default::default(),
        preparing_input: Mutex::new(HashMap::from([
            ("start".into(), "attempt".into()),
            ("other".into(), "other-attempt".into()),
        ])),
    }
}

#[test]
fn final_readiness_and_non_delivery_proof_removal_share_the_stop_lock() {
    let preparation = preparation();
    assert!(
        preparation
            .begin_input_write("start", || {
                assert!(preparation.preparing_input.try_lock().is_err());
                Ok(true)
            })
            .unwrap()
    );
    assert_eq!(
        *preparation.preparing_input.lock().unwrap(),
        HashMap::from([("other".into(), "other-attempt".into())])
    );
}

#[test]
fn cancelled_or_failed_readiness_keeps_the_non_delivery_proof() {
    let preparation = preparation();
    assert!(
        !preparation
            .begin_input_write("start", || Ok(false))
            .unwrap()
    );
    assert!(
        preparation
            .begin_input_write("start", || Err(Failure::new("read_failed", "unavailable")))
            .is_err()
    );
    assert_eq!(
        preparation.preparing_input.lock().unwrap().get("start"),
        Some(&"attempt".into())
    );
}

#[test]
fn delayed_stop_cannot_cancel_a_replacement_workspace_generation() {
    let preparation = preparation();
    let original = Uuid::new_v4();
    let replacement = Uuid::new_v4();
    let original_cancel = tokio_util::sync::CancellationToken::new();
    let replacement_cancel = tokio_util::sync::CancellationToken::new();
    *preparation.active.lock().unwrap() = Some((original, original_cancel.clone()));
    preparation.cancel_generation(original);
    assert!(original_cancel.is_cancelled());
    *preparation.active.lock().unwrap() = Some((replacement, replacement_cancel.clone()));
    preparation.cancel_generation(original);
    assert!(!replacement_cancel.is_cancelled());
    preparation.cancel_generation(replacement);
    assert!(replacement_cancel.is_cancelled());
}
