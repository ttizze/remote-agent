use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use host_protocol::{Ed25519PublicKey, JsonlReader, JsonlWriter, RelayEndpoint};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, split},
    net::{UnixListener, UnixStream},
    sync::Mutex,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    CodexRpcService, DeviceAuthenticationState, EncryptedGateway, HostIdentity, RemoteHosts,
    jsonl_session::serve_jsonl_session, ssh_gateway::unix_time_millis,
};

/// The lock lives as long as the listener. Only its owner may replace a stale
/// socket, and shutdown removes only the inode this process created.
pub struct LocalListener {
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
    _lock: File,
}

impl LocalListener {
    pub fn bind(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let metadata = fs::symlink_metadata(directory)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Host state directory must be owned by the current user",
            ));
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("host.lock"))?;
        // SAFETY: the owned file descriptor is valid for this call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let path = directory.join("host.sock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == uid => {
                fs::remove_file(&path)?
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Host socket path is not an owned socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let inode = fs::symlink_metadata(&path)?.ino();
        Ok(Self {
            listener,
            path,
            inode,
            _lock: lock,
        })
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.ino() == self.inode && metadata.file_type().is_socket())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct HostRuntime {
    service: CodexRpcService,
    gateway: Arc<EncryptedGateway>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    identity: Ed25519PublicKey,
    host_name: String,
    endpoint: RelayEndpoint,
    relay_connected: AtomicBool,
    remotes: RemoteHosts,
}

impl HostRuntime {
    pub fn new(
        service: CodexRpcService,
        identity: HostIdentity,
        authentication: DeviceAuthenticationState,
        host_name: String,
        endpoint: RelayEndpoint,
        remotes: RemoteHosts,
    ) -> Result<Self, String> {
        endpoint.validate().map_err(|error| error.to_string())?;
        let authentication = Arc::new(Mutex::new(authentication));
        let gateway = Arc::new(EncryptedGateway::new(
            &identity,
            authentication.clone(),
            service.clone(),
        ));
        Ok(Self {
            service,
            gateway,
            authentication,
            remotes,
            identity: identity.public_key(),
            host_name,
            endpoint,
            relay_connected: AtomicBool::new(false),
        })
    }

    pub async fn run(
        self: Arc<Self>,
        listener: LocalListener,
        shutdown: CancellationToken,
    ) -> Result<(), String> {
        let relay = self.relay_loop(shutdown.clone());
        tokio::pin!(relay);
        let mut sessions = JoinSet::new();
        let mut relay_finished = false;
        let result = loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break Ok(()),
                _ = &mut relay => { relay_finished = true; break Err("relay loop stopped unexpectedly".into()); },
                result = listener.listener.accept() => match result {
                    Ok((stream, _)) => {
                        // Directory permissions are the first boundary; peer UID
                        // independently rejects inherited or forwarded sockets.
                        let uid = unsafe { libc::geteuid() };
                        if !stream.peer_cred().is_ok_and(|credentials| credentials.uid() == uid) { continue; }
                        if sessions.len() >= 64 { continue; }
                        let runtime = self.clone();
                        let stop = shutdown.clone();
                        sessions.spawn(async move { runtime.serve_local(stream, stop).await });
                    }
                    Err(error) => break Err(error.to_string()),
                },
                Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
            }
        };
        shutdown.cancel();
        if !relay_finished {
            relay.await;
        }
        while sessions.join_next().await.is_some() {}
        result
    }

    async fn relay_loop(&self, shutdown: CancellationToken) {
        loop {
            let connection = tokio::select! {
                _ = shutdown.cancelled() => return,
                connection = relay_transport::connect_runner(&self.endpoint) => connection,
            };
            if let Ok((mut incoming, mut relay)) = connection {
                self.relay_connected.store(true, Ordering::Release);
                let mut sessions = JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => break,
                        _ = relay.wait() => break,
                        connection = incoming.recv() => {
                            let Some(connection) = connection else { break; };
                            let gateway = self.gateway.clone();
                            sessions.spawn(async move { gateway.serve(connection).await });
                        },
                        Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
                    }
                }
                self.relay_connected.store(false, Ordering::Release);
                drop(relay);
                drop(incoming);
                while sessions.join_next().await.is_some() {}
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    async fn serve_local(
        &self,
        mut stream: UnixStream,
        shutdown: CancellationToken,
    ) -> Result<(), String> {
        // Read exactly the header, without a buffering decoder that could eat
        // pipelined RPC bytes when handing the stream to its permanent target.
        let header = tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(10), async {
                let mut bytes = Vec::with_capacity(64);
                loop {
                    let byte = stream.read_u8().await.map_err(|error| error.to_string())?;
                    if byte == b'\n' { break; }
                    if bytes.len() == 4096 { return Err::<LocalHeader, String>("local header too large".into()); }
                    bytes.push(byte);
                }
                serde_json::from_slice::<LocalHeader>(&bytes).map_err(|_| "invalid local target".into())
            }) => result.map_err(|_| "local header timed out")??,
        };
        stream
            .write_all(b"{\"ready\":true}\n")
            .await
            .map_err(|error| error.to_string())?;
        match header {
            LocalHeader::Local => {
                let session = self.service.open_session(128);
                serve_jsonl_session(stream, self.service.clone(), shutdown, session).await
            }
            LocalHeader::Manager => self.serve_manager(stream, shutdown).await,
            LocalHeader::Remote { profile_id } => {
                let (config, key) = self.remotes.connection(&profile_id).await?;
                mobile_client::forward_rpc(config, &key, stream, shutdown)
                    .await
                    .map_err(|error| error.to_string())
            }
        }
    }

    async fn serve_manager(
        &self,
        stream: UnixStream,
        shutdown: CancellationToken,
    ) -> Result<(), String> {
        let (read, write) = split(stream);
        let mut reader = JsonlReader::with_max_message_bytes(read, 65536);
        let mut writer = JsonlWriter::with_max_message_bytes(write, 65536);
        loop {
            let line = tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                line = reader.read_line() => line.map_err(|error| error.to_string())?,
            };
            let Some(line) = line else {
                return Ok(());
            };
            let request: ManagerRequest =
                serde_json::from_str(&line).map_err(|_| "invalid management request")?;
            let result = self.manage(&request.method, request.params).await;
            let response = match result {
                Ok(result) => json!({"id": request.id, "result": result}),
                Err(message) => {
                    json!({"id": request.id, "error": {"code": -32602, "message": message}})
                }
            };
            let response = response.to_string();
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                result = writer.write_line(&response) => result.map_err(|error| error.to_string())?,
            }
        }
    }

    async fn manage(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "host/transfer" => self.remotes.transfer(params).await,
            "host/listRemotes" => serde_json::to_value(self.remotes.profiles().await)
                .map_err(|error| error.to_string()),
            "host/pairRemote" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Pair {
                    invitation: host_protocol::PairingQrPayload,
                    device_name: String,
                }
                let params: Pair = serde_json::from_value(params)
                    .map_err(|_| "invalid remote pairing parameters")?;
                serde_json::to_value(
                    self.remotes
                        .pair(params.invitation, params.device_name)
                        .await?,
                )
                .map_err(|error| error.to_string())
            }
            "host/removeRemote" => {
                #[derive(Deserialize)]
                struct Remove {
                    id: String,
                }
                let params: Remove =
                    serde_json::from_value(params).map_err(|_| "invalid remote Host ID")?;
                self.remotes.remove(&params.id).await?;
                Ok(json!({}))
            }
            "host/status" => Ok(
                json!({"hostIdentity": self.identity, "hostName": self.host_name, "runnerId": self.endpoint.runner_id, "relayConnected": self.relay_connected.load(Ordering::Acquire), "devices": self.authentication.lock().await.devices()}),
            ),
            "host/invite" => {
                let invitation = self.authentication.lock().await.invite(
                    self.identity,
                    self.host_name.clone(),
                    self.endpoint.clone(),
                    unix_time_millis(),
                )?;
                serde_json::to_value(invitation).map_err(|error| error.to_string())
            }
            "host/revoke" => {
                #[derive(Deserialize)]
                struct Revoke {
                    identity: Ed25519PublicKey,
                }
                let params: Revoke =
                    serde_json::from_value(params).map_err(|_| "invalid device identity")?;
                self.authentication.lock().await.revoke(params.identity)?;
                Ok(json!({}))
            }
            _ => Err("unknown management method".into()),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "target", rename_all = "camelCase", deny_unknown_fields)]
enum LocalHeader {
    Local,
    Manager,
    Remote {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
}

#[derive(Deserialize)]
struct ManagerRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}
