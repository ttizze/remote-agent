mod process;
mod provider;

use agent_peer::expand_path;
use anyhow::{Context, ensure};
use clap::{Command, arg, value_parser};
use serde_json::json;
use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    process::ExitCode,
    time::Duration,
};
use uuid::Uuid;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Command::new("agent-peer")
        .about("Ask Claude Code or Codex and resume their native conversations")
        .subcommand_required(true)
        .subcommands(["ask", "reply"].map(|action| {
            let mut command =
                Command::new(action).arg(arg!(<provider>).value_parser(["claude", "codex"]));
            if action == "reply" {
                command = command.arg(arg!(<session_id>).value_parser(value_parser!(Uuid)));
            }
            command.args([
                arg!(<prompt> "Text, or - for stdin"),
                arg!(--cwd <PATH>).required(true),
                arg!(--model <MODEL>),
                arg!(--timeout <SECONDS>)
                    .default_value("600")
                    .value_parser(value_parser!(f64)),
            ])
        }))
        .get_matches();
    let (_, args) = args.subcommand().expect("required subcommand");
    let provider = args.get_one::<String>("provider").unwrap();
    let session = args
        .try_get_one::<Uuid>("session_id")
        .ok()
        .flatten()
        .map(Uuid::to_string);
    let mut cwd = Path::new(args.get_one::<String>("cwd").unwrap()).to_owned();
    let result = async {
        cwd = std::path::absolute(expand_path(&cwd)?)?;
        ensure!(cwd.is_dir(), "--cwd must be an existing directory");
        cwd = cwd.canonicalize()?;
        let timeout = Duration::try_from_secs_f64(*args.get_one::<f64>("timeout").unwrap())?;
        ensure!(!timeout.is_zero(), "--timeout must be positive");
        let mut prompt = args.get_one::<String>("prompt").unwrap().clone();
        if prompt == "-" {
            ensure!(
                !io::stdin().is_terminal(),
                "Pass a prompt argument or pipe the prompt through stdin"
            );
            prompt.clear();
            io::stdin().read_to_string(&mut prompt)?;
        }
        ensure!(!prompt.trim().is_empty(), "Prompt must not be empty");
        let mut command = provider::command(
            provider,
            session.as_deref(),
            args.get_one::<String>("model").map(String::as_str),
        )?;
        command.current_dir(&cwd);
        let (output, success) = process::invoke(command, &prompt, timeout).await?;
        let mut reply = provider::decode(provider, &output, success)?;
        if !reply["session_id"].is_null() {
            let returned = reply["session_id"]
                .as_str()
                .context("Invalid conversation ID")?;
            Uuid::parse_str(returned)?;
            ensure!(
                session.as_deref().is_none_or(|id| id == returned),
                "Provider returned a different conversation ID"
            );
        } else {
            reply["session_id"] = json!(session);
        }
        ensure!(
            reply["status"] != "ok" || !reply["session_id"].is_null(),
            "Provider returned no conversation ID"
        );
        Ok::<_, anyhow::Error>(reply)
    }
    .await;
    let mut reply = result.unwrap_or_else(|error| {
        json!({
            "session_id": session, "status": "error", "text": "", "errors": [error.to_string()]
        })
    });
    reply["provider"] = json!(provider);
    reply["cwd"] = json!(cwd);
    println!("{reply}");
    ExitCode::from(u8::from(reply["status"] != "ok"))
}
