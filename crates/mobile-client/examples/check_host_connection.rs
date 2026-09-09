use std::{collections::HashSet, env, time::Duration};

use agent_core::client::{MobileClient, MobileClientConfig};
use host_protocol::{CURRENT_PROTOCOL_VERSION, PairingQrPayload};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payload: PairingQrPayload = serde_json::from_str(
        &env::var("BEX_PAIRING_PAYLOAD")
            .map_err(|_| "BEX_PAIRING_PAYLOAD must contain a fresh Host pairing payload")?,
    )?;
    if payload.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(format!(
            "Host protocol version {} does not match client version {CURRENT_PROTOCOL_VERSION}",
            payload.protocol_version
        )
        .into());
    }
    let config = MobileClientConfig {
        relay: payload.relay,
        host_identity: payload.host_identity,
        device_name: "Headless verification".into(),
        pairing_ticket: Some(payload.ticket),
        request_timeout: Duration::from_secs(10),
    };
    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "secure random generation failed")?;
    let reconnected = MobileClient::connect(config, key.as_ref()).await?;
    let projects = reconnected
        .request("host/project/list", json!({ "limit": 1 }))
        .await?;

    let threads = reconnected
        .request("host/thread/list", json!({ "limit": 20 }))
        .await?;
    let thread_entries = threads
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("Host returned an invalid thread list")?;
    let thread_count = thread_entries.len();
    let mut thread_ids = Vec::with_capacity(thread_entries.len());
    for thread in thread_entries {
        let thread = thread
            .as_object()
            .ok_or("Host returned an invalid thread list entry")?;
        let thread_id = thread
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("Host returned an invalid thread list entry id")?;
        if thread_id.trim().is_empty() {
            return Err("Host returned a blank thread id".into());
        }
        thread_ids.push(thread_id.to_owned());
    }
    if thread_ids.is_empty() {
        return Err("Host returned no threads".into());
    }

    let mut total_turn_count = 0;
    let mut total_item_count = 0;
    let mut zero_turn_thread_count = 0;
    let mut max_thread_item_count = 0;
    let mut max_turn_item_count = 0;
    let mut first_thread_turn_count = 0;
    let mut first_thread_item_count = 0;
    let mut first_thread_has_turns = false;
    let mut first_thread_has_items = false;
    let mut blank_turn_id_count = 0;
    let mut duplicate_turn_id_count = 0;
    let mut blank_item_id_count = 0;
    let mut duplicate_item_id_count = 0;
    for (thread_index, thread_id) in thread_ids.iter().enumerate() {
        let read = reconnected
            .request(
                "host/thread/read",
                json!({
                    "threadId": thread_id,
                    "includeTurns": true,
                }),
            )
            .await?;
        let thread = read
            .get("thread")
            .and_then(serde_json::Value::as_object)
            .ok_or("Host returned an invalid thread read")?;
        let read_thread_id = thread
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("Host returned an invalid thread read")?;
        if read_thread_id != thread_id {
            return Err("Host returned a different thread from the one requested".into());
        }

        let turns = thread
            .get("turns")
            .and_then(serde_json::Value::as_array)
            .ok_or("Host returned an invalid thread turns array")?;
        if turns.is_empty() {
            zero_turn_thread_count += 1;
        }
        let mut turn_ids = HashSet::new();
        let mut thread_item_count = 0;
        for turn in turns {
            let turn = turn
                .as_object()
                .ok_or("Host returned an invalid thread turn")?;
            match turn.get("id") {
                None | Some(serde_json::Value::Null) => blank_turn_id_count += 1,
                Some(id) => {
                    let id = id
                        .as_str()
                        .ok_or("Host returned an invalid thread turn id")?;
                    if id.trim().is_empty() {
                        blank_turn_id_count += 1;
                    } else if !turn_ids.insert(id.to_owned()) {
                        duplicate_turn_id_count += 1;
                    }
                }
            }
            let items = turn
                .get("items")
                .and_then(serde_json::Value::as_array)
                .ok_or("Host returned an invalid thread items array")?;
            if items.iter().any(|item| !item.is_object()) {
                return Err("Host returned an invalid thread item".into());
            }
            let mut item_ids = HashSet::new();
            for item in items {
                let item = item
                    .as_object()
                    .ok_or("Host returned an invalid thread item")?;
                match item.get("id") {
                    None | Some(serde_json::Value::Null) => blank_item_id_count += 1,
                    Some(id) => {
                        let id = id
                            .as_str()
                            .ok_or("Host returned an invalid thread item id")?;
                        if id.trim().is_empty() {
                            blank_item_id_count += 1;
                        } else if !item_ids.insert(id.to_owned()) {
                            duplicate_item_id_count += 1;
                        }
                    }
                }
            }
            thread_item_count += items.len();
            max_turn_item_count = max_turn_item_count.max(items.len());
            total_item_count += items.len();
        }
        total_turn_count += turns.len();
        max_thread_item_count = max_thread_item_count.max(thread_item_count);
        if thread_index == 0 {
            first_thread_turn_count = turns.len();
            first_thread_item_count = thread_item_count;
            first_thread_has_turns = first_thread_turn_count > 0;
            first_thread_has_items = first_thread_item_count > 0;
        }
    }

    reconnected.close();

    let project_count = projects
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    println!(
        "headless Host task load passed; received {project_count} project(s), {thread_count} thread(s), read {} thread(s), {total_turn_count} turn(s), {total_item_count} item(s), {zero_turn_thread_count} thread(s) with zero turns; first thread: {first_thread_turn_count} turn(s), {first_thread_item_count} item(s), has turns: {first_thread_has_turns}, items: {first_thread_has_items}; max thread items: {max_thread_item_count}, max turn items: {max_turn_item_count}; blank turn IDs: {blank_turn_id_count}, duplicate turn IDs: {duplicate_turn_id_count}, blank item IDs: {blank_item_id_count}, duplicate item IDs: {duplicate_item_id_count}",
        thread_ids.len(),
    );
    Ok(())
}
