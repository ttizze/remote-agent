use agent_transport::peer::{JsonlReader, PeerEvent, RpcPeer};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn slow_subscribers_keep_every_notification_request_response_and_close() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let (read, write) = tokio::io::split(client);
    let peer = RpcPeer::open(JsonlReader::new(read), write, None, 1).unwrap();
    let mut first = peer.subscribe();
    let mut second = peer.subscribe();
    let (read, mut write) = tokio::io::split(server);
    let producer = tokio::spawn(async move {
        for index in 0..10_000 {
            write
                .write_all(
                    format!("{{\"method\":\"delta\",\"params\":{{\"index\":{index}}}}}\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
        }
        write
            .write_all(b"{\"id\":\"approval\",\"method\":\"approve\",\"params\":{}}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        write
            .write_all(format!("{{\"id\":{},\"result\":{{}}}}\n", request["id"]).as_bytes())
            .await
            .unwrap();
    });
    // Receiving the tail RPC response proves that the actual JSONL reader has
    // consumed the whole burst while both notification subscribers were idle.
    let response = peer
        .request::<_, Value>("barrier", &json!({}))
        .await
        .unwrap();
    producer.await.unwrap();
    for events in [&mut first, &mut second] {
        for index in 0..10_000 {
            let PeerEvent::Message(event) = events.recv().await.unwrap() else {
                panic!("notification {index} was lost")
            };
            let value: Value = serde_json::from_str(&event.value).unwrap();
            assert_eq!(value["params"]["index"], index);
            assert_eq!(event.sequence, index + 1);
        }
        let PeerEvent::Message(approval) = events.recv().await.unwrap() else {
            panic!("server request was lost")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&approval.value).unwrap()["id"],
            "approval"
        );
        assert!(
            matches!(events.recv().await.unwrap(),PeerEvent::Response {sequence} if sequence==response.sequence)
        );
        assert!(matches!(events.recv().await.unwrap(), PeerEvent::Closed(_)));
    }
}
