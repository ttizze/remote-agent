use super::*;
use agent_domain::Json;
use serde_json::json;

fn entry(id: &str) -> PromptStashEntry {
    PromptStashEntry {
        id: id.into(),
        created_at_ms: 1_784_894_400_000,
        prompt: format!("prompt {id}"),
        images: vec![],
        files: vec![],
        dropped_image_names: vec![],
        unreadable_image_names: vec![],
        pending_image_count: 0,
        context: None,
    }
}

fn image(id: &str, name: &str, chars: usize) -> StashImage {
    StashImage {
        id: id.into(),
        name: name.into(),
        mime_type: "image/png".into(),
        size_bytes: chars as u64,
        data_url: "x".repeat(chars),
    }
}

fn ids(stash: &PromptStash) -> Vec<&str> {
    stash
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect()
}

#[test]
fn keeps_attachments_within_the_budget_and_reports_dropped_names_in_order() {
    let partition = partition_stash_images(vec![
        image("a", "small.png", 10),
        image("b", "huge.png", MAX_STASH_ENTRY_ATTACHMENT_CHARS),
        image("c", "also-small.png", 10),
    ]);
    let kept: Vec<&str> = partition
        .kept
        .iter()
        .map(|image| image.id.as_str())
        .collect();
    assert_eq!(kept, ["a", "c"]);
    assert_eq!(partition.dropped_names, ["huge.png"]);
}

#[test]
fn admits_a_single_attachment_that_exactly_fits_the_budget() {
    let partition = partition_stash_images(vec![image(
        "a",
        "exact.png",
        MAX_STASH_ENTRY_ATTACHMENT_CHARS,
    )]);
    assert_eq!(partition.kept.len(), 1);
    assert!(partition.dropped_names.is_empty());
}

#[test]
fn prepends_entries_so_the_newest_stash_is_first() {
    let mut stash = PromptStash::default();
    stash.stash(entry("first"));
    stash.stash(entry("second"));
    assert_eq!(ids(&stash), ["second", "first"]);
}

#[test]
fn evicts_the_oldest_entry_past_the_cap_and_returns_it() {
    let mut stash = PromptStash::default();
    for index in 0..MAX_STASH_ENTRIES {
        assert_eq!(stash.stash(entry(&format!("entry-{index}"))), None);
    }
    let evicted = stash.stash(entry("overflow"));
    assert_eq!(evicted.map(|entry| entry.id).as_deref(), Some("entry-0"));
    assert_eq!(stash.entries.len(), MAX_STASH_ENTRIES);
    assert_eq!(stash.entries[0].id, "overflow");
}

#[test]
fn take_entry_removes_and_returns_the_entry_second_take_returns_none() {
    let mut stash = PromptStash::default();
    stash.stash(entry("keep"));
    stash.stash(entry("take"));
    assert_eq!(
        stash.take("take").map(|entry| entry.id).as_deref(),
        Some("take")
    );
    assert_eq!(stash.take("take"), None);
    assert_eq!(ids(&stash), ["keep"]);
}

#[test]
fn finalize_entry_images_attaches_images_and_clears_the_pending_count() {
    let mut stash = PromptStash::default();
    stash.stash(PromptStashEntry {
        pending_image_count: 2,
        ..entry("pending")
    });
    assert!(stash.finalize_images(
        "pending",
        StashImages {
            images: vec![image("img-1", "a.webp", 4)],
            dropped_image_names: vec!["big.png".into()],
            unreadable_image_names: vec![],
        }
    ));
    let entry = &stash.entries[0];
    assert_eq!(entry.images.len(), 1);
    assert_eq!(entry.dropped_image_names, ["big.png"]);
    assert_eq!(entry.pending_image_count, 0);
}

#[test]
fn take_entry_returns_images_and_drop_metadata_finalized_after_a_menu_snapshot() {
    let mut stash = PromptStash::default();
    stash.stash(PromptStashEntry {
        pending_image_count: 1,
        ..entry("pending-restore")
    });
    let menu = stash_menu(&stash);
    assert!(menu[0].thumbnails.is_empty());
    stash.finalize_images(
        "pending-restore",
        StashImages {
            images: vec![image("img-finalized", "finalized.webp", 4)],
            dropped_image_names: vec!["too-large.png".into()],
            unreadable_image_names: vec!["unreadable.png".into()],
        },
    );
    let taken = stash.take("pending-restore").unwrap();
    assert_eq!(taken.images[0].id, "img-finalized");
    assert_eq!(taken.images[0].name, "finalized.webp");
    assert_eq!(taken.dropped_image_names, ["too-large.png"]);
    assert_eq!(taken.unreadable_image_names, ["unreadable.png"]);
    assert_eq!(taken.pending_image_count, 0);
}

#[test]
fn preserves_uploaded_file_references_without_storing_file_contents() {
    let file = StashFile {
        id: "file-1".into(),
        name: "report.pdf".into(),
        mime_type: "application/pdf".into(),
        size_bytes: 42,
        attachment_id: "pending-report-pdf".into(),
    };
    let mut stash = PromptStash::default();
    stash.stash(PromptStashEntry {
        files: vec![file.clone()],
        ..entry("with-file")
    });
    stash.finalize_images("with-file", StashImages::default());
    assert_eq!(stash.entries[0].files, [file]);
}

#[test]
fn finalize_entry_images_reports_false_when_the_entry_was_already_taken() {
    let mut stash = PromptStash::default();
    stash.stash(PromptStashEntry {
        pending_image_count: 1,
        ..entry("racing")
    });
    stash.take("racing");
    assert!(!stash.finalize_images("racing", StashImages::default()));
}

#[test]
fn settles_a_pending_count_left_behind_by_a_crashed_or_closed_session() {
    let mut stash = PromptStash {
        entries: vec![PromptStashEntry {
            pending_image_count: 2,
            ..entry("orphan")
        }],
    };
    stash.settle_pending_images();
    let entry = &stash.entries[0];
    assert_eq!(entry.pending_image_count, 0);
    assert_eq!(
        entry.unreadable_image_names,
        [
            "image 1 (not saved before reload)",
            "image 2 (not saved before reload)"
        ]
    );
}

#[test]
fn keeps_the_records_behind_a_stashed_prompts_chips() {
    let context = MessageContext {
        version: 1,
        records: vec![Json(json!({
            "version": 1,
            "contextId": "ctx-1",
            "kind": "terminal",
            "label": "Terminal 1 line 4",
            "terminalId": "default",
            "terminalLabel": "Terminal 1",
            "lineStart": 4,
            "lineEnd": 4,
            "text": "boom",
        }))],
    };
    let mut stash = PromptStash::default();
    stash.stash(PromptStashEntry {
        prompt: "see [Terminal 1 line 4](context://v1/terminal/ctx-1)".into(),
        context: Some(context.clone()),
        ..entry("entry-records")
    });
    assert_eq!(stash.take("entry-records").unwrap().context, Some(context));
}

fn draft(text: &str, attachments: Vec<DraftAttachment>) -> Draft {
    Draft {
        text: text.into(),
        attachments,
        ..Draft::default()
    }
}

fn draft_attachment(id: &str, kind: &str, name: &str, status: &str) -> DraftAttachment {
    DraftAttachment {
        id: id.into(),
        remote_id: (status == "ready").then(|| format!("upload-{id}")),
        name: name.into(),
        mime_type: if kind == "image" {
            "image/png"
        } else {
            "application/pdf"
        }
        .into(),
        kind: kind.into(),
        size_bytes: 10,
        local_path: String::new(),
        status: status.into(),
        error: None,
    }
}

#[test]
fn stashes_a_composer_with_content_and_restores_the_only_saved_entry_when_empty() {
    let mut stash = PromptStash::default();
    assert_eq!(
        stash_shortcut(&draft("hi", vec![]), &stash),
        StashShortcut::Stash
    );
    assert_eq!(
        stash_shortcut(&draft(" ", vec![]), &stash),
        StashShortcut::ToggleMenu
    );
    stash.stash(PromptStashEntry {
        pending_image_count: 1,
        ..entry("saving")
    });
    assert_eq!(
        stash_shortcut(&draft("", vec![]), &stash),
        StashShortcut::ToggleMenu
    );
    stash.finalize_images("saving", StashImages::default());
    assert_eq!(
        stash_shortcut(&draft("", vec![]), &stash),
        StashShortcut::Restore {
            entry_id: "saving".into()
        }
    );
    stash.stash(entry("second"));
    assert_eq!(
        stash_shortcut(&draft("", vec![]), &stash),
        StashShortcut::ToggleMenu
    );
}

#[test]
fn writes_the_trimmed_prompt_and_file_uploads_before_the_images() {
    let draft = draft(
        "  fix this \n",
        vec![
            draft_attachment("shot", "image", "shot.png", "uploading"),
            draft_attachment("doc", "file", "doc.pdf", "ready"),
        ],
    );
    let entry = new_stash_entry("e1".into(), 5, &draft, None).unwrap();
    assert_eq!(entry.prompt, "fix this");
    assert_eq!(entry.pending_image_count, 1);
    assert_eq!(entry.files[0].attachment_id, "upload-doc");
    assert_eq!(entry.created_at_ms, 5);

    let uploading = Draft {
        attachments: vec![draft_attachment("doc", "file", "doc.pdf", "uploading")],
        ..draft
    };
    assert_eq!(
        new_stash_entry("e2".into(), 5, &uploading, None),
        Err("Wait for file uploads before stashing this prompt".into())
    );
}

#[test]
fn restores_text_after_a_blank_line_and_reports_what_did_not_come_back() {
    let stashed = PromptStashEntry {
        prompt: "restored".into(),
        images: vec![image("dup", "shot.png", 10), image("new", "new.png", 10)],
        files: vec![
            StashFile {
                id: "kept".into(),
                name: "kept.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 10,
                attachment_id: "upload-kept".into(),
            },
            StashFile {
                id: "old".into(),
                name: "old.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 11,
                attachment_id: "upload-old".into(),
            },
        ],
        dropped_image_names: vec!["huge.png".into()],
        ..entry("e")
    };
    let current = draft(
        "current  \n",
        vec![draft_attachment("dup", "image", "shot.png", "ready")],
    );
    let restore = restore_stash_entry(&stashed, &current, &["upload-old".into()]);
    assert_eq!(restore.text, "current\n\nrestored");
    assert!(restore.text_changed);
    let images: Vec<&str> = restore
        .images
        .iter()
        .map(|image| image.id.as_str())
        .collect();
    assert_eq!(images, ["new"]);
    let files: Vec<&str> = restore.files.iter().map(|file| file.id.as_str()).collect();
    assert_eq!(files, ["kept"]);
    assert_eq!(
        restore.warning.as_deref(),
        Some(
            "huge.png exceeded the stash size limit when this prompt was saved. \
             old.pdf: stashed files are kept for 24 hours and this upload expired. Attach the file again."
        )
    );
}

#[test]
fn an_image_only_entry_leaves_the_draft_text_alone() {
    let stashed = PromptStashEntry {
        prompt: String::new(),
        images: vec![image("a", "a.png", 1)],
        ..entry("e")
    };
    let restore = restore_stash_entry(&stashed, &draft("keep  ", vec![]), &[]);
    assert_eq!(restore.text, "keep  ");
    assert!(!restore.text_changed);
    assert_eq!(restored_prompt("", "x"), "x");
}

#[test]
fn restores_up_to_the_attachment_limit() {
    let full: Vec<DraftAttachment> = (0..MAX_COMPOSER_ATTACHMENTS - 1)
        .map(|index| {
            draft_attachment(
                &format!("f{index}"),
                "file",
                &format!("{index}.pdf"),
                "ready",
            )
        })
        .collect();
    let stashed = PromptStashEntry {
        images: vec![image("a", "a.png", 1), image("b", "b.png", 1)],
        ..entry("e")
    };
    let restore = restore_stash_entry(&stashed, &draft("", full), &[]);
    assert_eq!(restore.images.len(), 1);
    assert_eq!(
        restore.warning.as_deref(),
        Some("b.png could not be restored: the composer is at its 100-attachment limit.")
    );
}

#[test]
fn summarizes_entries_for_the_stash_menu() {
    let long = "word ".repeat(30);
    let stash = PromptStash {
        entries: vec![
            PromptStashEntry {
                prompt: long.clone(),
                pending_image_count: 2,
                ..entry("long")
            },
            PromptStashEntry {
                prompt: String::new(),
                images: vec![image("a", "a.png", 1)],
                dropped_image_names: vec!["b.png".into()],
                ..entry("images")
            },
            PromptStashEntry {
                prompt: String::new(),
                files: vec![StashFile {
                    id: "f".into(),
                    name: "f.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size_bytes: 1,
                    attachment_id: "u".into(),
                }],
                images: vec![image("a", "a.png", 1)],
                ..entry("mixed")
            },
            PromptStashEntry {
                prompt: String::new(),
                ..entry("empty")
            },
        ],
    };
    let menu = stash_menu(&stash);
    let expected = format!("{}…", &long.trim_end()[..90]);
    assert_eq!(menu[0].snippet, expected);
    assert_eq!(menu[0].status.as_deref(), Some("saving 2 images…"));
    assert!(!menu[0].status_warning);
    assert_eq!(menu[1].snippet, "(2 images)");
    assert_eq!(menu[1].status.as_deref(), Some("1 image dropped"));
    assert!(menu[1].status_warning);
    assert_eq!(menu[2].snippet, "(2 attachments)");
    assert_eq!(menu[2].file_count, 1);
    assert_eq!(menu[3].snippet, "(empty)");
    assert_eq!(menu[3].restore_label, "Restore stashed prompt: (empty)");
}
