//! Debug Mac comparison: AppKit owns the conversation viewport and TextKit text.
//! Tables are flattened and media are links; rich table/media controls, message
//! search and the minimap are outside this prototype. Hybrid remains the default.
//! Plan saving, queued markers, elapsed clocks and history error/notice details
//! are also omitted. Offscreen AX rows and keyboard/AX viewport scrolling still
//! need native implementations. CPU figures include the current full-snapshot
//! Rust adapter; they do not isolate AppKit/TextKit layout cost.
use super::*;
use agent_core::presentation::markdown::{MarkdownBlock, MarkdownRun, MarkdownStyle};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Documents {
    context: Option<(
        String,
        agent_core::localization::UiLanguage,
        String,
        Option<String>,
    )>,
    rows: HashMap<String, (Row, bool, Value)>,
}

impl Documents {
    fn project(
        &mut self,
        rows: &[Row],
        opened: &HashSet<String>,
        target: &str,
        cwd: &str,
        home: Option<&str>,
    ) -> Vec<Value> {
        let context = (
            target.to_owned(),
            crate::l10n::language(),
            cwd.to_owned(),
            home.map(str::to_owned),
        );
        let side_chat =
            serde_json::from_str::<Value>(target).map_or(true, |value| value["pane"].is_null());
        if self.context.as_ref() != Some(&context) {
            self.rows.clear();
            self.context = Some(context);
        }
        let ids: HashSet<_> = rows.iter().map(|row| row.id.as_str()).collect();
        self.rows.retain(|id, _| ids.contains(id.as_str()));
        rows.iter()
            .map(|row| {
                let open = row
                    .detail
                    .as_ref()
                    .is_some_and(|item| opened.contains(&item.source.data.id));
                let unchanged =
                    self.rows
                        .get(&row.id)
                        .is_some_and(|(previous, previous_open, _)| {
                            previous == row && *previous_open == open
                        });
                if !unchanged {
                    self.rows.insert(
                        row.id.clone(),
                        (
                            row.clone(),
                            open,
                            project_row(row, open, target, cwd, home, side_chat),
                        ),
                    );
                }
                self.rows[&row.id].2.clone()
            })
            .collect()
    }
}

fn paragraphs(blocks: Vec<MarkdownBlock>) -> Vec<Value> {
    blocks
        .into_iter()
        .flat_map(|block| match block {
            MarkdownBlock::Paragraph { mut runs, style } => {
                for run in &mut runs {
                    if let Some(image) = run.image.take() {
                        run.text = format!("画像: {image}");
                        run.link = Some(image);
                    }
                }
                vec![crate::rich_text::paragraph(runs, &style, &|_| None)]
            }
            MarkdownBlock::Table { rows, .. } => rows
                .into_iter()
                .map(|cells| {
                    let mut runs = Vec::new();
                    for (index, cell) in cells.into_iter().enumerate() {
                        if index > 0 {
                            runs.push(MarkdownRun {
                                text: "  │  ".into(),
                                ..Default::default()
                            });
                        }
                        runs.extend(cell.runs);
                    }
                    crate::rich_text::paragraph(
                        runs,
                        &MarkdownStyle {
                            code: true,
                            ..Default::default()
                        },
                        &|_| None,
                    )
                })
                .collect(),
            MarkdownBlock::Visualization { path } => vec![crate::rich_text::paragraph(
                vec![MarkdownRun {
                    text: path.clone(),
                    link: Some(path),
                    ..Default::default()
                }],
                &MarkdownStyle::default(),
                &|_| None,
            )],
        })
        .collect()
}

fn button(action: &str, title: &str) -> Value {
    json!({"action": action, "title": title})
}

fn project_row(
    row: &Row,
    open: bool,
    target: &str,
    cwd: &str,
    home: Option<&str>,
    side_chat: bool,
) -> Value {
    let mut buttons = Vec::new();
    let mut body = row.body.to_string();
    match row.shape {
        Shape::Fold { open } => {
            buttons.push(button(
                "fold",
                &format!("{} {}", if open { "▾" } else { "▸" }, body),
            ));
            body.clear();
        }
        Shape::Group { open } => {
            buttons.push(button(
                "group",
                &format!("{} {}", if open { "▾" } else { "▸" }, body),
            ));
            body.clear();
        }
        _ => {}
    }
    let mut blocks = agent_core::presentation::markdown::workspace_markdown_blocks(
        body,
        cwd.into(),
        home.map(str::to_owned),
    );
    for image in &row.images {
        blocks.push(MarkdownBlock::Paragraph {
            runs: vec![MarkdownRun {
                text: format!("画像: {image}"),
                link: Some(image.clone()),
                ..Default::default()
            }],
            style: MarkdownStyle::default(),
        });
    }
    if let Some(item) = &row.detail {
        buttons.push(button(
            "item",
            &format!("{} {}", if open { "▾" } else { "▸" }, row.body),
        ));
        blocks = if open {
            item.source.detail_blocks()
        } else {
            Vec::new()
        };
    }
    if matches!(row.shape, Shape::Message | Shape::Commentary) && row.kind != Kind::Activity {
        buttons.push(button(
            "copy",
            tr(if row.kind == Kind::User {
                "メッセージをコピー"
            } else {
                "回答をコピー"
            }),
        ));
    }
    if row.fork.is_some() {
        buttons.push(button("fork", tr("ここから会話を分岐")));
    }
    json!({
        "id": row.id,
        "user": row.kind == Kind::User,
        "gap": row.gap,
        "buttons": buttons,
        "document": {
            "blocks": paragraphs(blocks), "secondary": row.kind == Kind::Activity,
            "role": if row.kind == Kind::User { "user" } else { "assistant" },
            "target": target, "sideChat": side_chat, "maxWidth": 700.0,
            "menu": {"quote": tr("チャットに追加"), "sideChat": tr("サイドチャットで質問")}
        }
    })
}

/// Diff against the native host's last delivered state, not a reactive
/// computation: computations may run more than once before the mount is sent.
pub(crate) fn changes(
    scope: &str,
    order: &[String],
    delivered: &BTreeMap<String, Value>,
    next: &Value,
) -> Value {
    let next_scope = next["scope"].as_str().unwrap_or_default();
    let reset = scope != next_scope;
    let rows = next["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let ids: Vec<_> = rows
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();
    let current: HashSet<_> = ids.iter().map(String::as_str).collect();
    let changed: Vec<_> = rows
        .iter()
        .filter(|row| reset || delivered.get(row["id"].as_str().unwrap_or_default()) != Some(*row))
        .cloned()
        .collect();
    let removed: Vec<_> = delivered
        .keys()
        .filter(|id| !current.contains(id.as_str()))
        .cloned()
        .collect();
    json!({
        "scope": next_scope, "reset": reset,
        "order": (reset || ids != order).then_some(ids),
        "changed": changed, "removed": removed,
        "following": next["following"], "inset": next["inset"],
        "history": next["history"], "notice": next["notice"], "latest": next["latest"]
    })
}

#[cfg(all(target_os = "macos", debug_assertions))]
pub(super) fn view(timeline: Timeline) -> impl IntoElement {
    let surroundings = crate::rich_text::Surroundings::current();
    let connection = surroundings.connection;
    let target = crate::rich_text::target(connection);
    let scope = computed(move || {
        connection.publication.with(|p| {
            json!([
                connection.host.get(),
                p.composer.controls.navigation.draft_key
            ])
            .to_string()
        })
    });
    let documents = stored(Documents::default());
    let opened = signal(HashSet::<String>::new());
    effect(move || {
        scope.get();
        timeline.following.set(true);
        timeline.position.replace(None);
        timeline.expanded.set(HashMap::new());
        timeline.groups.set(HashSet::new());
        timeline.inset.set(timeline.covered.get_untracked());
        opened.set(HashSet::new());
        surroundings.find.reset_position();
    });
    nsh_core::element::view().flex_1().min_h_0().label(tr("会話の履歴"))
        .child(surroundings.selection_bar())
        .child(super::opening(connection))
        .child(native("bex.conversation")
            .element_ref(&timeline.list)
            .prop("data", move || {
                let target = target.get();
                let (cwd, home, history, notice) = connection.publication.with(|p| (
                    p.composer.controls.navigation.cwd.clone(), p.context.home(),
                    p.context.history(), p.context.thread_error()));
                let rows = timeline.rows.with(|rows| opened.with(|opened| documents.with(|cache| cache.project(rows, opened, &target, &cwd, home.as_deref()))));
                json!({"scope": scope.get(), "rows": rows, "following": timeline.following.get(),
                    "inset": timeline.inset.get(), "notice": notice,
                    "history": history.map(|history| json!({"title": history.load_label, "enabled": !history.loading})),
                    "latest": tr("最新のメッセージへ")})
            })
            .on_native_event(move |event: &NativeEvent| {
                if event.data["scope"].as_str() != Some(scope.get_untracked().as_str()) { return; }
                if event.data["action"] == "scroll" {
                    surroundings.selection.dismiss();
                    let now = agent_core::presentation::conversation::ScrollPosition {
                        offset: event.data["offset"].as_f64().unwrap_or(0.0) as f32,
                        viewport: event.data["viewport"].as_f64().unwrap_or(0.0) as f32,
                        content: event.data["content"].as_f64().unwrap_or(0.0) as f32,
                    };
                    timeline.position.replace(Some(now));
                    // AppKit reports user moves only. While following, restore
                    // was at the current end; the last reported offset may
                    // predate streamed growth and cannot be the baseline.
                    let following = agent_core::presentation::conversation::follows_end(timeline.following.get_untracked(), None, now);
                    timeline.following.set(following);
                } else {
                    handle(timeline, opened, surroundings, &event.data);
                }
            }).flex_1().min_h_0())
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn handle(
    timeline: Timeline,
    opened: Signal<HashSet<String>>,
    surroundings: crate::rich_text::Surroundings,
    data: &Value,
) {
    let connection = surroundings.connection;
    let action = data["action"].as_str().unwrap_or_default();
    if action == "history" {
        timeline.following.set(false);
        if let Some(thread_id) = connection
            .context
            .with_untracked(|p| p.navigation().thread_id)
        {
            connection.dispatch(Intent::ReadOlder { thread_id });
        }
        return;
    }
    if action == "latest" {
        timeline.following.set(true);
        return;
    }
    let row = timeline.rows.with_untracked(|rows| {
        rows.iter()
            .find(|row| Some(row.id.as_str()) == data["row"].as_str())
            .cloned()
    });
    let Some(row) = row else {
        return crate::rich_text::action(surroundings, data);
    };
    match action {
        "copy" => set_clipboard(row.body.to_string()),
        "fold" => {
            if let Some(activity) = row.activity {
                let opening = !matches!(row.shape, Shape::Fold { open: true });
                if let Some(load) = timeline.set_activity(activity, opening) {
                    connection.dispatch(Intent::LoadTurnItems(load));
                }
            }
        }
        "group" => timeline.toggle_group(row.id),
        "item" => {
            if let Some(item) = row.detail {
                let opening = !opened.with_untracked(|all| all.contains(&item.source.data.id));
                opened.update(|all| {
                    if opening {
                        all.insert(item.source.data.id.clone());
                    } else {
                        all.remove(&item.source.data.id);
                    }
                });
                if opening
                    && item.source.data.deferred
                    && let (Some(thread_id), Some(item_id)) = (
                        connection
                            .context
                            .with_untracked(|p| p.navigation().thread_id),
                        item.source.data.native_id.clone(),
                    )
                {
                    connection.dispatch(Intent::ReadItem(op::ReadItem {
                        thread_id,
                        turn_id: item.turn,
                        item_id,
                    }));
                }
            }
        }
        "fork" => {
            if let (Some(thread_id), Some(last_turn_id)) = (
                connection
                    .context
                    .with_untracked(|p| p.navigation().thread_id),
                row.fork,
            ) {
                connection.dispatch(Intent::ForkSession(op::ForkSession {
                    thread_id,
                    last_turn_id,
                }));
            }
        }
        _ => crate::rich_text::action(surroundings, data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_documents_keep_shared_markdown_and_drop_removed_messages() {
        let mut documents = Documents::default();
        let source = "# Heading\n\nA **bold** [link](https://example.test).\n\n```rust\nfn main() {}\n```\n\n| A | B |\n| - | - |\n| left | right |";
        let row = Row::new("message".into(), Kind::Response, Shape::Message, source);
        let rows = documents.project(
            std::slice::from_ref(&row),
            &HashSet::new(),
            "target",
            "/fixture",
            None,
        );
        let blocks = rows[0]["document"]["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["header"], 1);
        let code: String = blocks
            .iter()
            .filter(|block| block["code"] == true)
            .flat_map(|block| block["runs"].as_array().unwrap())
            .filter_map(|run| run["text"].as_str())
            .collect();
        assert!(code.contains("fn main()"));
        assert!(blocks.iter().any(|block| {
            let text: String = block["runs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|run| run["text"].as_str())
                .collect();
            block["code"] == true && text == "left  │  right"
        }));
        assert!(
            blocks
                .iter()
                .flat_map(|block| block["runs"].as_array().unwrap())
                .any(|run| run["link"] == "https://example.test")
        );
        assert_eq!(
            documents.project(&[row], &HashSet::new(), "target", "/fixture", None),
            rows
        );
        assert!(
            documents
                .project(&[], &HashSet::new(), "target", "/fixture", None)
                .is_empty()
        );
        assert!(documents.rows.is_empty());
    }

    #[test]
    fn streaming_sends_only_changed_rows_and_preserves_native_identity() {
        let first = json!({"id":"first", "text":"complete"});
        let second = json!({"id":"second", "text":"stream"});
        let delivered = [("first".into(), first.clone()), ("second".into(), second)].into();
        let next = json!({"scope":"a", "rows":[first, {"id":"second", "text":"streaming"}], "following":false, "inset":40});
        let update = changes("a", &["first".into(), "second".into()], &delivered, &next);
        assert_eq!(
            update["changed"],
            json!([{"id":"second", "text":"streaming"}])
        );
        assert!(update["order"].is_null());
        assert_eq!(update["following"], false);
        assert_eq!(update["inset"], 40);
        let switched = changes("b", &[], &delivered, &next);
        assert_eq!(switched["reset"], true);
        assert_eq!(switched["changed"], next["rows"]);
        assert_eq!(switched["order"], json!(["first", "second"]));
    }

    #[test]
    fn prepending_history_and_removing_rows_uses_stable_identifiers() {
        let delivered = [
            ("old".into(), json!({"id":"old"})),
            ("visible".into(), json!({"id":"visible"})),
        ]
        .into();
        let next = json!({"scope":"same", "rows":[{"id":"earlier"},{"id":"visible"}]});
        let update = changes("same", &["old".into(), "visible".into()], &delivered, &next);
        assert_eq!(update["changed"], json!([{"id":"earlier"}]));
        assert_eq!(update["removed"], json!(["old"]));
        assert_eq!(update["order"], json!(["earlier", "visible"]));
    }
}
