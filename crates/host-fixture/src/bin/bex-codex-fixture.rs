#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match host_fixture::fixture::run(&std::env::args().skip(1).collect::<Vec<_>>()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Codex fixture failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
