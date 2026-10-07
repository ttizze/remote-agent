use super::*;

fn shortcut(key: &str, mod_key: bool) -> Shortcut {
    Shortcut {
        key: key.into(),
        meta: false,
        ctrl: false,
        shift: false,
        alt: false,
        mod_key,
    }
}

fn id(name: &str) -> Box<When> {
    Box::new(When::Identifier(name.into()))
}

#[test]
fn parses_shortcuts_including_plus_key() {
    assert_eq!(parse_shortcut("mod+j"), Some(shortcut("j", true)));
    assert_eq!(parse_shortcut("mod++"), Some(shortcut("+", true)));
    assert_eq!(
        parse_shortcut("Ctrl+Shift+ArrowUp"),
        Some(Shortcut {
            key: "arrowup".into(),
            ctrl: true,
            shift: true,
            ..shortcut("", false)
        })
    );
    assert_eq!(parse_shortcut("esc").unwrap().key, "escape");
    assert_eq!(parse_shortcut("space").unwrap().key, " ");
}

#[test]
fn compiles_valid_rule_with_parsed_when_ast() {
    let compiled = compile_rule(&KeybindingRule::new(
        "mod+d",
        "terminal.split",
        Some("terminalOpen && !terminalFocus"),
    ))
    .unwrap();
    assert_eq!(
        compiled,
        CompiledRule {
            command: "terminal.split".into(),
            shortcut: shortcut("d", true),
            when: Some(When::And(
                id("terminalOpen"),
                Box::new(When::Not(id("terminalFocus")))
            )),
        }
    );
}

#[test]
fn encodes_resolved_plus_key_shortcuts() {
    assert_eq!(shortcut("+", true).encode().as_deref(), Some("mod++"));
    assert_eq!(shortcut(" ", false).encode().as_deref(), Some("space"));
    assert_eq!(shortcut("a+b", false).encode(), None);
}

#[test]
fn rejects_invalid_rules() {
    assert!(compile_rule(&KeybindingRule::new("mod+shift+d+o", "terminal.new", None)).is_err());
    assert!(
        compile_rule(&KeybindingRule::new(
            "mod+d",
            "terminal.split",
            Some("terminalFocus && (")
        ))
        .is_err()
    );
    let deep = format!("{}terminalFocus", "!".repeat(300));
    assert!(compile_rule(&KeybindingRule::new("mod+d", "terminal.split", Some(&deep))).is_err());
    assert!(compile_rule(&KeybindingRule::new("mod+x", "invalid.command", None)).is_err());
}

#[test]
fn formats_invalid_rules_with_the_custom_message() {
    let error =
        compile_rule(&KeybindingRule::new("mod+shift+d+o", "terminal.new", None)).unwrap_err();
    assert!(error.contains("Invalid keybinding rule"), "{error}");
}

#[test]
fn when_expressions_keep_precedence_and_print_minimal_parentheses() {
    let parsed = parse_when("a || b && !(c || d)").unwrap();
    assert_eq!(parsed.expression(), "a || (b && !(c || d))");
    let truth = |name: &str| name == "b";
    assert!(parsed.evaluate(&truth));
    assert!(!parse_when("b && false").unwrap().evaluate(&truth));
    assert!(parse_when("true").unwrap().evaluate(&|_| false));
    assert_eq!(parse_when(""), None);
    assert_eq!(parse_when("a &&"), None);
    assert_eq!(parse_when("a $ b"), None);
}

#[test]
fn script_commands_take_lowercase_ids() {
    assert!(is_keybinding_command("script.run-tests.run"));
    assert!(!is_keybinding_command("script.Run.run"));
    assert!(!is_keybinding_command("script..run"));
    assert!(!is_keybinding_command(&format!(
        "script.{}.run",
        "a".repeat(25)
    )));
}

#[test]
fn every_default_rule_compiles() {
    for rule in default_keybindings() {
        assert!(compile_rule(&rule).is_ok(), "{rule:?}");
    }
}

#[test]
fn file_rules_replace_the_defaults_of_their_commands() {
    let custom = [KeybindingRule::new("mod+shift+t", "terminal.toggle", None)];
    let merged = merge_with_defaults(&custom);
    assert_eq!(merged.last(), custom.last());
    assert!(
        !merged
            .iter()
            .any(|rule| rule.command == "terminal.toggle" && rule.key == "mod+j")
    );
    assert_eq!(merge_with_defaults(&[]), default_keybindings());
}

#[test]
fn shortcut_contexts_compare_parsed_keys_and_conditions() {
    let left = KeybindingRule::new("Mod+J", "terminal.toggle", None);
    let right = KeybindingRule::new("mod+j", "script.custom-action.run", None);
    assert!(same_shortcut_context(&left, &right));
    let focused = KeybindingRule::new("mod+j", "terminal.toggle", Some("terminalFocus"));
    assert!(!same_shortcut_context(&left, &focused));
}
