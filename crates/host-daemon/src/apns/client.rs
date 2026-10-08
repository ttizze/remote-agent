//! HTTP/2 APNs transport. Credentials stay in memory and outside the repository.
use agent_protocol::live_activity::{PushEnvironment, TASK_ACTIVITY_PUSH_TTL_SECONDS};
use anyhow::{Context, Result};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub key_id: String,
    pub team_id: String,
    pub bundle_id: String,
    pub key_file: PathBuf,
    pub environment: PushEnvironment,
}
pub(super) struct Client {
    http: reqwest::Client,
    config: Config,
    key: EcdsaKeyPair,
    token: Mutex<Option<(u64, Zeroizing<String>)>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResultKind {
    Accepted,
    Expired,
    Retry,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum Rejection {
    BadDeviceToken,
    DeviceTokenNotForTopic,
    Unregistered,
    BadTopic,
    TopicDisallowed,
    InvalidProviderToken,
    ExpiredProviderToken,
    TooManyProviderTokenUpdates,
    BadPriority,
    PayloadTooLarge,
    #[serde(other)]
    Unknown,
}

fn rejection_result(status: u16, rejection: Rejection) -> ResultKind {
    match status {
        410 => ResultKind::Expired,
        400 if matches!(
            rejection,
            Rejection::BadDeviceToken | Rejection::DeviceTokenNotForTopic
        ) =>
        {
            ResultKind::Expired
        }
        _ => ResultKind::Retry,
    }
}

impl Client {
    pub async fn load(path: &Path) -> Result<Option<Self>> {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("cannot read APNs configuration"),
        };
        let mut config: Config =
            serde_json::from_slice(&bytes).context("invalid APNs configuration")?;
        anyhow::ensure!(
            valid_id(&config.key_id) && valid_id(&config.team_id),
            "APNs Key ID and Team ID must be 10 ASCII letters/digits"
        );
        anyhow::ensure!(
            !config.bundle_id.is_empty()
                && config
                    .bundle_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')),
            "invalid APNs bundle ID"
        );
        if config.key_file.is_relative() {
            config.key_file = path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.key_file);
        }
        let pem = Zeroizing::new(
            tokio::fs::read_to_string(&config.key_file)
                .await
                .context("cannot read APNs private key file")?,
        );
        let key = parse_key(&pem)?;
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let http = reqwest::Client::builder()
            .http2_prior_knowledge()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .context("cannot initialize APNs HTTP/2 client")?;
        Ok(Some(Self {
            http,
            config,
            key,
            token: Mutex::new(None),
        }))
    }
    pub fn environment(&self) -> PushEnvironment {
        self.config.environment
    }

    fn authentication(&self, now: u64) -> Result<Zeroizing<String>> {
        let mut cached = self.token.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((issued, token)) = cached.as_ref()
            && now >= *issued
            && now - issued < 50 * 60
        {
            return Ok(token.clone());
        }
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &serde_json::json!({"alg":"ES256", "kid":self.config.key_id}),
        )?);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &serde_json::json!({"iss":self.config.team_id, "iat":now}),
        )?);
        let input = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| anyhow::anyhow!("cannot sign APNs authentication token"))?;
        let token = Zeroizing::new(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ));
        *cached = Some((now, token.clone()));
        Ok(token)
    }

    fn request(
        &self,
        token: &[u8],
        payload: &[u8],
        urgent: bool,
        now: u64,
    ) -> Result<reqwest::Request> {
        let authentication = self.authentication(now)?;
        let endpoint = match self.config.environment {
            PushEnvironment::Sandbox => "https://api.sandbox.push.apple.com",
            PushEnvironment::Production => "https://api.push.apple.com",
        };
        let hex = token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.http
            .post(format!("{endpoint}/3/device/{hex}"))
            .version(reqwest::Version::HTTP_2)
            .bearer_auth(authentication.as_str())
            .header(
                "apns-topic",
                format!("{}.push-type.liveactivity", self.config.bundle_id),
            )
            .header("apns-push-type", "liveactivity")
            .header("apns-priority", if urgent { "10" } else { "5" })
            .header(
                "apns-expiration",
                (now + u64::from(TASK_ACTIVITY_PUSH_TTL_SECONDS)).to_string(),
            )
            .header("content-type", "application/json")
            .body(payload.to_owned())
            .build()
            .map_err(|_| anyhow::anyhow!("cannot encode APNs request"))
    }

    pub async fn send(&self, token: &[u8], payload: &[u8], urgent: bool, now: u64) -> ResultKind {
        let Ok(request) = self.request(token, payload, urgent, now) else {
            tracing::warn!(target: "bex", operation = "host.apns.delivery", "Cannot prepare a Live Activity push request");
            return ResultKind::Retry;
        };
        // reqwest errors include the URL, which contains a secret push token.
        let response = match self.http.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                let kind = if error.is_timeout() {
                    "timeout"
                } else if error.is_connect() {
                    "connection"
                } else {
                    "transport"
                };
                tracing::warn!(target: "bex", operation = "host.apns.delivery",
                    message = %format_args!("Live Activity push transport failed: {kind}"));
                return ResultKind::Retry;
            }
        };
        let status = response.status();
        if status.is_success() {
            tracing::info!(target: "bex", operation = "host.apns.delivery", "APNs accepted a Live Activity update");
            return ResultKind::Accepted;
        }
        #[derive(Deserialize)]
        struct Failure {
            reason: Rejection,
        }
        let rejection = response
            .json::<Failure>()
            .await
            .map_or(Rejection::Unknown, |failure| failure.reason);
        tracing::warn!(target: "bex", operation = "host.apns.delivery", error_code = u64::from(status.as_u16()),
            message = %format_args!("APNs rejected a Live Activity update: {rejection:?}"));
        rejection_result(status.as_u16(), rejection)
    }
}
fn valid_id(value: &str) -> bool {
    value.len() == 10 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}
fn parse_key(pem: &str) -> Result<EcdsaKeyPair> {
    let content = pem
        .strip_prefix("-----BEGIN PRIVATE KEY-----")
        .and_then(|pem| pem.trim().strip_suffix("-----END PRIVATE KEY-----"))
        .context("APNs private key must be a PKCS#8 PEM file")?;
    let encoded = Zeroizing::new(content.split_whitespace().collect::<String>());
    let bytes = Zeroizing::new(
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid APNs private key encoding"))?,
    );
    EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        &bytes,
        &SystemRandom::new(),
    )
    .map_err(|_| anyhow::anyhow!("APNs private key must use ES256"))
}

#[cfg(test)]
impl Client {
    pub(super) fn testing() -> Self {
        let random = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        Self {
            http: reqwest::Client::new(),
            config: Config {
                key_id: "TESTKEY001".into(),
                team_id: "TESTTEAM01".into(),
                bundle_id: "dev.remoteagent.mobile.ios".into(),
                key_file: PathBuf::new(),
                environment: PushEnvironment::Sandbox,
            },
            key: EcdsaKeyPair::from_pkcs8(
                &ECDSA_P256_SHA256_FIXED_SIGNING,
                pkcs8.as_ref(),
                &random,
            )
            .unwrap(),
            token: Mutex::new(None),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair, UnparsedPublicKey};

    #[test]
    fn configuration_rejections_keep_device_registration_and_diagnostics_exclude_response_text() {
        for reason in [
            Rejection::BadTopic,
            Rejection::TopicDisallowed,
            Rejection::InvalidProviderToken,
            Rejection::ExpiredProviderToken,
            Rejection::TooManyProviderTokenUpdates,
            Rejection::BadPriority,
            Rejection::PayloadTooLarge,
            Rejection::Unknown,
        ] {
            assert_eq!(rejection_result(400, reason), ResultKind::Retry);
            assert_eq!(rejection_result(403, reason), ResultKind::Retry);
        }
        assert_eq!(
            serde_json::from_str::<Rejection>("\"unexpected-secret-response\"").unwrap(),
            Rejection::Unknown
        );
        for reason in [Rejection::BadDeviceToken, Rejection::DeviceTokenNotForTopic] {
            assert_eq!(rejection_result(400, reason), ResultKind::Expired);
            assert_eq!(rejection_result(403, reason), ResultKind::Retry);
        }
        for reason in [Rejection::Unregistered, Rejection::Unknown] {
            assert_eq!(rejection_result(410, reason), ResultKind::Expired);
            assert_eq!(rejection_result(400, reason), ResultKind::Retry);
        }
    }

    proptest::proptest! {
        #[test]
        fn only_confirmed_invalid_devices_expire(status in 400u16..600, reason in 0usize..4) {
            let reasons = [Rejection::BadDeviceToken, Rejection::DeviceTokenNotForTopic,
                Rejection::Unregistered, Rejection::Unknown];
            let expected = if status == 410 || (status == 400 && reason < 2) { ResultKind::Expired }
                else { ResultKind::Retry };
            proptest::prop_assert_eq!(rejection_result(status, reasons[reason]), expected);
        }
    }
    #[test]
    fn push_request_uses_live_activity_topic_http2_and_the_signed_builds_environment() {
        let mut client = Client::testing();
        for (environment, domain) in [
            (PushEnvironment::Sandbox, "api.sandbox.push.apple.com"),
            (PushEnvironment::Production, "api.push.apple.com"),
        ] {
            client.config.environment = environment;
            for urgent in [false, true] {
                let payload = b"{\"aps\":{\"event\":\"update\"}}";
                let request = client.request(&[0xab; 32], payload, urgent, 10000).unwrap();
                assert_eq!(request.version(), reqwest::Version::HTTP_2);
                assert_eq!(request.method(), reqwest::Method::POST);
                assert_eq!(request.url().host_str(), Some(domain));
                assert_eq!(
                    request.url().path(),
                    format!("/3/device/{}", "ab".repeat(32))
                );
                assert_eq!(
                    request.headers()["apns-topic"],
                    "dev.remoteagent.mobile.ios.push-type.liveactivity"
                );
                assert_eq!(request.headers()["apns-push-type"], "liveactivity");
                assert_eq!(
                    request.headers()["apns-priority"],
                    if urgent { "10" } else { "5" }
                );
                assert_eq!(request.headers()["apns-expiration"], "10600");
                assert_eq!(request.headers()["content-type"], "application/json");
                assert!(request.headers()["authorization"].is_sensitive());
                assert_eq!(request.body().unwrap().as_bytes(), Some(payload.as_slice()));
            }
        }
    }
    #[test]
    fn authentication_is_es256_with_apple_claims_and_reuses_a_token_for_fifty_minutes() {
        let client = Client::testing();
        let token = client.authentication(10_000).unwrap();
        let parts: Vec<_> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |part| {
            serde_json::from_slice::<serde_json::Value>(&URL_SAFE_NO_PAD.decode(part).unwrap())
                .unwrap()
        };
        assert_eq!(
            decode(parts[0]),
            serde_json::json!({"alg":"ES256","kid":"TESTKEY001"})
        );
        assert_eq!(
            decode(parts[1]),
            serde_json::json!({"iss":"TESTTEAM01","iat":10000})
        );
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, client.key.public_key().as_ref())
            .verify(
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            )
            .unwrap();
        assert!(client.authentication(12_999).unwrap().as_str() == token.as_str());
        assert!(client.authentication(13_000).unwrap().as_str() != token.as_str());
    }
    #[test]
    fn private_key_pem_accepts_apple_pkcs8_and_rejects_wrong_encoding_or_algorithm() {
        let random = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
        let pem = Zeroizing::new(format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        ));
        assert!(parse_key(&pem).is_ok());
        for pem in [
            "secret",
            "-----BEGIN PRIVATE KEY-----\n?\n-----END PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----",
        ] {
            assert!(parse_key(pem).is_err());
        }
    }
    #[tokio::test]
    async fn absent_configuration_is_optional_but_invalid_configuration_fails_startup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bex-apns.json");
        assert!(Client::load(&path).await.unwrap().is_none());
        tokio::fs::write(&path, b"{}").await.unwrap();
        assert!(Client::load(&path).await.is_err());
    }
}
