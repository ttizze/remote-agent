use std::{path::PathBuf, sync::Arc};

use host_daemon::{
    CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, HostIdentity, HostRuntime,
    KeychainRemoteStore, LocalListener, RemoteHosts,
};
use host_protocol::RelayEndpoint;
use tokio_util::sync::CancellationToken;

use crate::command_line::StartupConfig;

pub(crate) async fn run(config: StartupConfig) -> Result<(), String> {
    let directory = match config.state_dir {
        Some(path) => path,
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set; provide --state-dir")?)
                .join(".bex")
        }
    };
    // Acquire the process lock before touching identities or starting Codex.
    let listener = LocalListener::bind(&directory).map_err(|error| error.to_string())?;
    let directory = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let account = directory.to_str().ok_or("state directory is not UTF-8")?;
    let provided = if config.configure {
        use std::io::Read;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 65536 {
            return Err("relay configuration is too large".into());
        }
        Some(
            serde_json::from_slice::<RelayEndpoint>(&bytes)
                .map_err(|_| "invalid relay configuration JSON")?,
        )
    } else {
        config.relay
    };
    let endpoint = match provided {
        Some(endpoint) => {
            endpoint.validate().map_err(|error| error.to_string())?;
            let bytes = zeroize::Zeroizing::new(
                serde_json::to_vec(&endpoint).map_err(|error| error.to_string())?,
            );
            security_framework::passwords::set_generic_password(
                "app.bex.relay.v4",
                account,
                &bytes,
            )
            .map_err(|error| format!("Keychain configuration write failed: {}", error.code()))?;
            endpoint
        }
        None => {
            let bytes = zeroize::Zeroizing::new(
                security_framework::passwords::get_generic_password("app.bex.relay.v4", account)
                    .map_err(|_| "configure this Host's relay connection first")?,
            );
            serde_json::from_slice::<RelayEndpoint>(&bytes)
                .map_err(|_| "stored relay configuration is invalid")?
        }
    };
    endpoint.validate().map_err(|error| error.to_string())?;
    if config.configure {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    let identity = HostIdentity::load_or_create(
        "app.bex.host.identity.v4",
        directory.to_str().ok_or("state directory is not UTF-8")?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "macos"))]
    let identity: HostIdentity =
        return Err("Host identity storage currently requires macOS Keychain".into());
    let authentication = DeviceAuthenticationState::load(directory.join("devices.json"))?;
    let desktop_projects =
        DesktopProjectStore::from_environment().map_err(|error| error.to_string())?;
    let app_server = Arc::new(
        codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig {
            program: config.codex,
            ..codex_app_server::AppServerConfig::default()
        })
        .await
        .map_err(|error| error.to_string())?,
    );
    let service = CodexRpcService::new(app_server.clone(), desktop_projects);
    service.start();
    let remotes = RemoteHosts::load(Arc::new(KeychainRemoteStore::new(
        directory
            .to_str()
            .ok_or("state directory is not UTF-8")?
            .to_owned(),
    )))?;
    let host_name = std::env::var("HOSTNAME").unwrap_or_else(|_| "Mac".into());
    let runtime = Arc::new(HostRuntime::new(
        service,
        identity,
        authentication,
        host_name,
        endpoint,
        remotes,
    )?);
    let shutdown = CancellationToken::new();
    let result = {
        let run = runtime.clone().run(listener, shutdown.clone());
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            signal = tokio::signal::ctrl_c() => {
                shutdown.cancel();
                let result = run.await;
                signal.map_err(|error| error.to_string()).and(result)
            }
        }
    };
    drop(runtime);
    let app_server =
        Arc::try_unwrap(app_server).map_err(|_| "active session retained Codex during shutdown")?;
    app_server
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    result
}
