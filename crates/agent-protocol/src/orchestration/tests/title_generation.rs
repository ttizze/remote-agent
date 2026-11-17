use super::*;

fn draft() -> State {
    let mut state = initial();
    state.thread.title.clear();
    state
}

fn title_effect(state: &State) -> &Effect {
    state
        .effects
        .values()
        .find(|effect| matches!(effect.request, EffectRequest::ThreadTitleGenerate { .. }))
        .unwrap()
}

fn generating() -> State {
    let mut state = accepted(&draft(), 1, Admission::Queue);
    let id = title_effect(&state).id.clone();
    state.effects.get_mut(&id).unwrap().status = EffectStatus::Running;
    state.effects.get_mut(&id).unwrap().lease_owner = Some("title-worker".into());
    state
}

#[test]
fn first_real_input_titles_a_draft_once_without_losing_execution_metadata() {
    let state = draft();
    let mut message = input(1);
    message.text = "  日本語のタスクを要約してほしい\n詳細".into();
    message.model_selection.model = "selected-model".into();
    let admitted =
        state.apply(&admit(&state, message, Admission::Queue, context("first", &state)).unwrap());
    assert_eq!(admitted.thread.title, "日本語のタスクを要約してほしい");
    assert_eq!(admitted.thread.model_selection.model, "selected-model");
    assert!(admitted.thread.active_provider_thread_id.is_some());
    let next = accepted(&admitted, 2, Admission::Queue);
    assert_eq!(next.thread.title, admitted.thread.title);
    assert_eq!(
        next.effects
            .values()
            .filter(|effect| matches!(effect.request, EffectRequest::ThreadTitleGenerate { .. }))
            .count(),
        1
    );
    let compact = {
        let mut message = input(1);
        message.text = "/compact".into();
        state.apply(
            &admit(
                &state,
                message,
                Admission::Queue,
                context("compact", &state),
            )
            .unwrap(),
        )
    };
    assert!(compact.thread.title.is_empty());
    let real = accepted(&compact, 2, Admission::Queue);
    assert_eq!(real.thread.title, "input 2");
    assert!(
        matches!(&title_effect(&real).request, EffectRequest::ThreadTitleGenerate {message_id,..} if message_id == &input(2).message_id)
    );
}

#[test]
fn failed_admission_and_named_or_forked_threads_never_arm_generation() {
    let state = draft();
    let mut empty = input(1);
    empty.text.clear();
    assert!(admit(&state, empty, Admission::Queue, context("empty", &state)).is_err());
    assert!(state.effects.is_empty());
    for title in ["Manually named", "Source title (2)"] {
        let mut state = draft();
        state.thread.title = title.into();
        let state = accepted(&state, 1, Admission::Queue);
        assert_eq!(state.thread.title, title);
        assert!(
            !state
                .effects
                .values()
                .any(|effect| matches!(effect.request, EffectRequest::ThreadTitleGenerate { .. }))
        );
    }
}

#[test]
fn only_the_current_live_generation_can_replace_its_placeholder() {
    let state = generating();
    let id = title_effect(&state).id.clone();
    let renamed = state.apply(&titles::complete(
        &state,
        &id,
        "「内容からタスク名を生成」",
        NOW,
    ));
    assert_eq!(renamed.thread.title, "内容からタスク名を生成");
    assert!(
        titles::complete(&state, "unknown", "late", NOW)
            .changes
            .is_empty()
    );
    assert!(
        titles::complete(&state, &id, &state.thread.title, NOW)
            .changes
            .is_empty()
    );
    let mut replaced = state.clone();
    replaced.thread.title = "Separately updated title".into();
    assert!(
        titles::complete(&replaced, &id, "late", NOW)
            .changes
            .is_empty()
    );
    for status in [
        EffectStatus::Pending,
        EffectStatus::Succeeded,
        EffectStatus::Failed,
        EffectStatus::Cancelled,
    ] {
        let mut inactive = state.clone();
        inactive.effects.get_mut(&id).unwrap().status = status;
        assert!(
            titles::complete(&inactive, &id, "late", NOW)
                .changes
                .is_empty()
        );
    }
    for value in ["", " \n ", "New thread", "New task"] {
        assert!(titles::complete(&state, &id, value, NOW).changes.is_empty());
    }
    let mut stale = state.clone();
    stale.thread.archived_at = Some(NOW.into());
    assert!(
        titles::complete(&stale, &id, "late", NOW)
            .changes
            .is_empty()
    );
    stale.thread.archived_at = None;
    stale.thread.deleted_at = Some(NOW.into());
    assert!(
        titles::complete(&stale, &id, "late", NOW)
            .changes
            .is_empty()
    );
    let renamed_at = "2026-11-14T00:00:02.000Z";
    let manually_named =
        state.apply(&titles::rename(&state, &state.thread.title, renamed_at).unwrap());
    assert_eq!(
        title_effect(&manually_named).status,
        EffectStatus::Cancelled
    );
    assert_eq!(title_effect(&manually_named).lease_owner, None);
    assert_eq!(title_effect(&manually_named).updated_at, renamed_at);
    assert_eq!(
        title_effect(&manually_named).completed_at.as_deref(),
        Some(renamed_at)
    );
    for (other_id, effect) in &state.effects {
        if other_id != &id {
            assert_eq!(&manually_named.effects[other_id], effect);
        }
    }
    assert!(
        titles::complete(&manually_named, &id, "late", NOW)
            .changes
            .is_empty()
    );
    let changed = state.apply(&titles::rename(&state, "Custom title", NOW).unwrap());
    assert!(
        titles::complete(&changed, &id, "late", NOW)
            .changes
            .is_empty()
    );
    assert!(titles::rename(&state, " \n ", NOW).is_err());
}

#[test]
fn title_prompt_bounds_user_content_and_preserves_language_and_attachment_names() {
    let text = format!("最初の依頼{}最後の依頼", "日本語\n".repeat(10_000));
    let prompt = titles::prompt(&text, &[serde_json::json!({"name":"画面.png"})]);
    assert!(prompt.contains("日本語\\n日本語"));
    assert!(prompt.contains("画面.png"));
    assert!(prompt.contains("Use the user's language"));
    let request: String = serde_json::from_str(
        prompt
            .lines()
            .find_map(|line| line.strip_prefix("Request: "))
            .unwrap(),
    )
    .unwrap();
    assert!(request.starts_with("最初の依頼"));
    assert!(request.ends_with("最後の依頼"));
    assert!(request.chars().count() < 8_050);
}

#[test]
fn native_notices_refresh_imports_but_cannot_replace_application_titles() {
    let state = generating();
    assert!(
        titles::observe_native(&state, "native title", NOW)
            .changes
            .is_empty()
    );
    let mut imported = initial();
    imported.thread.created_by = Actor::System;
    let renamed = imported.apply(&titles::observe_native(&imported, "new native title", NOW));
    assert_eq!(renamed.thread.title, "new native title");
    assert!(
        titles::observe_native(&imported, " \n ", NOW)
            .changes
            .is_empty()
    );
}
