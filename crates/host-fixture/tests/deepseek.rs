//! The DSH fixture behind the Host's real DeepSeek adapter: its catalog follows
//! the keys the Host saves, as DSH's does.
use agent_protocol::{
    dsh::{KeyProvider, KeyUpdate},
    operations as op,
    session::ProviderKind,
};
use host_fixture::test_support::{HostFixture, Memory};
use std::{path::Path, sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_openrouter_key_adds_flash_through_openrouter_to_the_catalog() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let host = HostFixture::start(
            &root,
            codex_app_server::AppServerConfig {
                program: root.join("missing-codex"),
                ..Default::default()
            },
            Arc::new(Memory::default()),
            "DeepSeek fixture Host",
            false,
            None,
            Some(Path::new(env!("CARGO_BIN_EXE_agent-dsh-fixture"))),
        )
        .await
        .unwrap();
        let local = host.local().await.unwrap();
        let models = async || {
            local
                .peer
                .call(&op::ListModels {
                    provider: ProviderKind::DeepSeek,
                    cursor: None,
                    limit: 100,
                })
                .await
                .unwrap()
                .data
                .into_iter()
                .map(|model| (model.display_name, model.credential_route))
                .collect::<Vec<_>>()
        };
        assert!(
            models().await.is_empty(),
            "unconfigured routes are not offered"
        );
        local
            .peer
            .call(&KeyUpdate {
                provider: KeyProvider::DeepSeek,
                key: Some("deepseek-fixture".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            models().await,
            [(
                "DeepSeek公式 · DeepSeek V4 Flash".to_owned(),
                Some(KeyProvider::DeepSeek)
            )]
        );
        local
            .peer
            .call(&KeyUpdate {
                provider: KeyProvider::OpenRouter,
                key: Some("sk-or-fixture".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            models().await,
            [
                (
                    "DeepSeek公式 · DeepSeek V4 Flash".to_owned(),
                    Some(KeyProvider::DeepSeek)
                ),
                (
                    "OpenRouter · DeepSeek: DeepSeek V4.1 Flash".to_owned(),
                    Some(KeyProvider::OpenRouter)
                ),
            ]
        );
        local.close().await;
        host.close().await.unwrap();
    })
    .await
    .expect("DeepSeek fixture exceeded its deadline");
}
