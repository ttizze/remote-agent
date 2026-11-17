use agent_protocol::attribution::Author;

/// Human messages use the ordinary body fold; authenticated agent, automation
/// and system messages have their own presentation. Names do not classify them.
pub fn user_message_body_folding_enabled(author: Option<&Author>, has_body: bool) -> bool {
    has_body && author.is_none()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessageFold {
    pub can_collapse: bool,
    pub max_height: Option<f32>,
    pub fade_height: f32,
    pub label: &'static str,
}

/// The pinned body clips to 11rem once its actual height reaches 22rem, with a
/// 1.75rem bottom fade. NSH layout uses logical points (the source root is 16px);
/// native text scaling changes the measured content, not these layout lengths.
pub fn user_message_fold(content_height: f32, enabled: bool, expanded: bool) -> UserMessageFold {
    let can_collapse = enabled && content_height >= 352.0;
    UserMessageFold {
        can_collapse,
        max_height: (can_collapse && !expanded).then_some(176.0),
        fade_height: 28.0,
        label: if expanded {
            "Show less"
        } else {
            "Show full message"
        },
    }
}

/// Text-only collapse hint for the classic native transcript, whose projection
/// does not receive rendered body bounds. The measured TSX transcript instead
/// uses `user_message_fold`, matching the pinned useTallEnoughToFold contract.
pub fn should_collapse_user_message(text: &str) -> bool {
    let visible = link_labels(text);
    user_message_has_body(&visible)
        && (visible.encode_utf16().count() > 600 || visible.split('\n').count() > 8)
}

/// Authored whitespace follows the same ECMAScript boundary as the source UI.
pub fn user_message_has_body(text: &str) -> bool {
    !crate::text::trim(text).is_empty()
}

/// Resolve names from authenticated thread/task metadata, never from prompt text.
/// A local user's absent author always displays as You, irrespective of a name.
pub fn user_message_author_label<'a>(
    author: Option<&Author>,
    origin_name: Option<&'a str>,
) -> &'a str {
    let fallback = match author {
        None => return "You",
        Some(Author::Automation { .. }) => "Automation",
        Some(Author::Agent { .. }) => "Agent",
        Some(Author::System) => return "System",
    };
    origin_name
        .map(crate::text::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
}

/// Sent images stand on their own when the message has no words or file labels.
pub fn shows_user_message_bubble(text: &str, has_images: bool) -> bool {
    !has_images || user_message_has_body(text)
}

/// `text` with each Markdown link `[label](target)` as its label.
fn link_labels(text: &str) -> String {
    let mut visible = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let (before, candidate) = rest.split_at(open);
        visible.push_str(before);
        let link = candidate[1..].split_once("](").and_then(|(label, after)| {
            let (_, tail) = after.split_once(')')?;
            (!label.is_empty() && !label.contains(']')).then_some((label, tail))
        });
        match link {
            Some((label, tail)) => {
                visible.push_str(label);
                rest = tail;
            }
            None => {
                visible.push('[');
                rest = &candidate[1..];
            }
        }
    }
    visible + rest
}

#[cfg(test)]
mod tests {
    use super::{should_collapse_user_message as collapses, *};

    #[test]
    fn rewind_requires_a_connected_idle_session_without_another_send_or_rewind() {
        assert!(conversation_rewind_enabled(true, false, false, false));
        for input in [
            (false, false, false, false),
            (true, true, false, false),
            (true, false, true, false),
            (true, false, false, true),
        ] {
            assert!(!conversation_rewind_enabled(
                input.0, input.1, input.2, input.3
            ));
        }
    }

    #[test]
    fn measured_body_reaches_twice_the_preview_before_it_folds() {
        for height in [0.0, 176.0, 351.99] {
            assert!(!user_message_fold(height, true, false).can_collapse);
            assert_eq!(user_message_fold(height, true, false).max_height, None);
        }
        let fold = user_message_fold(352.0, true, false);
        assert!(fold.can_collapse);
        assert_eq!(fold.max_height, Some(176.0));
        assert_eq!(fold.fade_height, 28.0);
        assert_eq!(fold.label, "Show full message");
        assert_eq!(user_message_fold(900.0, true, true).max_height, None);
        assert_eq!(user_message_fold(900.0, true, true).label, "Show less");
        assert!(!user_message_fold(900.0, false, false).can_collapse);
    }

    #[test]
    fn only_human_nonblank_bodies_use_the_ordinary_fold() {
        assert!(user_message_body_folding_enabled(None, true));
        assert!(!user_message_body_folding_enabled(None, false));
        assert!(!user_message_body_folding_enabled(
            Some(&Author::System),
            true
        ));
        assert!(!user_message_body_folding_enabled(
            Some(&Author::Agent {
                session: agent_protocol::session::SessionRef::new(None, "sender".into()).unwrap()
            }),
            true
        ));
    }

    #[test]
    fn sender_labels_use_authenticated_metadata_and_never_prompt_text() {
        let agent = Author::Agent {
            session: agent_protocol::session::SessionRef::new(None, "sender".into()).unwrap(),
        };
        let automation = Author::Automation {
            id: uuid::Uuid::nil(),
        };
        for (author, name, expected) in [
            (None, None, "You"),
            (None, Some("Agent"), "You"),
            (Some(&Author::System), Some("You"), "System"),
            (Some(&agent), Some("\u{feff} Worker "), "Worker"),
            (Some(&agent), Some("\u{feff} "), "Agent"),
            (Some(&automation), Some("Scheduled work"), "Scheduled work"),
            (Some(&automation), None, "Automation"),
        ] {
            assert_eq!(user_message_author_label(author, name), expected);
        }
    }

    #[test]
    fn images_without_words_have_no_bubble_and_captions_and_file_labels_keep_theirs() {
        for text in ["", " \n\t", "\u{3000}", "\u{feff} \n"] {
            assert!(!super::shows_user_message_bubble(text, true));
            assert!(super::shows_user_message_bubble(text, false));
        }
        for text in ["a", " 日本語\n", "添付: hello.txt (/hello.txt)", "\u{0085}"] {
            assert!(super::shows_user_message_bubble(text, true));
            assert!(super::shows_user_message_bubble(text, false));
        }
    }

    #[test]
    fn a_message_folds_past_eight_lines_or_600_characters() {
        assert!(!collapses(&["a"; 8].join("\n")));
        assert!(collapses(&["a"; 9].join("\n")));
        assert!(!collapses(&"あ".repeat(600)));
        assert!(collapses(&"あ".repeat(601)));
        assert!(!collapses("   \n  "));
        assert!(!collapses(&"\u{feff}".repeat(601)));
        assert!(collapses(&"\u{0085}".repeat(601)));
    }

    #[test]
    fn links_count_as_their_labels() {
        let long_target = format!("[file.rs](/{})", "a".repeat(700));
        assert!(!collapses(&long_target));
        let brackets = format!("[not a link] {}", "a".repeat(601));
        assert!(collapses(&brackets));
    }
}

/// Conversation editing and checkpoint actions share the same idle gate.
/// The Host still owns authoritative command admission.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn conversation_rewind_enabled(
    connected: bool,
    running: bool,
    sending: bool,
    rewinding: bool,
) -> bool {
    connected && !running && !sending && !rewinding
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RewindConfirmation {
    pub title: String,
    pub description: String,
    pub cancel_label: String,
    pub confirm_label: String,
    pub confirm_action: String,
}

/// All UI bindings describe the same operation: history and composer restore,
/// leaving workspace files in place.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn rewind_confirmation(language: crate::localization::UiLanguage) -> RewindConfirmation {
    RewindConfirmation {
        title: language.text("ここから編集しますか？").into(),
        description: language.text("この発言の前まで会話を巻き戻します。発言と添付は入力欄に戻ります。ファイルの変更はそのまま残ります。").into(),
        cancel_label: language.text("キャンセル").into(),
        confirm_label: language.text("巻き戻す").into(),
        confirm_action: "rewind".into(),
    }
}
