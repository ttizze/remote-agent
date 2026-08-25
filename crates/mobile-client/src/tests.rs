use std::{
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use host_protocol::{Ed25519PublicKey, PairingToken};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};

use super::*;

fn valid_config() -> MobileClientConfig {
    MobileClientConfig {
        address: "127.0.0.1:22".parse().unwrap(),
        host_identity: Ed25519PublicKey::from_bytes([1; 32]),
        device_name: "test phone".to_owned(),
        pairing_ticket: None,
        request_timeout: Duration::from_secs(1),
    }
}

#[test]
fn pairing_config_is_validated_before_network_io() {
    let mut config = valid_config();
    config.pairing_ticket = Some(PairingToken::from_bytes([7; 32]));
    assert!(config.validate().is_ok());

    config.device_name.clear();
    assert!(matches!(
        config.validate(),
        Err(MobileClientError::InvalidConfig("device_name is empty"))
    ));
}

#[tokio::test]
async fn connection_attempt_is_bounded_by_the_configured_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _connection = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(1));
    });

    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let mut config = valid_config();
    config.address = address;
    config.request_timeout = Duration::from_millis(50);

    let started = Instant::now();
    let result = MobileClient::connect(config, key.as_ref()).await;

    assert!(matches!(result, Err(MobileClientError::ConnectionTimeout)));
    assert!(started.elapsed() < Duration::from_millis(500));
}
