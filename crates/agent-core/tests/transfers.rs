use agent_core::peer::{JsonlReader, JsonlWriter};
use agent_core::{
    transfers::{download_file, upload_file},
    transport::{Endpoint, Identity, Relays, Trust},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn upload_and_download_use_distinct_iroh_streams_and_preserve_content() {
    let client_identity = Identity::generate();
    let trust = Trust {
        allowed: BTreeSet::from([client_identity.node_id()]),
        ..Default::default()
    };
    let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
        .await
        .unwrap();
    let client = Endpoint::bind(client_identity, Relays::Disabled)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("download.bin");
    let uploaded = directory.path().join("host-upload.bin");
    let content: Vec<u8> = (0..65537).map(|index| (index % 251) as u8).collect();
    tokio::fs::write(&source, &content).await.unwrap();
    let digest = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, &content));
    let token = URL_SAFE_NO_PAD.encode([42u8; 32]);
    let server = async {
        let session = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let control = session.accept_stream().await.unwrap();
        let (read, write) = tokio::io::split(control);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(""));
        for method in ["host/blob/upload", "host/blob/download"] {
            let request: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            if method.ends_with("upload") {
                assert_eq!(request["params"]["size"], content.len());
                assert_eq!(request["params"]["sha256"], digest);
            }
            writer.write_line(&json!({"id":request["id"],"result":{"token":token,"size":content.len(),"sha256":digest}}).to_string()).await.unwrap();
            let mut stream = session.accept_stream().await.unwrap();
            let length = stream.read_u32().await.unwrap();
            assert_eq!(length, 43);
            let mut received_token = [0; 43];
            stream.read_exact(&mut received_token).await.unwrap();
            assert_eq!(received_token.as_slice(), token.as_bytes());
            if method.ends_with("upload") {
                let mut received = Vec::new();
                stream.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, content);
                let acknowledgement =
                    json!({"path":uploaded,"size":content.len(),"sha256":digest}).to_string();
                stream
                    .write_u32(acknowledgement.len() as u32)
                    .await
                    .unwrap();
                stream.write_all(acknowledgement.as_bytes()).await.unwrap();
            } else {
                stream.write_all(&content).await.unwrap();
            }
            stream.shutdown().await.unwrap();
        }
        assert!(reader.read_line().await.unwrap().is_none());
    };
    let transfer = async {
        let session = client.connect(&host.ticket()).await.unwrap();
        let peer = session.open_peer(Duration::from_secs(3), 16).await.unwrap();
        let result = upload_file(
            &peer,
            || async { session.open_stream().await.map_err(io::Error::other) },
            &source,
            directory.path(),
            "source.bin",
        )
        .await
        .unwrap();
        assert_eq!(result["path"], serde_json::to_value(&uploaded).unwrap());
        download_file(
            &peer,
            || async { session.open_stream().await.map_err(io::Error::other) },
            &uploaded,
            &destination,
        )
        .await
        .unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), content);
        peer.close().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(server, transfer);
    })
    .await
    .unwrap();
    client.close().await;
    host.close().await;
}
