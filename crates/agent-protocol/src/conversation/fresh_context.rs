use super::*;

pub(super) fn begin(state: &State, input: &PendingInput) -> Option<Vec<Change>> {
    if !input.content.fresh_context || input.context_reset.is_some() {
        return None;
    }
    let reset = ContextReset {
        provider: state.thread.provider,
        context_start: state.thread.context_start,
        bindings: state.bindings.clone(),
    };
    let mut changes = vec![
        Change::Thread(Thread {
            context_start: state.counters.turn,
            ..state.thread.clone()
        }),
        Change::Input(PendingInput {
            context_reset: Some(reset),
            ..input.clone()
        }),
    ];
    changes.extend(state.bindings.keys().map(|provider| Change::Binding {
        provider: *provider,
        value: None,
    }));
    Some(changes)
}

pub(super) fn restore(state: &State, id: &ClientInputId, mut result: Step) -> Step {
    let Some(reset) = state
        .inputs
        .get(id)
        .and_then(|input| input.context_reset.as_ref())
    else {
        return result;
    };
    let next = state.apply(&result.changes);
    result.changes.extend(
        next.bindings
            .keys()
            .filter(|provider| !reset.bindings.contains_key(provider))
            .map(|provider| Change::Binding {
                provider: *provider,
                value: None,
            }),
    );
    result.changes.extend(
        reset
            .bindings
            .iter()
            .map(|(provider, binding)| Change::Binding {
                provider: *provider,
                value: Some(binding.clone()),
            }),
    );
    result.changes.push(Change::Thread(Thread {
        context_start: reset.context_start,
        provider: reset.provider,
        ..next.thread
    }));
    if let Some(input) = next.inputs.get(id) {
        result.changes.push(Change::Input(PendingInput {
            context_reset: None,
            ..input.clone()
        }));
    }
    result
}
