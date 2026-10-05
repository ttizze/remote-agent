use agent_protocol::{models::Invitation, operations::Pair};
use agent_transport::transport::{Endpoint, Identity, Relays};
use serde_json::Value;
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_host-daemon"));
    command.args(["--isolated", "--state-dir"]);
    command.arg(directory);
    command.kill_on_drop(true);
    command
}

#[tokio::test]
async fn headless_invitation_pairs_once_and_status_uses_the_running_host() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        let stopped = command(&state).arg("invite").output().await.unwrap();
        assert!(!stopped.status.success());
        assert!(String::from_utf8_lossy(&stopped.stderr).contains("Host is not running"));
        assert!(!state.join("identity.keys").exists());

        let mut host = command(&state)
            .args(["--no-relay", "--name", "Linux fixture"])
            .arg("--codex")
            .arg(directory.path().join("missing-codex"))
            .arg("--claude")
            .arg(directory.path().join("missing-claude"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        while !state.join("host.ticket").exists() {
            assert!(
                host.try_wait().unwrap().is_none(),
                "Host exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let original_keys = std::fs::read(state.join("identity.keys")).unwrap();
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let output = command(&state).arg("invite").output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let invitation: Invitation = serde_json::from_slice(&output.stdout).unwrap();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((before + 604_800..=after + 604_800).contains(&invitation.expires_at));
        let ticket = invitation.endpoint.parse().unwrap();
        let phone = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let session = phone.connect(&ticket).await.unwrap();
        let (peer, _events) = session.open_peer(Duration::from_secs(5), 8).await.unwrap();
        peer.call(&Pair {
            invitation: invitation.invitation,
        })
        .await
        .unwrap();

        assert_eq!(
            peer.request::<String>(&agent_protocol::protocol::Call::HostName(
                agent_protocol::models::Empty {}
            ))
            .await
            .unwrap(),
            "Linux fixture"
        );
        assert!(
            peer.call(&agent_protocol::operations::ReadHostStatus {})
                .await
                .is_err()
        );

        let output = command(&state).arg("status").output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["name"], "Linux fixture");
        assert!(
            status["devices"]
                .as_array()
                .unwrap()
                .contains(&Value::String(phone.node_id().to_string()))
        );
        assert_eq!(
            std::fs::read(state.join("identity.keys")).unwrap(),
            original_keys
        );

        let stranger = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let rejected = stranger.connect(&ticket).await.unwrap();
        let (other, _events) = rejected.open_peer(Duration::from_secs(5), 8).await.unwrap();
        assert!(
            other
                .call(&Pair {
                    invitation: invitation.invitation
                })
                .await
                .is_err()
        );
        stranger.close().await;
        let revoked = command(&state)
            .args(["revoke", &phone.node_id().to_string()])
            .output()
            .await
            .unwrap();
        assert!(revoked.status.success());
        assert!(
            peer.call(&Pair {
                invitation: invitation.invitation
            })
            .await
            .is_err()
        );
        let output = command(&state).arg("status").output().await.unwrap();
        assert!(output.status.success());
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["devices"].as_array().unwrap().len(), 1);
        peer.close().await;
        phone.close().await;
        host.kill().await.unwrap();

        let stopped = command(&state).arg("status").output().await.unwrap();
        assert!(!stopped.status.success());
        assert!(String::from_utf8_lossy(&stopped.stderr).contains("Host is not running"));
    })
    .await
    .expect("bounded headless pairing test");
}
