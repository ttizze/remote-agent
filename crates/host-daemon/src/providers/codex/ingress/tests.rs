use super::*;
use serde_json::json;

fn message(sequence: u64, method: &str, params: Value) -> PeerEvent {
    PeerEvent::Message(Reply {
        sequence,
        value: Arc::from(
            json!({"jsonrpc":"2.0","method":method,"params":params,"future":{"keep":true}})
                .to_string(),
        ),
    })
}
fn assistant(sequence: u64, text: &str) -> PeerEvent {
    message(
        sequence,
        "item/agentMessage/delta",
        json!({"threadId":"thread","turnId":"turn","itemId":"a","delta":text}),
    )
}
fn value(event: &PeerEvent) -> Value {
    let PeerEvent::Message(message) = event else {
        panic!("notification expected")
    };
    serde_json::from_str(&message.value).unwrap()
}

#[test]
fn interleaved_items_threads_and_reasoning_parts_keep_text_and_one_batch_fence() {
    let mut pending = Pending::default();
    for event in [
        assistant(1, "甲"),
        message(
            2,
            "item/agentMessage/delta",
            json!({"threadId":"thread","turnId":"turn","itemId":"b","delta":"乙"}),
        ),
        assistant(3, "😀"),
        message(
            4,
            "item/reasoning/summaryTextDelta",
            json!({"threadId":"thread","turnId":"turn","itemId":"r","summaryIndex":0,"delta":"考"}),
        ),
        assistant(5, "終"),
        message(
            6,
            "item/reasoning/textDelta",
            json!({"threadId":"thread","turnId":"turn","itemId":"r","contentIndex":0,"delta":"本文"}),
        ),
        message(
            7,
            "item/reasoning/summaryTextDelta",
            json!({"threadId":"thread","turnId":"turn","itemId":"r","summaryIndex":1,"delta":"別"}),
        ),
        message(
            8,
            "item/agentMessage/delta",
            json!({"threadId":"other","turnId":"turn","itemId":"a","delta":"他"}),
        ),
        message(
            9,
            "item/reasoning/summaryTextDelta",
            json!({"threadId":"thread","turnId":"turn","itemId":"r","summaryIndex":0,"delta":"察"}),
        ),
    ] {
        assert!(pending.append(&event));
    }
    let (output, mut received) = mpsc::unbounded_channel();
    assert!(pending.flush(&output));
    let batch = received.try_recv().unwrap();
    assert_eq!(batch.through, Some(9));
    let actual: Vec<_> = batch
        .events
        .iter()
        .map(|event| {
            let value = value(event);
            assert_eq!(value["future"], json!({"keep":true}));
            json!([
                value["params"]["threadId"],
                value["method"],
                value["params"]["itemId"],
                value["params"]["summaryIndex"],
                value["params"]["contentIndex"],
                value["params"]["delta"]
            ])
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            json!([
                "thread",
                "item/agentMessage/delta",
                "a",
                null,
                null,
                "甲😀終"
            ]),
            json!(["thread", "item/agentMessage/delta", "b", null, null, "乙"]),
            json!([
                "thread",
                "item/reasoning/summaryTextDelta",
                "r",
                0,
                null,
                "考察"
            ]),
            json!(["thread", "item/reasoning/textDelta", "r", null, 0, "本文"]),
            json!([
                "thread",
                "item/reasoning/summaryTextDelta",
                "r",
                1,
                null,
                "別"
            ]),
            json!(["other", "item/agentMessage/delta", "a", null, null, "他"]),
        ]
    );
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn response_request_and_close_flush_all_prior_text_in_wire_order() {
    let (input, events) = mpsc::unbounded_channel();
    let mut received = receive(events);
    input.send(assistant(1, "前")).unwrap();
    input.send(PeerEvent::Response { sequence: 2 }).unwrap();
    input.send(assistant(3, "後")).unwrap();
    let request = PeerEvent::Message(Reply {
        sequence: 4,
        value: Arc::from(
            r#"{"id":"approval","method":"item/agentMessage/delta","params":{"delta":"control"}}"#,
        ),
    });
    input.send(request.clone()).unwrap();
    input.send(assistant(5, "末尾")).unwrap();
    input.send(PeerEvent::Closed("finished".into())).unwrap();
    drop(input);
    let before = received.recv().await.unwrap();
    assert_eq!(before.through, Some(1));
    assert_eq!(before.events.len(), 1);
    assert_eq!(value(&before.events[0])["params"]["delta"], "前");
    let response = received.recv().await.unwrap();
    assert_eq!(response.through, Some(2));
    assert!(matches!(
        response.events.as_slice(),
        [PeerEvent::Response { sequence: 2 }]
    ));
    let after = received.recv().await.unwrap();
    assert_eq!(after.through, Some(3));
    assert_eq!(after.events.len(), 1);
    assert_eq!(value(&after.events[0])["params"]["delta"], "後");
    let approval = received.recv().await.unwrap();
    assert_eq!(approval.through, Some(4));
    assert_eq!(approval.events.len(), 1);
    assert_eq!(value(&approval.events[0]), value(&request));
    let tail = received.recv().await.unwrap();
    assert_eq!(tail.through, Some(5));
    assert_eq!(tail.events.len(), 1);
    assert_eq!(value(&tail.events[0])["params"]["delta"], "末尾");
    let closed = received.recv().await.unwrap();
    assert_eq!(closed.through, None);
    assert!(matches!(closed.events.as_slice(),[PeerEvent::Closed(reason)] if reason=="finished"));
    assert!(received.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn item_and_turn_completion_flush_text_without_a_second_timer_delivery() {
    // CodexAdapterV2.test.ts at 1a12c4af: buffered text precedes item/turn
    // completion synchronously and must not be emitted again after 50ms.
    let (input, events) = mpsc::unbounded_channel();
    let mut received = receive(events);
    for (sequence, method, item_id, text, params) in [
        (
            1,
            "item/completed",
            "a",
            "item final",
            json!({"threadId":"thread","turnId":"turn","item":{"type":"agentMessage","id":"a","text":"item final"}}),
        ),
        (
            3,
            "turn/completed",
            "b",
            "turn final",
            json!({"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}),
        ),
    ] {
        input
            .send(message(
                sequence,
                "item/agentMessage/delta",
                json!({"threadId":"thread","turnId":"turn","itemId":item_id,"delta":text}),
            ))
            .unwrap();
        input.send(message(sequence + 1, method, params)).unwrap();
        let delta = received.recv().await.unwrap();
        assert_eq!(delta.through, Some(sequence));
        assert_eq!(delta.events.len(), 1);
        assert_eq!(value(&delta.events[0])["params"]["delta"], text);
        let completion = received.recv().await.unwrap();
        assert_eq!(completion.through, Some(sequence + 1));
        assert_eq!(completion.events.len(), 1);
        assert_eq!(value(&completion.events[0])["method"], method);
    }
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    assert!(
        received.try_recv().is_err(),
        "completion drained the buffer"
    );
    drop(input);
    assert!(received.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn continuous_text_flushes_from_the_first_fragment_and_releases_its_receiver() {
    let (input, events) = mpsc::unbounded_channel();
    let mut received = receive(events);
    input.send(assistant(1, "A")).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(49)).await;
    assert!(
        received.try_recv().is_err(),
        "the provider interval is 50ms"
    );
    input.send(assistant(2, "B")).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    let batch = tokio::time::timeout(Duration::from_millis(1), received.recv())
        .await
        .expect("continuous arrivals must not postpone the scheduled flush")
        .unwrap();
    assert_eq!(batch.through, Some(2));
    assert_eq!(batch.events.len(), 1);
    assert_eq!(value(&batch.events[0])["params"]["delta"], "AB");
    drop(received);
    tokio::time::timeout(Duration::from_millis(1), input.closed())
        .await
        .expect("no orphan receiver after persistence exits");
}
