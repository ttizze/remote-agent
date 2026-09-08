use crate::command;
use tokio::process::Command;
use xtask::Result;

pub async fn run(language: Option<&str>) -> Result<()> {
    const LANGUAGES: [&str; 4] = ["rust", "elixir", "kotlin", "swift"];
    if language.is_some_and(|name| !LANGUAGES.contains(&name)) {
        return Err("quality expects rust, elixir, kotlin, or swift".into());
    }
    let mut failed = false;
    for name in LANGUAGES {
        if language.is_some_and(|selected| selected != name) {
            continue;
        }
        eprintln!("Checking {name}");
        if let Err(error) = check(name).await {
            eprintln!("{name}: {error}");
            failed = true;
        }
    }
    if failed {
        Err("quality checks failed".into())
    } else {
        Ok(())
    }
}

async fn check(language: &str) -> Result<()> {
    match language {
        "rust" => {
            let format = command::run(command::cargo().args(["fmt", "--all", "--check"])).await;
            let lint = command::run(command::cargo().args([
                "clippy",
                "--locked",
                "--workspace",
                "--all-targets",
                "--",
                "--no-deps",
                "-D",
                "warnings",
            ]))
            .await;
            format.and(lint)
        }
        "elixir" => {
            command::run(
                Command::new("mix")
                    .arg("quality")
                    .current_dir("apps/server"),
            )
            .await
        }
        "kotlin" => {
            command::run(Command::new("./gradlew").args([
                ":apps:mobile:ktfmtCheck",
                ":apps:mobile:detekt",
                "--continue",
                "--console=plain",
            ]))
            .await
        }
        "swift" => {
            let format = command::run(Command::new("swiftformat").args([
                "--lint",
                "apps/mobile/iosApp/Bex",
                "apps/mobile/iosApp/BexUITests",
                "apps/desktop/macos",
            ]))
            .await;
            let lint = command::run(Command::new("swiftlint").args(["lint", "--strict"])).await;
            format.and(lint)
        }
        _ => unreachable!("validated language"),
    }
}
