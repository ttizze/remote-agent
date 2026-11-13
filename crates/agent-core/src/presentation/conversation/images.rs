use super::{ConversationRow, ConversationRowContent};
use crate::presentation::markdown::{MarkdownFileSurface, markdown_file_target, markdown_images};
use std::collections::HashSet;

fn image_key(source: &str) -> String {
    let path = markdown_file_target(source.into(), String::new(), MarkdownFileSurface::Mobile)
        .map_or_else(|| source.to_owned(), |file| file.path);
    image_path_key(&path)
}

fn image_path_key(path: &str) -> String {
    if agent_protocol::paths::is_windows_absolute_host_path(path)
        || agent_protocol::paths::is_network_host_path(path)
    {
        path.replace('\\', "/").to_lowercase()
    } else {
        path.to_owned()
    }
}

/// A visible inline image takes over its earlier standalone preview. Only
/// responses in the same user exchange count; collapsed work cannot replace it.
pub(super) fn without_repeated_images(mut rows: Vec<ConversationRow>) -> Vec<ConversationRow> {
    if !rows.iter().any(|row| {
        matches!(&row.content,
            ConversationRowContent::Response { item, .. } if !item.data.image_sources.is_empty()
        )
    }) {
        return rows;
    }
    let mut inline_images = HashSet::new();
    rows.reverse();
    rows.retain(|row| {
        match &row.content {
            ConversationRowContent::User { .. } => inline_images.clear(),
            ConversationRowContent::Activity { item, .. } if item.data.kind == "user" => {
                inline_images.clear()
            }
            ConversationRowContent::Response { item, .. }
                if matches!(item.data.kind.as_str(), "agent" | "commentary") =>
            {
                if let Some(text) = &item.data.body {
                    inline_images
                        .extend(markdown_images(text).iter().map(|source| image_key(source)));
                }
            }
            // Grouping exposes assistant text and images as responses.
            ConversationRowContent::Response { item, .. } => {
                return item.data.body.is_some()
                    || item.data.image_sources.is_empty()
                    || !item.data.image_sources.iter().all(|source| {
                        let key = if item.data.kind == "imageView" && source.starts_with("file:") {
                            image_key(source)
                        } else {
                            image_path_key(source)
                        };
                        inline_images.contains(&key)
                    });
            }
            _ => {}
        }
        true
    });
    rows.reverse();
    rows
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::json;

    fn image(id: &str, path: &str) -> Arc<models::Item> {
        Arc::new(models::Item::new(
            id.into(),
            models::ItemStatus::Completed,
            models::ItemBody::ImageGeneration {
                saved_path: Some(path.into()),
                data: None,
                revised_prompt: None,
            },
        ))
    }

    fn answer(text: &str, phase: &str) -> Arc<models::Item> {
        Arc::new(
            serde_json::from_value(json!({
                "id": "answer", "status": "completed",
                "body": {"inline": {"body": {"assistantText": {"text": text, "phase": phase}}}}
            }))
            .unwrap(),
        )
    }

    fn viewed_image(id: &str, path: &str) -> Arc<models::Item> {
        Arc::new(models::Item::new(
            id.into(),
            models::ItemStatus::Completed,
            models::ItemBody::ImageView { path: path.into() },
        ))
    }

    fn thread(items: Vec<Arc<models::Item>>, status: models::TurnStatus) -> Arc<models::Thread> {
        Arc::new(models::Thread {
            turns: Some(vec![Arc::new(models::Turn {
                id: "turn".into(),
                status,
                items: Some(items),
                ..Default::default()
            })]),
            ..Default::default()
        })
    }

    fn response_ids(conversation: &RenderedConversation) -> Vec<&str> {
        conversation.turns[0]
            .rows
            .iter()
            .filter_map(|row| match &row.content {
                ConversationRowContent::Response { item, .. } => Some(item.data.id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn viewed_screenshots_stay_visible_outside_collapsed_work_live_and_reopened() {
        for status in [models::TurnStatus::Running, models::TurnStatus::Completed] {
            let source = thread(
                vec![
                    answer("Checking the screenshots", "commentary"),
                    Arc::new(models::Item::new(
                        "command".into(),
                        models::ItemStatus::Completed,
                        models::ItemBody::CommandExecution {
                            output_omitted: false,
                            command: "capture screenshots".into(),
                            cwd: None,
                            output: String::new(),
                            exit_code: Some(0),
                        },
                    )),
                    viewed_image("desktop", "file:///tmp/blog-desktop.png"),
                    viewed_image("phone", "/tmp/blog-phone.png"),
                    answer("The screenshots are above", "final"),
                ],
                status,
            );
            let snapshot = Snapshot::default();
            let mut previous = None;
            for _ in 0..2 {
                let projected = project_conversation(
                    crate::localization::UiLanguage::Japanese,
                    &snapshot.panes.primary().navigation.draft_key,
                    &snapshot.sessions.pending_submissions,
                    source.clone(),
                    &previous,
                );
                let visible = turn_rows(
                    &projected.turns[0],
                    &Default::default(),
                    &Default::default(),
                );
                let screenshots: Vec<_> = visible
                    .iter()
                    .filter_map(|row| match row {
                        TurnRow::Row(ConversationRow {
                            content: ConversationRowContent::Response { item, .. },
                            ..
                        }) if item.data.kind == "imageView" => Some(item),
                        _ => None,
                    })
                    .collect();
                assert_eq!(screenshots.len(), 2);
                for (item, path) in screenshots
                    .iter()
                    .zip(["file:///tmp/blog-desktop.png", "/tmp/blog-phone.png"])
                {
                    assert_eq!(item.data.image_sources, [path]);
                    assert_eq!(item.data.body, None);
                    assert_eq!(item.data.title, None);
                    assert!(!item.data.collapsible);
                    assert!(!item.data.image_placeholder);
                }
                previous = Some(projected);
            }
        }
    }

    #[rstest::rstest]
    #[case::path("/tmp/blog.png", "![blog](/tmp/blog.png)", false)]
    #[case::uri("file:///tmp/blog.png", "![blog](/tmp/blog.png)", false)]
    #[case::encoded_uri(
        "file:///tmp/blog%20screen.png",
        "![blog](/tmp/blog%20screen.png)",
        false
    )]
    #[case::literal_percent("/tmp/blog%20screen.png", "![blog](/tmp/blog%2520screen.png)", false)]
    #[case::different_file("/tmp/blog.png", "![blog](/tmp/other.png)", true)]
    fn markdown_takes_over_viewed_screenshots(
        #[case] path: &str,
        #[case] text: &str,
        #[case] standalone: bool,
        #[values(models::TurnStatus::Running, models::TurnStatus::Completed)]
        status: models::TurnStatus,
        #[values("commentary", "final")] phase: &str,
    ) {
        let snapshot = Snapshot::default();
        let projected = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread(
                vec![viewed_image("screenshot", path), answer(text, phase)],
                status,
            ),
            &None,
        );
        assert_eq!(response_ids(&projected).contains(&"screenshot"), standalone);
        assert_eq!(response_ids(&projected).last(), Some(&"answer"));
    }

    #[rstest::rstest]
    fn missing_and_deferred_viewed_images_do_not_show_empty_rows(
        #[values(false, true)] deferred: bool,
    ) {
        let mut image = viewed_image("screenshot", if deferred { "/tmp/blog.png" } else { "" });
        if deferred {
            Arc::make_mut(&mut image).defer();
        }
        let snapshot = Snapshot::default();
        let projected = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread(vec![image], models::TurnStatus::Completed),
            &None,
        );
        assert_eq!(projected.turns[0].items().count(), 0);
    }

    #[rstest::rstest]
    fn screenshots_keep_the_last_comments_actions(
        #[values(
            models::TurnStatus::Completed,
            models::TurnStatus::Failed,
            models::TurnStatus::Interrupted
        )]
        status: models::TurnStatus,
    ) {
        let snapshot = Snapshot::default();
        let projected = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread(
                vec![
                    answer("Copy this", "commentary"),
                    viewed_image("screenshot", "/tmp/blog.png"),
                ],
                status,
            ),
            &None,
        );
        assert!(projected.turns[0].rows.iter().any(|row| matches!(&row.content,
            ConversationRowContent::Response { item, show_actions: true, .. } if item.data.body.as_deref() == Some("Copy this")
        )));
    }

    #[rstest::rstest]
    #[case::inline("![phone](/phone.png)", false)]
    #[case::reference("![phone][preview]\n\n[preview]: /phone.png", false)]
    #[case::file_url("![phone](file:///phone.png)", false)]
    #[case::encoded_path("![phone](/%70hone.png)", false)]
    #[case::disclosure(
        "<details><summary>Preview</summary>\n\n![phone](/phone.png)\n\n</details>",
        false
    )]
    #[case::link("[phone](/phone.png)", true)]
    #[case::code("`![phone](/phone.png)`", true)]
    #[case::fenced("```md\n![phone](/phone.png)\n```", true)]
    #[case::escaped(r"\![phone](/phone.png)", true)]
    #[case::incomplete("![phone](/phone.png", true)]
    #[case::different_file("![phone](/other/phone.png)", true)]
    fn generated_image_is_shown_once_at_its_place_in_the_response(
        #[case] text: &str,
        #[case] standalone: bool,
        #[values(models::TurnStatus::Running, models::TurnStatus::Completed)]
        status: models::TurnStatus,
    ) {
        let projected = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            thread(
                vec![
                    image("phone", "/phone.png"),
                    image("desktop", "/desktop.png"),
                    answer(text, "final"),
                ],
                status,
            ),
            &None,
        );
        assert_eq!(
            response_ids(&projected),
            if standalone {
                vec!["phone", "desktop", "answer"]
            } else {
                vec!["desktop", "answer"]
            }
        );
        assert_eq!(
            projected.turns[0]
                .items()
                .last()
                .unwrap()
                .data
                .body
                .as_deref(),
            Some(text)
        );
    }

    #[test]
    fn collapsed_commentary_and_later_user_input_keep_the_generated_image() {
        let user = Arc::new(models::Item::new(
            "user".into(),
            models::ItemStatus::Completed,
            models::ItemBody::UserMessage {
                text: Some("Next question".into()),
                content: vec![],
            },
        ));
        for items in [
            vec![
                image("phone", "/phone.png"),
                answer("![phone](/phone.png)", "commentary"),
                answer("Done", "final"),
            ],
            vec![
                image("phone", "/phone.png"),
                user,
                answer("![phone](/phone.png)", "final"),
            ],
        ] {
            let projected = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                thread(items, models::TurnStatus::Completed),
                &None,
            );
            assert_eq!(response_ids(&projected), ["phone", "answer"]);
        }
    }

    #[test]
    fn deferred_user_input_starts_a_new_exchange_but_tool_activity_does_not() {
        let deferred_user = Arc::new(serde_json::from_value(json!({
            "id": "user", "status": "completed",
            "body": {"deferred": {"summary": {"userMessage": {"text": "Next question", "content": []}}}}
        })).unwrap());
        let command = Arc::new(models::Item::new(
            "command".into(),
            models::ItemStatus::Completed,
            models::ItemBody::CommandExecution {
                output_omitted: false,
                command: "pwd".into(),
                cwd: None,
                output: String::new(),
                exit_code: Some(0),
            },
        ));
        for (activity, standalone) in [(deferred_user, true), (command, false)] {
            let projected = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                thread(
                    vec![
                        image("phone", "/phone.png"),
                        answer("Working on the next step", "commentary"),
                        activity,
                        answer("![phone](/phone.png)", "final"),
                    ],
                    models::TurnStatus::Running,
                ),
                &None,
            );
            assert!(
                projected.turns[0]
                    .rows
                    .iter()
                    .any(|row| matches!(row.content, ConversationRowContent::Activity { .. }))
            );
            assert_eq!(response_ids(&projected).contains(&"phone"), standalone);
        }
    }

    #[test]
    fn all_embedded_results_replace_previews_across_live_commentary_segments() {
        for status in [models::TurnStatus::Running, models::TurnStatus::Completed] {
            let projected = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                thread(
                    vec![
                        image("phone", "/phone.png"),
                        answer("Now generating a desktop version", "commentary"),
                        image("desktop", "/desktop.png"),
                        answer(
                            "Phone:\n\n![phone](/phone.png)\n\nDesktop:\n\n![desktop](/desktop.png)",
                            "final",
                        ),
                    ],
                    status,
                ),
                &None,
            );
            assert!(
                projected.turns[0]
                    .items()
                    .all(|item| item.data.kind != "imageGeneration")
            );
            assert_eq!(
                response_ids(&projected).len(),
                if status == models::TurnStatus::Running {
                    2
                } else {
                    1
                }
            );
        }
    }

    #[test]
    fn inline_data_replaces_its_preview_but_preserves_generation_errors() {
        for status in [models::ItemStatus::Completed, models::ItemStatus::Failed] {
            let generated = Arc::new(models::Item::new(
                "image".into(),
                status,
                models::ItemBody::ImageGeneration {
                    saved_path: None,
                    data: Some("png-data".into()),
                    revised_prompt: None,
                },
            ));
            let projected = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                thread(
                    vec![
                        generated,
                        answer("![image](data:image/png;base64,png-data)", "final"),
                    ],
                    models::TurnStatus::Completed,
                ),
                &None,
            );
            assert_eq!(
                response_ids(&projected),
                if status == models::ItemStatus::Failed {
                    vec!["image", "answer"]
                } else {
                    vec!["answer"]
                }
            );
        }
    }

    #[rstest::rstest]
    #[case::reserved("/tmp/a%20b#1.png", "![image](/tmp/a%2520b%231.png)", false)]
    #[case::distinct_literal_percent("/tmp/a%20b.png", "![image](/tmp/a%20b.png)", true)]
    #[case::windows(r"C:\Users\Me\image.png", "![image](c:/users/me/image.png)", false)]
    #[case::unc(
        r"\\HOST\Share\image.png",
        "![image](file://host/share/image.png)",
        false
    )]
    #[case::posix_case("/tmp/Image.png", "![image](/tmp/image.png)", true)]
    fn saved_paths_remain_literal_while_markdown_destinations_are_decoded(
        #[case] path: &str,
        #[case] text: &str,
        #[case] standalone: bool,
    ) {
        let projected = project_conversation(
            crate::localization::UiLanguage::Japanese,
            &Snapshot::default().panes.primary().navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            thread(
                vec![image("image", path), answer(text, "final")],
                models::TurnStatus::Completed,
            ),
            &None,
        );
        assert_eq!(response_ids(&projected).contains(&"image"), standalone);
    }

    #[test]
    fn streaming_response_takes_over_the_preview_and_reopening_keeps_it_unique() {
        let generated = image("phone", "/phone.png");
        let mut previous = None;
        for (text, standalone, status) in [
            ("", true, models::TurnStatus::Running),
            ("![phone](/phone.png", true, models::TurnStatus::Running),
            ("![phone](/phone.png)", false, models::TurnStatus::Running),
            (
                "Done\n\n![phone](/phone.png)",
                false,
                models::TurnStatus::Completed,
            ),
            (
                "The image is available above",
                true,
                models::TurnStatus::Completed,
            ),
        ] {
            let source = thread(vec![generated.clone(), answer(text, "final")], status);
            let projected = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                source.clone(),
                &previous,
            );
            let reopened = project_conversation(
                crate::localization::UiLanguage::Japanese,
                &Snapshot::default().panes.primary().navigation.draft_key,
                &Snapshot::default().sessions.pending_submissions,
                source,
                &None,
            );
            let expected = if standalone {
                vec!["phone", "answer"]
            } else {
                vec!["answer"]
            };
            assert_eq!(response_ids(&projected), expected);
            assert_eq!(response_ids(&reopened), expected);
            assert_eq!(
                projected.turns[0]
                    .items()
                    .last()
                    .unwrap()
                    .data
                    .body
                    .as_deref(),
                Some(text)
            );
            previous = Some(projected);
        }
    }
}
