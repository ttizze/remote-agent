use super::*;
use crate::test_support::*;

#[test]
fn automatic_intent_uses_the_negotiated_session_capabilities() {
    let mut p = running();
    let mut capabilities = crate::capabilities::capabilities(Driver::Codex);
    let provider_thread = &mut p.provider_threads[0];
    let session_id = ProviderSessionId::new("session-policy").unwrap();
    provider_thread.provider_session_id = Some(session_id.clone());
    p.provider_sessions.push(ProviderSession {
        id: session_id,
        driver: Driver::Codex,
        provider_instance_id: ProviderInstanceId::new("codex").unwrap(),
        status: SessionStatus::Running,
        cwd: "/workspace".into(),
        model: None,
        capabilities: capabilities.clone(),
        created_at: now(),
        updated_at: now(),
        last_error: None,
    });
    let requested = DispatchMode::StartImmediately;
    assert_eq!(
        resolve_message_dispatch_intent(&p, &requested, Some(DeliveryIntent::Auto)),
        DispatchMode::SteerActive {
            target_run_id: p.runs[0].id.clone()
        }
    );
    capabilities.turns.supports_active_steering = false;
    p.provider_sessions[0].capabilities = capabilities;
    assert_eq!(
        resolve_message_dispatch_intent(&p, &requested, Some(DeliveryIntent::Auto)),
        DispatchMode::QueueAfterActive
    );
}
