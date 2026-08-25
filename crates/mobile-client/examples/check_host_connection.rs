use std::{env, time::Duration};

use host_protocol::{CURRENT_PROTOCOL_VERSION, PairingQrPayload};
use mobile_client::{MobileClient, MobileClientConfig};
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
    let address = payload
        .addresses
        .first()
        .ok_or("pairing payload has no address")?
        .parse()?;
    let device_key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "failed to generate the temporary device key")?;
    let base_config = MobileClientConfig {
        address,
        host_identity: payload.host_identity,
        device_name: "Bex headless smoke".to_owned(),
        pairing_ticket: None,
        request_timeout: Duration::from_secs(10),
    };

    let paired = MobileClient::connect(
        MobileClientConfig {
            pairing_ticket: Some(payload.ticket),
            ..base_config.clone()
        },
        device_key.as_ref(),
    )
    .await?;
    paired.close();
    drop(paired);

    let reconnected = MobileClient::connect(base_config, device_key.as_ref()).await?;
    let projects = reconnected
        .request("host/project/list", json!({ "limit": 1 }))
        .await?;
    reconnected.close();

    let project_count = projects
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    println!("headless Host connection passed; received {project_count} project(s)");
    Ok(())
}
