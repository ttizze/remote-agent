use super::*;

fn rule(key: &str, command: &str) -> KeybindingRule {
    KeybindingRule::new(key, command, None)
}

fn path() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keybindings.json");
    (directory, path)
}

fn persisted(path: &Path) -> Vec<(String, String)> {
    writable_rules(path)
        .unwrap()
        .into_iter()
        .map(|rule| (rule.key, rule.command))
        .collect()
}

fn pairs(rules: &[(&str, &str)]) -> Vec<(String, String)> {
    rules
        .iter()
        .map(|(key, command)| ((*key).to_owned(), (*command).to_owned()))
        .collect()
}

#[test]
fn bootstraps_default_keybindings_when_config_file_is_missing() {
    let (_directory, path) = path();
    sync_defaults(&path).unwrap();
    assert_eq!(writable_rules(&path).unwrap(), default_keybindings());
}

#[test]
fn uses_defaults_in_runtime_when_config_is_malformed_without_overriding_file() {
    let (_directory, path) = path();
    std::fs::write(&path, "{ not-json").unwrap();
    let state = load(&path).unwrap();
    assert_eq!(state.rules, default_keybindings());
    assert!(matches!(
        state.issues.as_slice(),
        [KeybindingIssue::MalformedConfig { .. }]
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not-json");
}

#[test]
fn ignores_invalid_entries_in_runtime_and_reports_them_as_issues() {
    let (_directory, path) = path();
    std::fs::write(
        &path,
        r#"[
            {"key": "mod+j", "command": "terminal.toggle"},
            {"key": "mod+shift+d+o", "command": "terminal.new"},
            {"key": "mod+x", "command": "invalid.command"}
        ]"#,
    )
    .unwrap();
    let state = load(&path).unwrap();
    assert!(
        state
            .rules
            .iter()
            .any(|rule| rule.command == "terminal.toggle")
    );
    assert!(
        !state
            .rules
            .iter()
            .any(|rule| rule.command == "invalid.command")
    );
    let indexes: Vec<u32> = state
        .issues
        .iter()
        .map(|issue| match issue {
            KeybindingIssue::InvalidEntry { index, .. } => *index,
            KeybindingIssue::MalformedConfig { .. } => u32::MAX,
        })
        .collect();
    assert_eq!(indexes, [1, 2]);
}

#[test]
fn reads_comments_and_trailing_commas_in_a_hand_edited_file() {
    let (_directory, path) = path();
    std::fs::write(
        &path,
        "[\n  // the drawer\n  {\"key\": \"mod+j\", \"command\": \"terminal.toggle\",},\n  /* a note, ] */\n]\n",
    )
    .unwrap();
    let state = load(&path).unwrap();
    assert!(state.issues.is_empty(), "{:?}", state.issues);
    assert_eq!(persisted(&path), pairs(&[("mod+j", "terminal.toggle")]));
    assert_eq!(
        strip_lenient(r#"{"note": "a,] // b"}"#),
        r#"{"note": "a,] // b"}"#
    );
}

#[test]
fn upserts_missing_default_keybindings_on_startup_without_overriding_existing_command_rules() {
    let (_directory, path) = path();
    write_rules(
        &path,
        &[
            rule("mod+shift+t", "terminal.toggle"),
            rule("mod+shift+r", "script.run-tests.run"),
        ],
    )
    .unwrap();
    sync_defaults(&path).unwrap();
    let rules = writable_rules(&path).unwrap();
    let toggles: Vec<&str> = rules
        .iter()
        .filter(|rule| rule.command == "terminal.toggle")
        .map(|rule| rule.key.as_str())
        .collect();
    assert_eq!(toggles, ["mod+shift+t"]);
    for default in default_keybindings() {
        assert!(
            rules.iter().any(|rule| rule.command == default.command),
            "expected {}",
            default.command
        );
    }
    assert!(
        rules
            .iter()
            .any(|rule| rule.command == "script.run-tests.run")
    );
}

#[test]
fn skips_conflicting_default_keybindings_on_startup() {
    let (_directory, path) = path();
    write_rules(&path, &[rule("mod+j", "script.custom-action.run")]).unwrap();
    sync_defaults(&path).unwrap();
    let rules = writable_rules(&path).unwrap();
    assert!(!rules.iter().any(|rule| rule.command == "terminal.toggle"));
    assert!(
        rules
            .iter()
            .any(|rule| rule.command == "script.custom-action.run")
    );
}

#[test]
fn skips_the_backfill_while_the_config_has_issues() {
    let (_directory, path) = path();
    std::fs::write(&path, r#"[{"key": "mod+x", "command": "invalid.command"}]"#).unwrap();
    sync_defaults(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"[{"key": "mod+x", "command": "invalid.command"}]"#
    );
}

#[test]
fn upserts_custom_keybindings_to_configured_path() {
    let (_directory, path) = path();
    write_rules(&path, &[rule("mod+j", "terminal.toggle")]).unwrap();
    let resolved = upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+shift+r", "script.run-tests.run"),
            replace: None,
        },
    )
    .unwrap();
    assert_eq!(
        persisted(&path),
        pairs(&[
            ("mod+j", "terminal.toggle"),
            ("mod+shift+r", "script.run-tests.run")
        ])
    );
    assert!(
        resolved
            .rules
            .iter()
            .any(|rule| rule.command == "script.run-tests.run")
    );
}

#[test]
fn appends_additional_custom_keybindings_for_the_same_command() {
    let (_directory, path) = path();
    write_rules(&path, &[rule("mod+r", "script.run-tests.run")]).unwrap();
    upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+shift+r", "script.run-tests.run"),
            replace: None,
        },
    )
    .unwrap();
    assert_eq!(
        persisted(&path),
        pairs(&[
            ("mod+r", "script.run-tests.run"),
            ("mod+shift+r", "script.run-tests.run")
        ])
    );
}

#[test]
fn replaces_only_the_targeted_custom_keybinding() {
    let (_directory, path) = path();
    write_rules(
        &path,
        &[
            rule("mod+r", "script.run-tests.run"),
            rule("mod+shift+r", "script.run-tests.run"),
        ],
    )
    .unwrap();
    upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+alt+r", "script.run-tests.run"),
            replace: Some(rule("mod+r", "script.run-tests.run")),
        },
    )
    .unwrap();
    assert_eq!(
        persisted(&path),
        pairs(&[
            ("mod+shift+r", "script.run-tests.run"),
            ("mod+alt+r", "script.run-tests.run")
        ])
    );
}

#[test]
fn replacing_with_a_rule_that_already_exists_elsewhere_does_not_duplicate_it() {
    let (_directory, path) = path();
    write_rules(
        &path,
        &[
            rule("mod+r", "script.run-tests.run"),
            rule("mod+alt+r", "script.run-tests.run"),
        ],
    )
    .unwrap();
    upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+alt+r", "script.run-tests.run"),
            replace: Some(rule("mod+r", "script.run-tests.run")),
        },
    )
    .unwrap();
    assert_eq!(
        persisted(&path),
        pairs(&[("mod+alt+r", "script.run-tests.run")])
    );
}

#[test]
fn removes_only_the_targeted_custom_keybinding() {
    let (_directory, path) = path();
    write_rules(
        &path,
        &[
            rule("mod+r", "script.run-tests.run"),
            rule("mod+shift+r", "script.run-tests.run"),
        ],
    )
    .unwrap();
    remove(&path, &rule("mod+r", "script.run-tests.run")).unwrap();
    assert_eq!(
        persisted(&path),
        pairs(&[("mod+shift+r", "script.run-tests.run")])
    );
}

#[test]
fn refuses_to_overwrite_malformed_keybindings_config() {
    let (_directory, path) = path();
    std::fs::write(&path, "{ not-json").unwrap();
    let error = upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+shift+r", "script.run-tests.run"),
            replace: None,
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("expected JSON array"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not-json");
}

#[test]
fn reports_non_array_config_parse_errors_without_duplicate_prefix() {
    let (_directory, path) = path();
    std::fs::write(&path, r#"{"key":"mod+j","command":"terminal.toggle"}"#).unwrap();
    for _ in 0..2 {
        let error = upsert(
            &path,
            &UpsertKeybinding {
                rule: rule("mod+shift+r", "script.run-tests.run"),
                replace: None,
            },
        )
        .unwrap_err();
        assert_eq!(format!("{error:#}"), "expected JSON array");
    }
}

#[test]
fn keeps_only_the_newest_rules_at_the_limit() {
    let (_directory, path) = path();
    let fillers: Vec<KeybindingRule> = (0..MAX_KEYBINDINGS)
        .map(|index| rule("mod+alt+f1", &format!("script.filler-{index}.run")))
        .collect();
    write_rules(&path, &fillers).unwrap();
    upsert(
        &path,
        &UpsertKeybinding {
            rule: rule("mod+shift+r", "script.run-tests.run"),
            replace: None,
        },
    )
    .unwrap();
    let rules = writable_rules(&path).unwrap();
    assert_eq!(rules.len(), MAX_KEYBINDINGS);
    assert_eq!(rules[0].command, "script.filler-1.run");
    assert_eq!(rules.last().unwrap().command, "script.run-tests.run");
}

#[tokio::test]
async fn serializes_concurrent_upserts_to_avoid_lost_updates() {
    let (_directory, path) = path();
    write_rules(&path, &[]).unwrap();
    let keybindings = Arc::new(Keybindings::new(path.clone()));
    let commands: Vec<String> = (0..20)
        .map(|index| format!("script.concurrent-{index}.run"))
        .collect();
    let tasks: Vec<_> = commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let keybindings = keybindings.clone();
            let rule = rule(&format!("mod+{}", char::from(b'a' + index as u8)), command);
            tokio::spawn(async move {
                keybindings
                    .upsert(UpsertKeybinding {
                        rule,
                        replace: None,
                    })
                    .await
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let rules = writable_rules(&path).unwrap();
    for command in &commands {
        assert!(
            rules.iter().any(|rule| &rule.command == command),
            "expected persisted command {command}"
        );
    }
}

#[tokio::test]
async fn subscribers_hear_each_change() {
    let (_directory, path) = path();
    let keybindings = Arc::new(Keybindings::new(path.clone()));
    keybindings.start().await.unwrap();
    let mut changes = keybindings.subscribe();
    keybindings
        .upsert(UpsertKeybinding {
            rule: rule("mod+shift+t", "terminal.toggle"),
            replace: None,
        })
        .await
        .unwrap();
    let config = changes.recv().await.unwrap();
    assert!(
        config
            .rules
            .iter()
            .any(|rule| rule.key == "mod+shift+t" && rule.command == "terminal.toggle")
    );
}
