//! Registry changes exercise the real app-server launcher with private fixtures.
use agent_protocol::{
    operations::{ListModels, ModelPage},
    protocol::{Call, Response},
    providers::*,
};
use host_daemon::{HostRpcService, ProjectStore};
use serde_json::json;
use std::path::Path;

async fn call<T: serde::de::DeserializeOwned>(
    service: &HostRpcService,
    request: Call,
) -> Result<T, agent_protocol::error::RpcFailure> {
    let session = service.open_session();
    let reply = service
        .dispatch(session.id(), &request)
        .await
        .unwrap()
        .initial;
    match agent_protocol::protocol::decode::<Response<T>>(&reply).unwrap() {
        Response::Success { result } => Ok(result),
        Response::Failure { error } => Err(error),
    }
}

async fn update(
    service: &HostRpcService,
    revision: u64,
    mutation: ProviderMutation,
) -> ProviderSettings {
    call(
        service,
        Call::UpdateProviderInstance(UpdateProviderInstance {
            operation_id: uuid::Uuid::new_v4(),
            revision,
            mutation,
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn configured_launcher_pagination_and_inflight_models_remain_owned_by_the_instance() {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let shared = root.path().join("shared");
        let shadow = root.path().join("shadow");
        let fixture = host_fixture::fixture::Config {
            stream_delay_ms: 0,
            expected_launch_args: vec!["--fixture-label".into(), "two words".into()],
            expected_environment: [
                ("PATH".into(), bin.to_string_lossy().into()),
                ("CODEX_HOME".into(), shadow.to_string_lossy().into()),
            ]
            .into(),
            ..Default::default()
        }
        .install(Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")), &bin)
        .unwrap();
        let command = format!(
            "isolated-native-{}{}",
            uuid::Uuid::new_v4(),
            std::env::consts::EXE_SUFFIX
        );
        std::fs::rename(fixture, bin.join(&command)).unwrap();
        let model = |id: &str| {
            json!({
                "id":id, "model":id, "displayName":id,
                "defaultReasoningEffort":"", "supportedReasoningEfforts":[]
            })
        };
        std::fs::write(
            bin.join("model-pages.json"),
            serde_json::to_vec(&json!([[model("first")], [model("second")]])).unwrap(),
        )
        .unwrap();
        let service =
            HostRpcService::new(ProjectStore::new(root.path().join("projects.json"))).unwrap();
        let id: ProviderInstanceId = "work".parse().unwrap();
        let config = ProviderConfig {
            driver: "codex".parse().unwrap(),
            display_name: Some("Work".into()),
            accent_color: None,
            enabled: true,
            environment: [
                ("PATH", bin.to_string_lossy().into_owned()),
                (
                    "CODEX_HOME",
                    root.path()
                        .join("overridden-home")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ]
            .into_iter()
            .map(|(name, value)| EnvironmentVariable {
                name: name.into(),
                value,
                sensitive: false,
                value_redacted: false,
            })
            .collect(),
            config: json!({
                "binaryPath":command, "homePath":shared, "shadowHomePath":shadow,
                "launchArgs":"--fixture-label 'two words'"
            }),
        };
        let saved = update(
            &service,
            0,
            ProviderMutation::Create {
                instance_id: id.clone(),
                config,
            },
        )
        .await;
        assert_eq!(saved.revision, 1);
        // Persisted settings load before startup defaults and keep this private instance.
        service
            .configure_providers(
                root.path().to_owned(),
                root.path().join("missing-codex"),
                Some(shared),
                root.path().join("missing-claude"),
                Some(root.path().join("claude")),
            )
            .await
            .unwrap();
        let query = |cursor| Call::ListModels(ListModels { limit: 1, cursor });
        let first: ModelPage = call(&service, query(None)).await.unwrap();
        assert_eq!(first.instances.len(), 1);
        assert_eq!(first.instances[0].availability, ProviderAvailability::Ready);
        assert_eq!(
            first
                .data
                .iter()
                .map(|model| model.model.id.as_str())
                .collect::<Vec<_>>(),
            ["first"]
        );
        assert_eq!(first.data[0].model.instance_id, id);
        let cursor = first.next_cursor.unwrap();
        let second: ModelPage = call(&service, query(Some(cursor.clone()))).await.unwrap();
        assert_eq!(
            second
                .data
                .iter()
                .map(|model| model.model.id.as_str())
                .collect::<Vec<_>>(),
            ["second"]
        );
        assert!(second.next_cursor.is_none());
        std::fs::write(bin.join("models-wait"), []).unwrap();
        let pending_service = service.clone();
        let pending = tokio::spawn(async move {
            call::<ModelPage>(
                &pending_service,
                Call::ListModels(ListModels {
                    limit: 1,
                    cursor: None,
                }),
            )
            .await
        });
        while !bin.join("models-release.entered").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        update(&service, 1, ProviderMutation::Remove { instance_id: id }).await;
        assert_eq!(
            pending.await.unwrap().unwrap_err().code,
            "provider_catalog_changed"
        );
        assert_eq!(
            call::<ModelPage>(&service, query(Some(cursor)))
                .await
                .unwrap_err()
                .code,
            "provider_catalog_changed"
        );
    })
    .await
    .unwrap();
}
