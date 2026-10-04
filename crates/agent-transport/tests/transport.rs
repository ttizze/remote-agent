use agent_transport::transport::{Identity, TransportError, Trust, authorize};
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
    use agent_transport::transport::{Endpoint, Relays};
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

#[tokio::test]
async fn local_client_connects_over_loopback_without_relays() {
    use agent_transport::transport::{Endpoint, Relays};
    use std::{collections::BTreeSet, time::Duration};
    tokio::time::timeout(Duration::from_secs(10), async {
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let client = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let client_address: iroh_tickets::endpoint::EndpointTicket =
            client.ticket().to_string().parse().unwrap();
        assert!(client_address.endpoint_addr().relay_urls().next().is_none());
        let client_ips: Vec<_> = client_address.endpoint_addr().ip_addrs().collect();
        assert!(!client_ips.is_empty());
        assert!(client_ips.iter().all(|addr| addr.ip().is_loopback()));
        let ticket = host.local_ticket();
        // Inspect the published contract with the official ticket decoder.
        let local: iroh_tickets::endpoint::EndpointTicket = ticket.to_string().parse().unwrap();
        assert!(local.endpoint_addr().relay_urls().next().is_none());
        let addresses: Vec<_> = local.endpoint_addr().ip_addrs().collect();
        assert!(!addresses.is_empty());
        assert!(addresses.iter().all(|addr| addr.ip().is_loopback()));
        let trust = Trust {
            allowed: BTreeSet::from([client.node_id()]),
            ..Default::default()
        };
        let (outgoing, incoming) = tokio::join!(client.connect(&ticket), host.accept());
        let incoming = incoming.unwrap().unwrap().authorize(&trust).unwrap();
        let session = outgoing.unwrap();
        let (peer, accepted) = tokio::join!(
            session.open_peer(Duration::from_secs(2), 8),
            incoming.accept_peer()
        );
        let (peer, _) = peer.unwrap();
        accepted.unwrap();
        assert!(matches!(
            peer.connection_path().0,
            agent_transport::diagnostics::ConnectionRoute::Direct
        ));
        session.close();
        incoming.close();
        client.close().await;
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn incompatible_wire_versions_are_rejected_in_both_directions() {
    use agent_transport::transport::{Endpoint, Relays, Ticket};
    use std::time::Duration;

    tokio::time::timeout(Duration::from_secs(10), async {
        let incompatible_alpn = b"remote-agent/streams/3";
        let current = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let incompatible = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![incompatible_alpn.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_address_lookup()
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .unwrap()
            .bind()
            .await
            .unwrap();

        let ticket: Ticket = iroh_tickets::endpoint::EndpointTicket::new(incompatible.addr())
            .to_string()
            .parse()
            .unwrap();
        let (outgoing, incoming) = tokio::join!(current.connect(&ticket), async {
            incompatible.accept().await.unwrap().await
        });
        assert!(matches!(outgoing, Err(TransportError::Connection(_))));
        assert!(incoming.is_err());

        let ticket: iroh_tickets::endpoint::EndpointTicket =
            current.local_ticket().to_string().parse().unwrap();
        let (outgoing, incoming) = tokio::join!(
            incompatible.connect(ticket.endpoint_addr().clone(), incompatible_alpn),
            current.accept()
        );
        assert!(outgoing.is_err());
        assert!(matches!(incoming, Some(Err(TransportError::Connection(_)))));

        incompatible.close().await;
        current.close().await;
    })
    .await
    .unwrap();
}
