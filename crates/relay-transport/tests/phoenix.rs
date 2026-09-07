//! Exercise the actual Phoenix relay, not a WebSocket imitation. Run in the
//! repository Nix shell after `cd apps/server && mix deps.get`.
use std::time::Duration;

use relay_transport::{connect_client, connect_runner};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

#[path = "../../../tests/relay-e2e/phoenix.rs"]
mod phoenix;

async fn echo(mut stream: DuplexStream, slow: bool) {
    let mut bytes = [0; 32 * 1024];
    if slow { tokio::time::sleep(Duration::from_millis(100)).await; }
    loop {
        let count = stream.read(&mut bytes).await.unwrap();
        if count == 0 { return; }
        stream.write_all(&bytes[..count]).await.unwrap();
    }
}

async fn round_trip(stream: DuplexStream, marker: u8) {
    const BLOCKS: usize = 256;
    const BLOCK: usize = 32 * 1024;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let write = async {
        let bytes = [marker; BLOCK];
        for _ in 0..BLOCKS { writer.write_all(&bytes).await.unwrap(); }
    };
    let read = async {
        let mut bytes = [0; BLOCK];
        for _ in 0..BLOCKS {
            reader.read_exact(&mut bytes).await.unwrap();
            assert!(bytes.iter().all(|byte| *byte == marker), "another client's bytes crossed routes");
        }
    };
    tokio::join!(write, read);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phoenix_isolates_concurrent_byte_streams_and_handles_backpressure_and_reconnect() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let (mut server, endpoint) = phoenix::start().await;
        assert!(connect_client(&endpoint).await.is_err(), "offline runner must reject clients");
        let (mut incoming, runner) = connect_runner(&endpoint).await.unwrap();
        assert!(connect_runner(&endpoint).await.is_err(), "runner ownership must be exclusive");
        let (first, first_task) = connect_client(&endpoint).await.unwrap();
        let first_route = incoming.recv().await.unwrap();
        let first_id = first_route.id.clone();
        let (second, second_task) = connect_client(&endpoint).await.unwrap();
        let second_route = incoming.recv().await.unwrap();
        assert_ne!(first_id, second_route.id);
        let first_echo = tokio::spawn(echo(first_route.stream, true));
        let second_echo = tokio::spawn(echo(second_route.stream, false));
        tokio::join!(round_trip(first, 19), round_trip(second, 93));
        drop(first_task);
        drop(second_task);
        first_echo.await.unwrap();
        second_echo.await.unwrap();

        let (mut reconnected, reconnect_task) = connect_client(&endpoint).await.unwrap();
        let reconnect_route = incoming.recv().await.unwrap();
        assert_ne!(first_id, reconnect_route.id);
        drop(runner);
        let mut byte = [0];
        assert_eq!(reconnected.read(&mut byte).await.unwrap(), 0, "runner disconnect must close client stream");
        drop(reconnect_task);
        drop(reconnect_route);
        drop(incoming);
        server.kill().await.unwrap();
        server.wait().await.unwrap();
    }).await.expect("real relay test exceeded its deadline");
}

#[tokio::test]
async fn wss_starts_tls_and_returns_an_error_when_the_peer_drops_the_handshake() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = [0; 3];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(&header[..2], &[22, 3], "wss must send a TLS handshake, not plaintext");
            // A peer that disappears during TLS negotiation is a connection
            // error. It must never panic the Host or mobile process.
        });
        let endpoint = host_protocol::RelayEndpoint {
            relay_url: format!("wss://localhost:{}/socket/websocket", address.port()),
            relay_token: "isolated-tls-fixture".into(),
            runner_id: "tls-fixture".into(),
        };
        assert!(connect_runner(&endpoint).await.is_err());
        peer.await.unwrap();
    }).await.expect("TLS failure test exceeded its deadline");
}
