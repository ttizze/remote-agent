//! Loopback pairing controls for the script-owned UI fixture.
use std::path::PathBuf;
use xtask::{Result, pairing::PairingServer};

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let state = PathBuf::from(arguments.next().ok_or("fixture state directory required")?);
    let port_file = PathBuf::from(arguments.next().ok_or("port output file required")?);
    if arguments.next().is_some() {
        return Err("usage: bex-pairing-fixture STATE PORT_FILE".into());
    }
    let pairing = PairingServer::start(&state, 0)?;
    std::fs::write(port_file, pairing.port.to_string())?;
    tokio::signal::ctrl_c().await?;
    pairing.shutdown()
}
