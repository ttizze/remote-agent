use agent_transport::{
    peer::{JsonlReader, JsonlWriter},
    transport::Identity,
};
use host_fixture::{
    fixture::Config,
    pairing::PairingServer,
    test_support::{HostFixture, Memory},
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::process::Command;
mod codex_fixture;

#[tokio::test]
async fn codex_fixture_uses_its_own_directory_instead_of_inherited_user_configuration() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let fixture = tempfile::tempdir().unwrap();
        let ambient = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let expected = root.clone();
        let program = Config {
            trace: true,
            expected_cwd: Some(expected.clone()),
            stream_delay_ms: 0,
            ..Default::default()
        }
        .install(Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")), &root)
        .unwrap();
        let mut child = Command::new(program)
            .arg("app-server")
            .current_dir(ambient.path())
            .env("CODEX_HOME", ambient.path())
            .env(
                "BEX_FAKE_CODEX_TRACE",
                ambient.path().join("unwanted-trace"),
            )
            .env("BEX_FAKE_CODEX_EXPECTED_CWD", ambient.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut writer = JsonlWriter::new(child.stdin.take().unwrap());
        let mut reader = JsonlReader::new(child.stdout.take().unwrap());
        for (id, method, params) in [
            (
                1,
                "initialize",
                json!({"capabilities":{"experimentalApi":true}}),
            ),
            (2, "thread/start", json!({"cwd":expected})),
            (3, "thread/start", json!({"cwd":ambient.path()})),
            (4, "thread/start", json!({})),
        ] {
            writer
                .write_line(&json!({"id":id,"method":method,"params":params}).to_string())
                .await
                .unwrap();
            let response: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(response["id"], id);
            match id {
                1 => assert_eq!(response["result"]["codexHome"], json!(root)),
                2 | 4 => assert_eq!(response["result"]["thread"]["cwd"], json!(expected)),
                3 => assert_eq!(response["error"]["message"], "invalid cwd"),
                _ => unreachable!(),
            }
        }
        child.kill().await.unwrap();
        assert!(root.join("rpc-trace.jsonl").exists());
        assert_eq!(
            fs::read_dir(ambient.path()).unwrap().count(),
            0,
            "inherited user paths must remain untouched"
        );
    })
    .await
    .expect("isolated Codex fixture exceeded its deadline");
}

#[tokio::test]
async fn pairing_controls_restore_the_original_project_store_and_survive_rejected_host_requests() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let original = b"[]\n";
        fs::write(root.join("projects.json"), original).unwrap();
        let host = HostFixture::start(
            &root,
            codex_fixture::config(&root),
            Arc::new(Memory::default()),
            "isolated Host",
            false,
            None,
        )
        .await
        .unwrap();
        let server =
            PairingServer::start(&root, host.ticket.clone(), Identity::generate()).unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let base = format!("http://127.0.0.1:{}", server.port);
        assert_eq!(
            client
                .get(format!("{base}/pairing"))
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        assert_eq!(
            client
                .post(format!("{base}/title-fixture"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        let threads: Vec<Value> =
            serde_json::from_slice(&fs::read(root.join("list-fixture.json")).unwrap()).unwrap();
        assert!(!threads.is_empty());
        assert!(
            threads
                .iter()
                .all(|thread| Path::new(thread["cwd"].as_str().unwrap()).starts_with(&root))
        );
        assert_eq!(
            client
                .post(format!("{base}/list-fixture/reset"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(fs::read(root.join("projects.json")).unwrap(), original);
        assert!(!root.join("list-fixture.json").exists());
        assert!(!root.join("projects-before-list-fixture.json").exists());
        assert_eq!(
            client
                .post(format!("{base}/list-fixture/reset"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            client
                .get(format!("{base}/unknown"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        server.shutdown().unwrap();
        host.close().await.unwrap();
    })
    .await
    .expect("pairing controls exceeded their deadline");
}
