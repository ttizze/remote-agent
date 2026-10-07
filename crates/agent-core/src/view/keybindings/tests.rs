use super::*;

fn press(key: &str, meta: bool, ctrl: bool, shift: bool, alt: bool) -> KeyPress {
    KeyPress {
        key: key.into(),
        meta,
        ctrl,
        shift,
        alt,
    }
}

fn cmd(key: &str) -> KeyPress {
    press(key, true, false, false, false)
}

fn rule(key: &str, command: &str, when: Option<&str>) -> KeybindingRule {
    KeybindingRule::new(key, command, when)
}

fn defaults(mac: bool) -> Keymap {
    Keymap::new(&default_keybindings(), mac)
}

fn terminal() -> KeyContext {
    KeyContext {
        terminal_focus: true,
        terminal_open: true,
        ..KeyContext::default()
    }
}

#[test]
fn matches_cmd_j_on_macos() {
    let keymap = defaults(true);
    assert_eq!(
        keymap.resolve(&cmd("j"), &KeyContext::default()).as_deref(),
        Some("terminal.toggle")
    );
    assert_eq!(
        keymap.resolve(
            &press("j", false, true, false, false),
            &KeyContext::default()
        ),
        None
    );
}

#[test]
fn matches_ctrl_j_on_non_macos() {
    let keymap = defaults(false);
    let ctrl_j = press("j", false, true, false, false);
    assert_eq!(
        keymap.resolve(&ctrl_j, &KeyContext::default()).as_deref(),
        Some("terminal.toggle")
    );
    assert_eq!(
        keymap.resolve(&ctrl_j, &terminal()).as_deref(),
        Some("terminal.toggle")
    );
}

#[test]
fn requires_terminal_focus_for_default_split_new_close_bindings() {
    let keymap = defaults(true);
    let outside = KeyContext {
        terminal_open: true,
        ..KeyContext::default()
    };
    assert_eq!(
        keymap.resolve(&cmd("d"), &outside).as_deref(),
        Some("diff.toggle")
    );
    assert_eq!(
        keymap.resolve(&cmd("n"), &outside).as_deref(),
        Some("chat.new")
    );
    assert_eq!(
        keymap.resolve(&cmd("w"), &outside).as_deref(),
        Some("rightPanel.close")
    );
    assert_eq!(
        keymap.resolve(&press("d", true, false, true, false), &outside),
        None
    );
}

#[test]
fn matches_split_new_when_terminal_focus_is_true() {
    let keymap = defaults(true);
    assert_eq!(
        keymap.resolve(&cmd("d"), &terminal()).as_deref(),
        Some("terminal.split")
    );
    assert_eq!(
        keymap
            .resolve(&press("d", true, false, true, false), &terminal())
            .as_deref(),
        Some("terminal.splitVertical")
    );
    assert_eq!(
        keymap.resolve(&cmd("n"), &terminal()).as_deref(),
        Some("terminal.new")
    );
}

#[test]
fn routes_mod_w_to_the_terminal_while_focused_and_to_the_right_panel_otherwise() {
    let keymap = defaults(true);
    assert_eq!(
        keymap.resolve(&cmd("w"), &terminal()).as_deref(),
        Some("terminal.close")
    );
    assert_eq!(
        keymap.resolve(&cmd("w"), &KeyContext::default()).as_deref(),
        Some("rightPanel.close")
    );
}

#[test]
fn supports_when_expressions() {
    let keymap = Keymap::new(
        &[
            rule(
                "mod+k",
                "terminal.toggle",
                Some("terminalOpen && !terminalFocus"),
            ),
            rule(
                "mod+l",
                "diff.toggle",
                Some("composerFocus || (terminalOpen && !terminalFocus)"),
            ),
        ],
        true,
    );
    let open = KeyContext {
        terminal_open: true,
        ..KeyContext::default()
    };
    assert_eq!(
        keymap.resolve(&cmd("k"), &open).as_deref(),
        Some("terminal.toggle")
    );
    assert_eq!(keymap.resolve(&cmd("k"), &terminal()), None);
    assert_eq!(
        keymap.resolve(&cmd("l"), &open).as_deref(),
        Some("diff.toggle")
    );
    let composer = KeyContext {
        composer_focus: true,
        ..KeyContext::default()
    };
    assert_eq!(
        keymap.resolve(&cmd("l"), &composer).as_deref(),
        Some("diff.toggle")
    );
    assert_eq!(keymap.resolve(&cmd("l"), &terminal()), None);
}

#[test]
fn supports_when_boolean_literals() {
    let keymap = Keymap::new(
        &[
            rule("mod+k", "terminal.toggle", Some("true")),
            rule("mod+l", "diff.toggle", Some("false")),
        ],
        true,
    );
    assert_eq!(
        keymap.resolve(&cmd("k"), &KeyContext::default()).as_deref(),
        Some("terminal.toggle")
    );
    assert_eq!(keymap.resolve(&cmd("l"), &KeyContext::default()), None);
}

#[test]
fn uses_when_and_order_so_a_later_focused_rule_overrides_a_global_rule() {
    let keymap = Keymap::new(
        &[
            rule("mod+n", "chat.new", None),
            rule("mod+n", "terminal.new", Some("terminalFocus")),
        ],
        true,
    );
    assert_eq!(
        keymap.resolve(&cmd("n"), &terminal()).as_deref(),
        Some("terminal.new")
    );
    assert_eq!(
        keymap.resolve(&cmd("n"), &KeyContext::default()).as_deref(),
        Some("chat.new")
    );
}

#[test]
fn still_lets_a_later_global_rule_win_when_both_rules_match() {
    let keymap = Keymap::new(
        &[
            rule("mod+n", "terminal.new", Some("terminalFocus")),
            rule("mod+n", "chat.new", None),
        ],
        true,
    );
    assert_eq!(
        keymap.resolve(&cmd("n"), &terminal()).as_deref(),
        Some("chat.new")
    );
}

#[test]
fn returns_the_effective_binding_label() {
    assert_eq!(
        defaults(true)
            .shortcut_label("terminal.toggle", &KeyContext::default())
            .as_deref(),
        Some("\u{2318}J")
    );
    assert_eq!(
        defaults(false)
            .shortcut_label("terminal.toggle", &KeyContext::default())
            .as_deref(),
        Some("Ctrl+J")
    );
    assert_eq!(
        defaults(true)
            .shortcut_label("modelPicker.toggle", &KeyContext::default())
            .as_deref(),
        Some("\u{21e7}\u{2318}M")
    );
}

#[test]
fn returns_none_for_commands_shadowed_by_a_later_conflicting_shortcut() {
    let keymap = Keymap::new(
        &[
            rule("mod+j", "terminal.toggle", None),
            rule("mod+j", "diff.toggle", None),
        ],
        true,
    );
    assert_eq!(
        keymap.shortcut_label("terminal.toggle", &KeyContext::default()),
        None
    );
    assert_eq!(
        keymap
            .shortcut_label("diff.toggle", &KeyContext::default())
            .as_deref(),
        Some("\u{2318}J")
    );
}

#[test]
fn respects_when_context_while_resolving_labels() {
    let keymap = defaults(true);
    assert_eq!(
        keymap
            .shortcut_label("terminal.split", &terminal())
            .as_deref(),
        Some("\u{2318}D")
    );
    assert_eq!(
        keymap.shortcut_label("terminal.split", &KeyContext::default()),
        None
    );
    assert_eq!(
        keymap
            .shortcut_label("diff.toggle", &KeyContext::default())
            .as_deref(),
        Some("\u{2318}D")
    );
}

#[test]
fn maps_jump_commands_to_visible_model_indices() {
    assert_eq!(model_jump_index("modelPicker.jump.1"), Some(0));
    assert_eq!(model_jump_index("modelPicker.jump.9"), Some(8));
    assert_eq!(model_jump_index("thread.jump.1"), None);
    assert_eq!(thread_jump_index("thread.jump.3"), Some(2));
    let keymap = defaults(true);
    assert_eq!(
        keymap
            .model_jump_label(0, &KeyContext::default())
            .as_deref(),
        Some("\u{2318}1")
    );
    let picker = KeyContext {
        model_picker_open: true,
        ..KeyContext::default()
    };
    assert_eq!(
        keymap.resolve(&cmd("1"), &picker).as_deref(),
        Some("modelPicker.jump.1")
    );
    assert_eq!(
        keymap.resolve(&cmd("1"), &KeyContext::default()).as_deref(),
        Some("thread.jump.1")
    );
    assert_eq!(
        keymap
            .resolve(&press("arrowdown", true, false, true, false), &picker)
            .as_deref(),
        Some("modelPicker.nextProvider")
    );
}

#[test]
fn formats_labels_for_macos_non_macos_and_the_plus_key() {
    let shortcut = agent_protocol::keybindings::parse_shortcut("mod+shift+arrowup").unwrap();
    assert_eq!(shortcut_label(&shortcut, true), "\u{21e7}\u{2318}Up");
    assert_eq!(shortcut_label(&shortcut, false), "Ctrl+Shift+Up");
    let plus = agent_protocol::keybindings::parse_shortcut("mod++").unwrap();
    assert_eq!(shortcut_label(&plus, true), "\u{2318}+");
    assert_eq!(shortcut_label(&plus, false), "Ctrl++");
}

#[test]
fn key_caps_follow_the_platform() {
    assert_eq!(
        key_caps("mod+shift+arrowup", true),
        ["\u{21e7}", "\u{2318}", "\u{2191}"]
    );
    assert_eq!(
        key_caps("mod+shift+arrowup", false),
        ["Ctrl", "Shift", "\u{2191}"]
    );
}

#[test]
fn lists_queue_and_provider_commands_with_editable_defaults() {
    let rows = keybinding_rows(&default_keybindings(), "");
    for command in [
        "thread.steerQueuedMessage",
        "thread.editQueuedMessage",
        "modelPicker.previousProvider",
        "modelPicker.nextProvider",
    ] {
        let row = rows.iter().find(|row| row.command == command).unwrap();
        assert_eq!(row.source, KeybindingSource::Default, "{command}");
        assert!(row.conflicts.is_empty(), "{command}");
    }
}

#[test]
fn finds_the_editable_shortcut_for_sending_the_first_queued_message() {
    let rows = keybinding_rows(&default_keybindings(), "first queued");
    assert!(
        rows.iter()
            .any(|row| row.command == "thread.steerQueuedMessage" && row.key == "mod+shift+enter")
    );
}

#[test]
fn builds_searchable_rows_with_readable_key_and_when_values() {
    let rows = keybinding_rows(
        &[rule("mod+j", "terminal.toggle", Some("!terminalFocus"))],
        "terminal",
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.key, "mod+j");
    assert_eq!(row.when, "!terminalFocus");
    assert_eq!(row.default_key.as_deref(), Some("mod+j"));
    assert_eq!(row.default_when, "");
    assert_eq!(row.source, KeybindingSource::Custom);
    assert!(row.can_reset() && row.can_remove());
}

#[test]
fn captures_platform_specific_mod_shortcuts() {
    assert_eq!(
        keybinding_from_press(&press("K", true, false, true, false), true).as_deref(),
        Some("mod+shift+k")
    );
    assert_eq!(
        keybinding_from_press(&press("K", false, true, true, false), false).as_deref(),
        Some("mod+shift+k")
    );
    for (key, expected) in [("k", "k"), ("Tab", "tab"), ("F5", "f5")] {
        assert_eq!(
            keybinding_from_press(&press(key, false, false, false, false), true).as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn waits_for_a_key_when_only_a_modifier_is_pressed() {
    assert_eq!(
        keybinding_from_press(&press("Meta", true, false, false, false), true),
        None
    );
}

#[test]
fn serializes_when_expressions_and_rejects_unparseable_drafts() {
    assert_eq!(
        parse_when("editorFocus && !terminalFocus")
            .unwrap()
            .expression(),
        "editorFocus && !terminalFocus"
    );
    assert_eq!(
        when_expression_error("editorFocus && (!terminalFocus || modelPickerOpen)"),
        None
    );
    assert_eq!(
        when_expression_error("editorFocus &&").as_deref(),
        Some("Use variables with !, &&, ||, and parentheses.")
    );
    assert_eq!(when_expression_error(""), None);
}

#[test]
fn formats_static_and_project_script_command_labels() {
    assert_eq!(
        command_label("terminal.splitVertical"),
        "Terminal: Split Vertical"
    );
    assert_eq!(command_label("modelPicker.toggle"), "Model Picker: Toggle");
    assert_eq!(command_label("script.setup-db.run"), "Run Script: Setup Db");
    assert_eq!(command_label("thread.jump.3"), "Thread: Jump: 3");
}

#[test]
fn builds_command_options_from_all_static_commands_and_resolved_project_bindings() {
    let options = keybinding_command_options(&[rule("mod+r", "script.setup-db.run", None)]);
    for command in ["chat.new", "modelPicker.jump.1", "script.setup-db.run"] {
        assert!(options.iter().any(|option| option == command), "{command}");
    }
}

#[test]
fn marks_each_default_shortcut_for_multi_binding_commands_as_default() {
    let rows = keybinding_rows(
        &[
            rule("mod+n", "chat.new", Some("!terminalFocus")),
            rule("mod+shift+o", "chat.new", Some("!terminalFocus")),
        ],
        "",
    );
    let sources: Vec<KeybindingSource> = rows.iter().map(|row| row.source).collect();
    assert_eq!(
        sources,
        [KeybindingSource::Default, KeybindingSource::Default]
    );
}

#[test]
fn reports_conflicting_shortcuts_that_share_an_active_when_context() {
    let rows = keybinding_rows(
        &[
            rule("mod+n", "chat.new", Some("!terminalFocus")),
            rule("mod+n", "diff.toggle", Some("!terminalFocus")),
        ],
        "",
    );
    assert_eq!(rows[0].conflicts, ["Diff: Toggle"]);
    assert_eq!(
        keybinding_conflict_labels(&rows, &rows[0].id, "mod+n", ""),
        ["Diff: Toggle"]
    );
}

#[test]
fn the_snapshot_keymap_uses_the_hosts_rules_once_they_arrive() {
    let mut snapshot = Snapshot::default();
    assert_eq!(
        snapshot
            .keymap(true)
            .resolve(&cmd("j"), &KeyContext::default())
            .as_deref(),
        Some("terminal.toggle")
    );
    snapshot.keybindings = Some(std::sync::Arc::new(
        agent_protocol::keybindings::KeybindingsConfig {
            rules: vec![rule("mod+j", "diff.toggle", None)],
            ..Default::default()
        },
    ));
    assert_eq!(
        snapshot
            .keymap(true)
            .resolve(&cmd("j"), &KeyContext::default())
            .as_deref(),
        Some("diff.toggle")
    );
}
