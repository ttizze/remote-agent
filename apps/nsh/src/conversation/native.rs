//! Native transcript contract: native viewports own scrolling, reuse and text selection.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct Documents {
    context: Option<(
        String,
        agent_core::localization::UiLanguage,
        String,
        Option<String>,
        crate::file_actions::Chips,
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
        chips: &crate::file_actions::Chips,
    ) -> Vec<Value> {
        let context = (
            target.to_owned(),
            crate::l10n::language(),
            cwd.to_owned(),
            home.map(str::to_owned),
            chips.clone(),
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
                            project_row(row, open, target, cwd, home, side_chat, Some(chips)),
                        ),
                    );
                }
                self.rows[&row.id].2.clone()
            })
            .collect()
    }
}

fn button(action: &str, title: &str) -> Value {
    json!({"action": action, "title": title})
}

pub(super) fn project_row(
    row: &Row,
    open: bool,
    target: &str,
    cwd: &str,
    home: Option<&str>,
    side_chat: bool,
    chips: Option<&crate::file_actions::Chips>,
) -> Value {
    let mut buttons = row_buttons(row, open);
    let mut body = if matches!(row.shape, Shape::Plan) {
        row.note.to_string()
    } else {
        row.body.to_string()
    };
    if matches!(row.shape, Shape::Fold { .. } | Shape::Group { .. }) {
        body.clear();
    }
    if let Some(error) = &row.error {
        body = format!(
            "{}\n\n{}\n\n{}",
            error.title,
            error.message,
            error.details.as_deref().unwrap_or_default()
        );
    }
    if let Some(setup) = &row.preparation {
        body.push_str("\n\n");
        for step in &setup.steps {
            body.push_str(&format!(
                "{} · {} {}\n",
                step.state,
                step.label,
                step.detail.as_deref().unwrap_or_default()
            ));
            if !step.tail.is_empty() {
                body.push_str(&format!("\n```\n{}\n```\n", step.tail.join("\n")));
            }
        }
        for detail in &setup.details {
            body.push_str(&format!("\n{}: {}", detail.label, detail.value));
        }
        if let Some(error) = &setup.error {
            body.push_str(&format!("\n\n{error}"));
        }
    }
    let blocks = match &row.detail {
        Some(item) if open => item.source.detail_blocks(),
        Some(_) => Vec::new(),
        None => agent_core::presentation::markdown::workspace_markdown_blocks(
            body.clone(),
            cwd.into(),
            home.map(str::to_owned),
        ),
    };
    if row.retry.is_some() {
        buttons.push(button("retry", tr("再試行")));
    }
    let file_paths = crate::rich_text::file_paths(&blocks);
    let parts = crate::rich_text::indexed(crate::rich_text::parts(
        blocks,
        &crate::rich_text::diagrams(&body),
        &|file| chips.map(|chips| chips.chip(file)),
    ));
    json!({
        "id": row.id, "user": row.kind == Kind::User, "gap": row.gap,
        "buttons": buttons, "parts": parts, "images": row.images, "filePaths":file_paths, "media": {},
        "time": row.time, "queued": row.queued, "steer": row.steer,
        "error": row.error.as_ref().map(|error| json!({"title":error.title,"message":error.message,"details":error.details})),
        "document": {
            "blocks": [], "secondary": row.kind == Kind::Activity,
            "role": if row.kind == Kind::User { "user" } else { "assistant" },
            "target": target, "sideChat": side_chat, "maxWidth": 700.0,
            "labels": {"copyCode":tr("コードをコピー"),"copyTable":tr("表をコピー"),"copyMarkdown":tr("Markdownをコピー"),"copyCsv":tr("CSVをコピー"),"wrap":tr("折り返し"),"unwrap":tr("折り返し解除"),"openImage":tr("画像を開く"),"openFile":tr("ファイルを開く"),"source":tr("ソース"),"diagram":tr("図"),"queued":tr("キューから送信"),"steer":tr("追加メッセージ")},
            "menu": {"quote": tr("チャットに追加"), "explain":tr("詳しく解説"), "sideChat": tr("サイドチャットで質問")}
        }
    })
}

fn row_buttons(row: &Row, open: bool) -> Vec<Value> {
    let mut buttons = Vec::new();
    match row.shape {
        Shape::Fold { open } => buttons.push(button(
            "fold",
            &format!("{} {}", if open { "▾" } else { "▸" }, row.body),
        )),
        Shape::Group { open } => buttons.push(button(
            "group",
            &format!("{} {}", if open { "▾" } else { "▸" }, row.body),
        )),
        _ => {}
    }
    if row.detail.is_some() {
        buttons.push(button(
            "item",
            &format!("{} {}", if open { "▾" } else { "▸" }, row.body),
        ));
    }
    if matches!(
        row.shape,
        Shape::Message | Shape::Commentary | Shape::Actions
    ) && row.kind != Kind::Activity
    {
        buttons.push(button(
            "copy",
            tr(if row.kind == Kind::User {
                "発言をコピー"
            } else {
                "回答をコピー"
            }),
        ));
    }
    if row.fork.is_some() {
        buttons.push(button("fork", tr("ここから会話を分岐")));
    }
    if row.edit_turn.is_some() {
        buttons.push(button("edit", tr("ここから編集")));
    }
    if row.unresolved.is_some() {
        buttons.push(button("restore", tr("入力欄へ戻す")));
        buttons.push(button("discard", tr("破棄")));
    }
    if matches!(row.shape, Shape::Plan) {
        buttons.push(button("savePlan", tr("ワークスペースに保存")));
        buttons.push(button("downloadPlan", tr("ダウンロード")));
    }
    if row
        .agent
        .as_ref()
        .is_some_and(|agent| agent.target.is_some())
    {
        buttons.push(button("agent", &row.body));
    }
    if let Some(setup) = &row.preparation {
        for (enabled, action, title) in [
            (
                setup.terminal.is_some(),
                "setupTerminal",
                tr("ターミナルを開く"),
            ),
            (
                setup.work_locally.is_some(),
                "workLocally",
                tr("ローカルで作業"),
            ),
            (setup.cancel.is_some(), "cancelSetup", tr("キャンセル")),
        ] {
            if enabled {
                buttons.push(button(action, title));
            }
        }
    }
    buttons
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
    let saver = crate::plans::Saver::new();
    let chips = computed(move || crate::file_actions::Chips::read(connection));
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
    let projected = computed(move || {
        let (cwd, home) = connection
            .publication
            .with(|p| (p.composer.controls.navigation.cwd.clone(), p.context.home()));
        timeline.rows.with(|rows| {
            opened.with(|opened| {
                documents.with(|cache| {
                    cache.project(
                        rows,
                        opened,
                        &target.get(),
                        &cwd,
                        home.as_deref(),
                        &chips.get(),
                    )
                })
            })
        })
    });
    effect(move || {
        let paths = projected.with(|rows| {
            rows.iter()
                .flat_map(|row| {
                    row["filePaths"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        });
        crate::file_actions::read_metadata(connection, paths);
    });
    let media = image_resources(connection, scope, projected);
    nsh_core::element::view().flex_1().min_h_0().label(tr("会話の履歴"))

        .on_layout(move |bounds: &Bounds<nsh_core::geometry::Points>| timeline.width.set(bounds.size.width.0))
        .pb(move || px(timeline.covered.get()))
        .child(saver.view(connection))
        .child(super::opening(connection))
        .child(native("bex.conversation")
            .element_ref(&timeline.list)
            .prop("data", move || {
                let (history, notice) = connection.publication.with(|p| (p.context.history(), p.context.thread_error()));
                let mut rows = projected.get();
                let query = surroundings.find.highlighted();
                for row in &mut rows {
                    if let Ok(mut parts) = serde_json::from_value::<Vec<crate::rich_text::Part>>(row["parts"].clone()) {
                        crate::rich_text::highlight(&mut parts, &query, surroundings.find.selection(row["id"].as_str().unwrap_or_default()));
                        row["parts"] = json!(parts);
                    }
                    row["document"]["findKey"] = json!(surroundings.find.key());
                    row["media"] = media.with(|files| json!(files.iter().filter_map(|(source, file)| file.as_ref().map(|(_, data)| (source.clone(),data.clone()))).collect::<HashMap<_,_>>()));
                }
                json!({"scope": scope.get(), "rows": rows, "following": timeline.following.get(),
                    "inset": (timeline.inset.get() - timeline.covered.get()).max(0.0), "notice": notice,
                    "history": history.map(|history| json!({"title": history.load_label, "enabled": !history.loading})),
                    "latest": tr("最新のメッセージへ")})
            })
            .on_native_event(move |event: &NativeEvent| {
                if event.data["scope"].as_str() != Some(scope.get_untracked().as_str()) { return; }
                if event.data["action"] == "visible" {
                    timeline.viewable.set(event.data["start"].as_u64().zip(event.data["end"].as_u64()).map(|(start,end)| (start as usize,end as usize)));
                } else if event.data["action"] == "scroll" {
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
                    handle(timeline, opened, surroundings, saver, &event.data);
                }
            }).flex_1().min_h_0())
}

fn handle(
    timeline: Timeline,
    opened: Signal<HashSet<String>>,
    surroundings: crate::rich_text::Surroundings,
    saver: crate::plans::Saver,
    data: &Value,
) {
    let connection = surroundings.connection;
    let action = data["action"].as_str().unwrap_or_default();
    if action == "copyText" {
        if let Some(text) = data["text"].as_str() {
            set_clipboard(text);
        }
        return;
    }
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
    handle_row(timeline, opened, surroundings, saver, data, row);
}

fn handle_row(
    timeline: Timeline,
    opened: Signal<HashSet<String>>,
    surroundings: crate::rich_text::Surroundings,
    saver: crate::plans::Saver,
    data: &Value,
    row: Row,
) {
    let connection = surroundings.connection;
    let action = data["action"].as_str().unwrap_or_default();
    match action {
        "image" => {
            let source = data["text"].as_str().unwrap_or_default().to_owned();
            let client = connection.client.get();
            let host = connection.host.get_untracked();
            let cwd = connection
                .composer
                .with_untracked(|p| p.controls.navigation.cwd.clone());
            surroundings
                .preview
                .load(crate::media::load_image(client, host, cwd, source));
        }
        "setupTerminal" | "workLocally" | "cancelSetup" => {
            if let Some(setup) = row.preparation {
                match action {
                    "setupTerminal" => {
                        if let Some(terminal) = setup.terminal {
                            surroundings.workspace.open_setup_terminal(terminal);
                        }
                    }
                    "workLocally" => {
                        if let Some(retry) = setup.work_locally {
                            connection.dispatch(Intent::RetryPreparation(retry));
                        }
                    }
                    "cancelSetup" => {
                        if let Some(cancel) = setup.cancel {
                            connection.dispatch(Intent::Interrupt(cancel));
                        }
                    }
                    _ => {}
                }
            }
        }
        "copy" => set_clipboard(row.body.to_string()),
        "edit" => {
            if let Some(turn) = row.edit_turn {
                super::edit_from_here(connection, turn);
            }
        }
        "restore" | "discard" => {
            if let Some(id) = row.unresolved {
                connection.dispatch(if action == "restore" {
                    Intent::RestoreUnknownSubmission {
                        client_user_message_id: id.into(),
                    }
                } else {
                    Intent::DiscardUnknownSubmission {
                        client_user_message_id: id.into(),
                    }
                });
                if action == "restore" {
                    after_answer(move || connection.composer_ref.focus());
                }
            }
        }
        "retry" => {
            if let Some(retry) = row.retry {
                connection.dispatch(Intent::RetryPreparation(retry));
            }
        }
        "agent" => {
            if let Some(target) = row.agent.and_then(|agent| agent.target) {
                connection.dispatch(Intent::ReadThread(op::ReadThread::open(target)));
            }
        }
        "savePlan" | "downloadPlan" => {
            let (filename, text) = agent_core::presentation::conversation::plan_export(&row.note);
            if action == "downloadPlan" {
                saver.download(filename, text);
            } else {
                saver.open(
                    connection.host.get_untracked(),
                    connection
                        .composer
                        .with_untracked(|p| p.controls.navigation.cwd.clone()),
                    text,
                );
            }
        }
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
                toggle_item(opened, connection, item);
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

// The transcript owns cached files while native views consume their pixels.
type Images = HashMap<String, Option<(crate::shared::Shared<crate::client::LocalFile>, String)>>;
fn image_resources(
    connection: Connection,
    scope: Memo<String>,
    rows: Memo<Vec<Value>>,
) -> Signal<Images> {
    let files = signal(Images::new());
    effect(move || {
        scope.get();
        files.set(HashMap::new());
    });
    effect(move || {
        fn sources(parts: &[crate::rich_text::Part], output: &mut Vec<String>) {
            for part in parts {
                match part {
                    crate::rich_text::Part::Image { source, .. } => output.push(source.clone()),
                    crate::rich_text::Part::Table { rows, .. } => {
                        for cell in rows.iter().flatten() {
                            sources(cell, output);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut images = Vec::new();
        rows.with(|rows| {
            for row in rows {
                images.extend(
                    row["images"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
                if let Ok(parts) =
                    serde_json::from_value::<Vec<crate::rich_text::Part>>(row["parts"].clone())
                {
                    sources(&parts, &mut images);
                }
            }
        });
        for source in images {
            if files.with_untracked(|files| files.contains_key(&source)) {
                continue;
            }
            files.update(|files| {
                files.insert(source.clone(), None);
            });
            let expected = scope.get();
            let client = connection.client.get();
            let host = connection.host.get_untracked();
            let cwd = connection
                .composer
                .with_untracked(|p| p.controls.navigation.cwd.clone());
            spawn(async move {
                let loaded = crate::media::load_image(client, host, cwd, source.clone()).await;
                if let Ok(file) = loaded {
                    let path = file.path.clone();
                    let data = blocking(move || std::fs::read(path)).await;
                    if let Ok(Ok(data)) = data {
                        use base64::Engine;
                        if scope.try_get().as_ref() == Some(&expected) {
                            files.update(|files| {
                                files.insert(
                                    source,
                                    Some((
                                        file,
                                        base64::engine::general_purpose::STANDARD.encode(data),
                                    )),
                                );
                            });
                        }
                    }
                }
            });
        }
    });
    files
}

fn toggle_item(
    opened: Signal<HashSet<String>>,
    connection: Connection,
    item: crate::activity::Item,
) {
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
