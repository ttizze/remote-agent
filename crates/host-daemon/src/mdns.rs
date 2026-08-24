use mdns_sd::{ServiceDaemon, ServiceInfo};
use ring::digest::{SHA256, digest};

use host_protocol::{CURRENT_PROTOCOL_VERSION, Ed25519PublicKey};

pub const SERVICE_TYPE: &str = "_bex._udp.local.";
const FINGERPRINT_BYTES: usize = 8;
const PROTOCOL_VERSION_TXT_KEY: &str = "protocolVersion";
const HOST_IDENTITY_FINGERPRINT_TXT_KEY: &str = "hostIdentityFingerprint";

/// The non-secret discovery information advertised for one running Host.
///
/// Discovery identifies a possible Host only. A Mobile Client must still
/// authenticate the Host's pinned long-lived identity over QUIC before using it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMetadata {
    pub instance_name: String,
    pub hostname: String,
    pub port: u16,
    pub txt_properties: Vec<(String, String)>,
}

impl ServiceMetadata {
    fn for_host(host_identity: Ed25519PublicKey, port: u16) -> Self {
        let fingerprint = host_identity_fingerprint(host_identity);
        let instance_name = format!("bex-{fingerprint}");
        Self {
            hostname: format!("{instance_name}.local."),
            instance_name,
            port,
            txt_properties: vec![
                (
                    PROTOCOL_VERSION_TXT_KEY.to_owned(),
                    CURRENT_PROTOCOL_VERSION.to_string(),
                ),
                (HOST_IDENTITY_FINGERPRINT_TXT_KEY.to_owned(), fingerprint),
            ],
        }
    }
}

/// Advertises a running Host and withdraws its service when dropped.
pub struct MdnsAdvertisement {
    daemon: ServiceDaemon,
    service_fullname: String,
}

impl MdnsAdvertisement {
    pub fn register(host_identity: Ed25519PublicKey, port: u16) -> Result<Self, MdnsError> {
        let metadata = ServiceMetadata::for_host(host_identity, port);
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &metadata.instance_name,
            &metadata.hostname,
            "",
            metadata.port,
            metadata.txt_properties.as_slice(),
        )?
        .enable_addr_auto();
        let service_fullname = service.get_fullname().to_owned();
        let daemon = ServiceDaemon::new()?;
        if let Err(error) = daemon.register(service) {
            let _ = daemon.shutdown();
            return Err(error.into());
        }

        Ok(Self {
            daemon,
            service_fullname,
        })
    }
}

impl Drop for MdnsAdvertisement {
    fn drop(&mut self) {
        // Shutdown follows unregister in the daemon command queue, so a normal
        // shutdown emits the DNS-SD goodbye before its worker exits. Drop
        // cannot report an already-stopped daemon, which is harmless.
        let _ = self.daemon.unregister(&self.service_fullname);
        let _ = self.daemon.shutdown();
    }
}

pub fn service_metadata(host_identity: Ed25519PublicKey, port: u16) -> ServiceMetadata {
    ServiceMetadata::for_host(host_identity, port)
}

fn host_identity_fingerprint(host_identity: Ed25519PublicKey) -> String {
    digest(&SHA256, host_identity.as_bytes())
        .as_ref()
        .iter()
        .take(FINGERPRINT_BYTES)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, thiserror::Error)]
#[error("mDNS advertisement failed: {0}")]
pub struct MdnsError(#[from] mdns_sd::Error);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_metadata_contains_only_non_secret_discovery_fields() {
        let metadata = service_metadata(Ed25519PublicKey::from_bytes([0; 32]), 49_152);

        assert_eq!(metadata.instance_name, "bex-66687aadf862bd77");
        assert_eq!(metadata.hostname, "bex-66687aadf862bd77.local.");
        assert_eq!(metadata.port, 49_152);
        assert_eq!(
            metadata.txt_properties,
            vec![
                (
                    PROTOCOL_VERSION_TXT_KEY.to_owned(),
                    CURRENT_PROTOCOL_VERSION.to_string(),
                ),
                (
                    HOST_IDENTITY_FINGERPRINT_TXT_KEY.to_owned(),
                    "66687aadf862bd77".to_owned(),
                ),
            ]
        );
    }
}
