//! Isolated, raw transport comparison. Neither endpoint talks to a running Host.
//! WebSocket is T3's transport family, not T3's Effect RPC implementation.
use anyhow::{Context, Result, ensure};
use async_tungstenite::{WebSocketStream, tokio::TokioAdapter, tungstenite::Message};
use futures_util::StreamExt;
use iroh::{Endpoint, RelayMode, endpoint::presets};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Instant,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const ALPN: &[u8] = b"bex/transport-comparison/1";
const CONTROL_BYTES: usize = 1024;
const BULK_BYTES: usize = 1024 * 1024;
type WebSocket = WebSocketStream<TokioAdapter<tokio_rustls::client::TlsStream<TcpStream>>>;

#[derive(Serialize)]
struct Measurements {
    transport: &'static str,
    connection_establishment_ms: Vec<f64>,
    control_rtt_ms: Vec<f64>,
    bulk_roundtrip_ms: Vec<f64>,
}

fn certificate(
    directory: &Path,
    name: &str,
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let cert = directory.join(format!("{name}.pem"));
    let key = directory.join(format!("{name}-key.pem"));
    // The private key exists only inside a temporary directory with mode 0700.
    // It is never emitted, checked into source, or added to the Nix store.
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "ed25519",
            "-noenc",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "extendedKeyUsage=serverAuth,clientAuth",
        ])
        .arg("-out")
        .arg(&cert)
        .arg("-keyout")
        .arg(&key)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run the Nix OpenSSL tool")?;
    ensure!(
        status.success(),
        "could not generate an ephemeral benchmark certificate"
    );
    Ok((
        CertificateDer::from_pem_file(cert)?,
        PrivateKeyDer::from_pem_file(key)?,
    ))
}

async fn websocket_server() -> Result<(SocketAddr, TlsConnector, tokio::task::JoinHandle<()>)> {
    let directory = tempfile::tempdir()?;
    let (server_cert, server_key) = certificate(directory.path(), "server")?;
    let (client_cert, client_key) = certificate(directory.path(), "client")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut clients = RootCertStore::empty();
    clients.add(client_cert.clone())?;
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(clients),
        provider.clone(),
    )
    .build()?;
    let server = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![server_cert.clone()], server_key)?;
    let mut roots = RootCertStore::empty();
    roots.add(server_cert)?;
    let client = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![client_cert], client_key)?;
    let acceptor = TlsAcceptor::from(Arc::new(server));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let result: Result<()> = async {
                    tcp.set_nodelay(true)?;
                    let tls = acceptor.accept(tcp).await?;
                    let mut socket = async_tungstenite::tokio::accept_async(tls).await?;
                    while let Some(message) = socket.next().await {
                        match message? {
                            message @ Message::Binary(_) => socket.send(message).await?,
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    eprintln!("benchmark WebSocket server: {error:#}");
                }
            });
        }
    });
    Ok((address, TlsConnector::from(Arc::new(client)), task))
}

async fn connect_websocket(address: SocketAddr, tls: &TlsConnector) -> Result<WebSocket> {
    let tcp = TcpStream::connect(address).await?;
    tcp.set_nodelay(true)?;
    let tls = tls.connect("localhost".try_into()?, tcp).await?;
    let (socket, _) = async_tungstenite::tokio::client_async(
        format!("wss://localhost:{}/echo", address.port()),
        tls,
    )
    .await?;
    Ok(socket)
}

async fn exchange_websocket(socket: &mut WebSocket, payload: &[u8]) -> Result<()> {
    socket
        .send(Message::Binary(payload.to_vec().into()))
        .await?;
    let response = socket.next().await.context("WebSocket echo closed")??;
    ensure!(
        matches!(response, Message::Binary(ref data) if data.as_ref() == payload),
        "WebSocket echo differs"
    );
    Ok(())
}

async fn endpoint() -> Result<Endpoint> {
    Ok(Endpoint::builder(presets::N0)
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")?
        .relay_mode(RelayMode::Disabled)
        .clear_address_lookup()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?)
}

async fn exchange_iroh(connection: &iroh::endpoint::Connection, payload: &[u8]) -> Result<()> {
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_all(payload).await?;
    send.finish()?;
    ensure!(
        receive.read_to_end(payload.len() + 1).await? == payload,
        "iroh echo differs"
    );
    Ok(())
}

async fn run(rounds: usize) -> Result<serde_json::Value> {
    let server = endpoint().await?;
    let client = endpoint().await?;
    let receiver = server.clone();
    let iroh_task = tokio::spawn(async move {
        while let Some(incoming) = receiver.accept().await {
            tokio::spawn(async move {
                let result: Result<()> = async {
                    let connection = incoming.await?;
                    while let Ok((mut send, mut receive)) = connection.accept_bi().await {
                        let payload = receive.read_to_end(BULK_BYTES + 1).await?;
                        send.write_all(&payload).await?;
                        send.finish()?;
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    eprintln!("benchmark iroh server: {error:#}");
                }
            });
        }
    });
    let (address, connector, websocket_task) = websocket_server().await?;
    let mut iroh = Measurements {
        transport: "iroh-1.1.0-QUIC-TLS1.3",
        connection_establishment_ms: vec![],
        control_rtt_ms: vec![],
        bulk_roundtrip_ms: vec![],
    };
    let mut websocket = Measurements {
        transport: "WebSocket-TCP-TLS1.3",
        connection_establishment_ms: vec![],
        control_rtt_ms: vec![],
        bulk_roundtrip_ms: vec![],
    };
    let control = vec![0x42; CONTROL_BYTES];
    let bulk = vec![0x42; BULK_BYTES];
    let mut order = Vec::new();
    for round in 0..rounds {
        let transports = if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        };
        order.push(if round % 2 == 0 {
            ["iroh", "websocket"]
        } else {
            ["websocket", "iroh"]
        });
        for use_iroh in transports {
            if use_iroh {
                for _ in 0..4 {
                    let started = Instant::now();
                    let connection = client.connect(server.addr(), ALPN).await?;
                    iroh.connection_establishment_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                    // QUIC must send application data before cleanly closing a
                    // connection; otherwise its accepted handshake can be cancelled.
                    exchange_iroh(&connection, &control).await?;
                    connection.close(0u32.into(), b"benchmark complete");
                }
                let connection = client.connect(server.addr(), ALPN).await?;
                for _ in 0..12 {
                    exchange_iroh(&connection, &control).await?;
                }
                for _ in 0..500 {
                    let started = Instant::now();
                    exchange_iroh(&connection, &control).await?;
                    iroh.control_rtt_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                }
                for _ in 0..16 {
                    let started = Instant::now();
                    exchange_iroh(&connection, &bulk).await?;
                    iroh.bulk_roundtrip_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                }
                connection.close(0u32.into(), b"benchmark complete");
            } else {
                for _ in 0..4 {
                    let started = Instant::now();
                    let mut socket = connect_websocket(address, &connector).await?;
                    websocket
                        .connection_establishment_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                    exchange_websocket(&mut socket, &control).await?;
                    socket.close(None).await?;
                }
                let mut socket = connect_websocket(address, &connector).await?;
                for _ in 0..12 {
                    exchange_websocket(&mut socket, &control).await?;
                }
                for _ in 0..500 {
                    let started = Instant::now();
                    exchange_websocket(&mut socket, &control).await?;
                    websocket
                        .control_rtt_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                }
                for _ in 0..16 {
                    let started = Instant::now();
                    exchange_websocket(&mut socket, &bulk).await?;
                    websocket
                        .bulk_roundtrip_ms
                        .push(started.elapsed().as_secs_f64() * 1000.);
                }
                socket.close(None).await?;
            }
        }
    }
    websocket_task.abort();
    let _ = websocket_task.await;
    server.close().await;
    client.close().await;
    iroh_task.abort();
    let _ = iroh_task.await;
    Ok(serde_json::json!({
        "schema": 1,
        "conditions": {"route": "IPv4 loopback", "relay": false, "tls": "TLS1.3 with mutual peer authentication", "certificate_algorithm": "Ed25519", "control_bytes": CONTROL_BYTES, "bulk_bytes_each_direction": BULK_BYTES, "warmup": 12, "rounds": rounds, "tcp_nodelay": true, "application": "raw echo, excluding T3 Effect RPC and Bex RPC", "iroh_request": "independent bidirectional stream", "websocket_request": "binary frame on a retained connection", "order": order},
        "measurements": [iroh, websocket]
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let rounds = std::env::args()
        .nth(1)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(6);
    ensure!(
        (2..=20).contains(&rounds),
        "rounds must be between 2 and 20"
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), run(rounds)).await??;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
