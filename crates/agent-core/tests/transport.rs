use agent_core::transport::{Identity, TransportError, Trust, authorize};
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
async fn incoming_session_requires_allowlist_and_shutdown_is_distinct() {
    use agent_core::transport::{Endpoint, Relays};
    use std::{collections::BTreeSet, time::Duration};
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let ticket = host.ticket();
        let (outgoing, incoming) = tokio::join!(client.connect(&ticket), host.accept());
        let incoming = incoming.unwrap().unwrap();
        assert_eq!(incoming.node_id(), client.node_id());
        assert!(matches!(
            incoming.authorize(&Trust::default()),
            Err(TransportError::Unauthorized)
        ));
        outgoing.unwrap().close();
        client.close().await;

        let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let trust = Trust {
            allowed: BTreeSet::from([client.node_id()]),
            ..Default::default()
        };
        let ticket = host.ticket();
        let (outgoing, incoming) = tokio::join!(client.connect(&ticket), host.accept());
        let session = incoming.unwrap().unwrap().authorize(&trust).unwrap();
        assert_eq!(session.node_id(), client.node_id());
        session.close();
        outgoing.unwrap().close();
        client.close().await;
        host.close().await;
        assert!(host.accept().await.is_none());
    })
    .await
    .unwrap();
}
