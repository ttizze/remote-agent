use super::*;
use crate::providers::Configuration;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn title_cli_uses_selected_credentials_without_creating_a_native_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("claude");
    std::fs::write(&program, r#"#!/usr/bin/env node
const args = process.argv.slice(2);
const value = flag => args[args.indexOf(flag) + 1];
if (!args.includes('-p') || value('--tools') !== '' || !args.includes('--strict-mcp-config')) process.exit(2);
if (value('--model') !== 'claude-haiku-4-5' || !args.includes('--no-session-persistence')) process.exit(3);
if (!process.env.CLAUDE_CONFIG_DIR.endsWith('native-home') || !process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR.endsWith('native-home')) process.exit(4);
if (JSON.parse(value('--settings')).disableAllHooks !== true || args.at(-1) !== '日本語の依頼') process.exit(5);
if (JSON.parse(value('--json-schema')).required[0] !== 'title') process.exit(6);
setTimeout(() => { console.log(JSON.stringify({type:'result',structured_output:{title:'内容からタスク名を生成'}})); }, 400);
"#).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let claude = Claude::load(
        program,
        directory.path().join("state"),
        Some(directory.path().join("native-home")),
    )
    .await
    .unwrap();
    assert_eq!(
        claude
            .generate_title("日本語の依頼")
            .await
            .unwrap()
            .as_deref(),
        Some("内容からタスク名を生成")
    );
    assert!(claude.records.lock().await.is_empty());
}
