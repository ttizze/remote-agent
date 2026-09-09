use agent_core::{
    models::Invitation,
    transport::{Identity, Ticket, TransportError, Trust, authorize},
};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn pairing_consumes_invitation_and_authorizes_only_the_paired_identity() {
    let host = Identity::generate();
    let client = Identity::generate();
    let stranger = Identity::generate();
    let trust = Trust {
        allowed: BTreeSet::from([host.node_id()]),
        ..Default::default()
    };
    assert!(matches!(
        authorize(&trust, client.node_id(), None, 0),
        Err(TransportError::Unauthorized)
    ));
    let token = uuid::Uuid::new_v4();
    let issued = Trust {
        invitations: BTreeMap::from([(token, 100)]),
        ..trust
    };
    assert!(matches!(
        authorize(&issued, client.node_id(), Some(token), 100),
        Err(TransportError::Unauthorized)
    ));
    let paired = authorize(&issued, client.node_id(), Some(token), 99)
        .unwrap()
        .unwrap();
    assert!(paired.invitations.is_empty());
    assert!(paired.allowed.contains(&host.node_id()));
    assert!(
        authorize(&paired, client.node_id(), None, 101)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        authorize(&paired, stranger.node_id(), Some(token), 99),
        Err(TransportError::Unauthorized)
    ));
    let restored: Trust = serde_json::from_slice(&serde_json::to_vec(&paired).unwrap()).unwrap();
    assert!(restored == paired);
}
#[tokio::test]
async fn endpoint_ticket_and_identity_round_trip_with_public_services_disabled() {
    use agent_core::transport::{Endpoint, Relays};
    let identity = Identity::generate();
    let restored = Identity::from_bytes(identity.to_bytes());
    assert_eq!(identity.node_id(), restored.node_id());
    let endpoint = Endpoint::bind(identity, Relays::Disabled).await.unwrap();
    let ticket = endpoint.ticket();
    let parsed: Ticket = ticket.to_string().parse().unwrap();
    assert_eq!(parsed.node_id(), endpoint.node_id());
    let pairing = Invitation {
        endpoint: parsed.to_string(),
        invitation: uuid::Uuid::new_v4(),
        expires_at: 100,
        extra: Default::default(),
    };
    let restored: Invitation =
        serde_json::from_slice(&serde_json::to_vec(&pairing).unwrap()).unwrap();
    assert_eq!(restored.invitation, pairing.invitation);
    assert_eq!(restored.expires_at, 100);
    assert_eq!(
        restored.endpoint.parse::<Ticket>().unwrap().node_id(),
        endpoint.node_id()
    );
    endpoint.close().await;
}
