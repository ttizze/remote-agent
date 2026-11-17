use codex_app_server::AppServerConfig;
use std::path::Path;

pub(crate) fn config(directory: &Path) -> AppServerConfig {
    AppServerConfig {
        program: host_fixture::fixture::Config {
            stream_delay_ms: 5,
            ..Default::default()
        }
        .install(
            Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")),
            directory,
        )
        .unwrap(),
        ..Default::default()
    }
}
