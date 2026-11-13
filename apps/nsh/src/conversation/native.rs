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
                let open = row.preparation.is_some() && opened.contains(&row.id)
                    || row
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
    let icon = match action {
        "copy" | "copyPlan" | "copyError" => Some("copy"),
        "fork" => Some("fork"),
        "edit" => Some("edit"),
        "planMenu" => Some("more"),
        "fold" | "group" | "item" | "setupDetails" => Some(if title.starts_with("▾ ") {
            "chevronDown"
        } else {
            "chevronRight"
        }),
        _ => None,
    };
    let disclosure = matches!(action, "fold" | "group" | "item" | "setupDetails");
    json!({"action":action,"title":if disclosure {title.get(4..).unwrap_or(title)} else {title},"icon":icon,"iconOnly":matches!(action,"copy" | "copyError" | "fork" | "edit" | "planMenu"),"disclosure":disclosure})
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
        format!("{}\n\n{}", row.body, row.note)
    } else {
        row.body.to_string()
    };
    if matches!(
        row.shape,
        Shape::Fold { .. } | Shape::Group { .. } | Shape::Actions
    ) {
        body.clear();
    }
    if row.status == Some(crate::activity::Status::Thinking) {
        body = tr("思考中").into();
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
        for step in setup.steps.iter().filter(|_| setup.show_steps) {
            body.push_str(&format!(
                "{} · {} {}\n",
                match step.state.as_str() {
                    "done" => "✓",
                    "running" => "●",
                    "pending" => "○",
                    "skipped" => "−",
                    _ => "!",
                },
                step.label,
                step.detail.as_deref().unwrap_or_default()
            ));
            if !step.tail.is_empty() {
                body.push_str(&format!("\n```\n{}\n```\n", step.tail.join("\n")));
            }
        }
        for detail in setup.details.iter().filter(|_| open) {
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
    if row.error.is_some() {
        buttons.push(button("copyError", tr("エラーの詳細をコピー")));
    }
    if row.retry.is_some() {
        buttons.push(button("retry", tr("再試行")));
    }
    let file_paths = crate::rich_text::file_paths(&blocks);
    let parts = crate::rich_text::indexed(crate::rich_text::parts(
        blocks,
        &crate::rich_text::diagrams(&body),
        &|file| chips.map(|chips| chips.chip(file)),
    ));
    let labels = json!({"copyCode":tr("コードをコピー"),"copyTable":tr("表をコピー"),"copyMarkdown":tr("Markdownをコピー"),"copyCsv":tr("CSVをコピー"),"wrap":tr("折り返し"),"unwrap":tr("折り返し解除"),"expand":tr("全文を表示"),"collapse":tr("折りたたむ"),"copied":tr("コピーしました"),"retry":tr("再試行"),"loadingImage":tr("画像を読み込み中…"),"openImage":tr("画像を拡大"),"openFile":tr("ファイルを開く"),"source":tr("ソース"),"diagram":tr("図を表示"),"queued":tr("キューから送信"),"steer":tr("追加の指示")});
    let document = json!({"blocks":[],"secondary":row.kind == Kind::Activity,"role":if row.kind == Kind::User {"user"} else {"assistant"},"target":target,"sideChat":side_chat,"maxWidth":LANE_WIDTH,"labels":labels,"menu":{"quote":tr("チャットに追加"),"explain":tr("詳細を表示"),"sideChat":tr("サイドチャットで質問")}});
    json!({
        "id":row.id,"user":row.kind == Kind::User,"gap":row.gap,"buttons":buttons,"parts":parts,"images":row.images,"filePaths":file_paths,"media":{},
        "collapsible":row.kind == Kind::User && agent_core::presentation::conversation::should_collapse_user_message(&row.body),
        "trailing":matches!(row.shape,Shape::Actions),"fold":matches!(row.shape,Shape::Fold{..} | Shape::Working),"time":row.time,"queued":row.queued,"steer":row.steer,
        "busy":row.streaming || row.status.is_some() || row.detail.as_ref().is_some_and(|item|item.live),"document":document
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
    ) && (row.kind != Kind::Activity || matches!(row.shape, Shape::Actions))
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
        let mut menu = button("planMenu", tr("計画の操作"));
        menu["menuItems"] = json!([
            {"id":"copyPlan","title":tr("クリップボードにコピー")},
            {"id":"downloadPlan","title":tr("Markdownとして保存…")},
            {"id":"savePlan","title":tr("ワークスペースに保存…")}
        ]);
        buttons.push(menu);
    }
    if row
        .agent
        .as_ref()
        .is_some_and(|agent| agent.target.is_some())
    {
        buttons.push(button("agent", &row.body));
    }
    if let Some(setup) = &row.preparation {
        buttons.push(button(
            "setupDetails",
            &format!("{} {}", if open { "▾" } else { "▸" }, tr("詳細")),
        ));
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
    buttons.sort_by_key(|button| match (row.kind, button["action"].as_str()) {
        (Kind::User, Some("edit")) | (_, Some("fork")) => 0,
        (_, Some("copy")) => 1,
        _ => 2,
    });
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
        "history": next["history"], "notice": next["notice"], "opening":next["opening"], "latest": next["latest"], "lane":next["lane"], "padding":next["padding"]
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
    let agents = stored(HashSet::new());
    effect(move || {
        scope.get();
        timeline.following.set(true);
        timeline.expanded.set(HashMap::new());
        timeline.groups.set(HashSet::new());
        timeline.inset.set(timeline.covered.get_untracked());
        opened.set(HashSet::new());
        agents.with(HashSet::clear);
        surroundings.find.reset_position();
    });
    let clock = signal(std::time::SystemTime::now());
    spawn(async move {
        loop {
            timer(std::time::Duration::from_secs(1)).await;
            if timeline.rows.with_untracked(|rows| {
                rows.iter().any(|row| {
                    row.progress.is_some()
                        || row.agent.is_some()
                        || row
                            .preparation
                            .as_ref()
                            .is_some_and(|setup| setup.in_progress)
                })
            }) {
                clock.set(std::time::SystemTime::now());
            }
        }
    });
    effect(move || {
        timeline.rows.with(|rows| {
            for row in rows.iter() {
                if let Some(target) = row.agent.as_ref().and_then(|agent| agent.target.clone())
                    && agents.with(|all| all.insert(target.clone()))
                {
                    connection.dispatch(Intent::ReadThread(op::ReadThread::new(target)));
                }
            }
        });
    });
    let projected = computed(move || {
        let (cwd, home) = connection
            .publication
            .with(|p| (p.composer.controls.navigation.cwd.clone(), p.context.home()));
        let now = clock
            .get()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let rows = timeline.rows.with(|rows| {
            rows.iter()
                .cloned()
                .map(|mut row| {
                    if let Some(setup) = &row.preparation {
                        row.time = setup.started_at_ms.map(|start| {
                            agent_core::presentation::conversation::preparation_elapsed(
                                crate::l10n::language(),
                                start,
                                setup.ended_at_ms,
                                (now * 1000.0) as u64,
                            )
                        });
                    }
                    if let Some(progress) = &row.progress {
                        row.body = progress.label(crate::l10n::language(), now).into();
                    }
                    if let Some(agent) = &row.agent {
                        let (name, elapsed) = connection.publication.with(|p| {
                            let child = agent
                                .target
                                .as_ref()
                                .and_then(|target| p.snapshot.sessions.conversations.get(target))
                                .map(|child| &**child);
                            (
                                agent.name(child),
                                agent_core::presentation::agents::subagent_elapsed(
                                    crate::l10n::language(),
                                    child,
                                    now,
                                ),
                            )
                        });
                        row.body = format!("{} {}", name, elapsed.unwrap_or_default()).into();
                    }
                    row
                })
                .collect::<Vec<_>>()
        });
        let mut displayed = opened.with(|opened| {
            documents.with(|cache| {
                cache.project(
                    &rows,
                    opened,
                    &target.get(),
                    &cwd,
                    home.as_deref(),
                    &chips.get(),
                )
            })
        });
        for (row, value) in rows.iter().zip(&mut displayed) {
            operation_display(connection, row, opened, value);
        }
        displayed
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
    let retry_images = signal(0_u64);
    let media = image_resources(connection, scope, projected, retry_images);
    nsh_core::element::view().flex_1().min_h_0().label(tr("会話の履歴"))

        .on_layout(move |bounds: &Bounds<nsh_core::geometry::Points>| timeline.width.set(bounds.size.width.0))
        .pb(move || px(timeline.covered.get()))
        .child(saver.view(connection))
        .child(native("bex.conversation")
            .element_ref(&timeline.list)
            .prop("data", move || {
                let (history, notice, opening) = connection.publication.with(|p| {
                    use agent_core::presentation::context::ConversationOpening;
                    (p.context.history(),p.context.thread_error(),match p.context.opening() {
                        Some(ConversationOpening::Loading) => json!({"loading":true,"text":tr("会話を読み込み中…")}),
                        Some(ConversationOpening::Failed{error}) => json!({"loading":false,"text":error.to_string(),"retry":tr("再試行")}),
                        None => Value::Null,
                    })
                });
                let mut rows = projected.get();
                let query = surroundings.find.highlighted();
                for (index,row) in rows.iter_mut().enumerate() {
                    row["top"] = json!(if index == 0 {24} else {0});
                    if let Ok(mut parts) = serde_json::from_value::<Vec<crate::rich_text::Part>>(row["parts"].clone()) {
                        crate::rich_text::highlight(&mut parts, &query, surroundings.find.selection(row["id"].as_str().unwrap_or_default()));
                        row["parts"] = json!(parts);
                    }
                    row["document"]["findKey"] = json!(surroundings.find.key());
                    row["media"] = media.with(|files| json!(files.iter().filter_map(|(source, file)| match file { Image::Ready(_,data) => Some((source.clone(),data.clone())), _=>None }).collect::<HashMap<_,_>>()));
                    row["document"]["mediaErrors"] = media.with(|files| json!(files.iter().filter_map(|(source, file)| match file { Image::Failed(error) => Some((source.clone(),error.clone())), _=>None }).collect::<HashMap<_,_>>()));
                }
                json!({"scope": scope.get(), "rows": rows, "lane":LANE_WIDTH,"padding":lane_padding(window().width.0), "following": timeline.following.get(),
                    "inset": (timeline.inset.get() - timeline.covered.get()).max(0.0), "notice": notice,"opening":opening,
                    "history": history.map(|history| json!({"title": history.load_label, "enabled": !history.loading})),
                    "latest": tr("最新のメッセージへ")})
            })
            .on_native_event(move |event: &NativeEvent| {
                if event.data["scope"].as_str() != Some(scope.get_untracked().as_str()) { return; }
                if event.data["action"] == "retryImage" {
                    if let Some(source) = event.data["text"].as_str() { media.update(|files| {files.remove(source);}); retry_images.update(|serial|*serial = serial.wrapping_add(1)); }
                } else if event.data["action"] == "visible" {
                    timeline.viewable.set(event.data["start"].as_u64().zip(event.data["end"].as_u64()).map(|(start,end)| (start as usize,end as usize)));
                } else if event.data["action"] == "scroll" {
                    let now = agent_core::presentation::conversation::ScrollPosition {
                        offset: event.data["offset"].as_f64().unwrap_or(0.0) as f32,
                        viewport: event.data["viewport"].as_f64().unwrap_or(0.0) as f32,
                        content: event.data["content"].as_f64().unwrap_or(0.0) as f32,
                    };
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
    if action == "retryOpen" {
        if let Some(thread) = connection
            .context
            .with_untracked(|p| p.navigation().thread_id)
        {
            connection.dispatch(Intent::ReadThread(op::ReadThread::open(thread)));
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
            surroundings.preview.load_image(
                connection,
                source,
                (!row.images.is_empty()).then_some(row.images),
            );
        }
        "setupTerminal" | "workLocally" | "cancelSetup" => {
            if let Some(setup) = row.preparation {
                setup_action(surroundings.workspace, connection, action, setup);
            }
        }
        "copyError" => {
            if let Some(error) = row.error {
                set_clipboard(format!(
                    "{}\n{}\n{}",
                    error.title,
                    error.message,
                    error.details.unwrap_or_default()
                ));
            }
        }
        "setupDetails" => opened.update(|all| {
            if !all.remove(&row.id) {
                all.insert(row.id);
            }
        }),
        "retryWork" => {
            if let Some(load) = row.activity.and_then(|activity| activity.load_items) {
                connection.dispatch(Intent::LoadTurnItems(load));
            }
        }
        "retryItem" => {
            if let Some(item) = row.detail {
                request_item(connection, item);
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
        "savePlan" | "downloadPlan" | "copyPlan" => {
            let (filename, text) = agent_core::presentation::conversation::plan_export(&row.note);
            if action == "copyPlan" {
                set_clipboard(text);
            } else if action == "downloadPlan" {
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
#[derive(Clone, PartialEq)]
enum Image {
    Loading,
    Ready(crate::shared::Shared<crate::client::LocalFile>, String),
    Failed(String),
}
type Images = HashMap<String, Image>;
fn image_resources(
    connection: Connection,
    scope: Memo<String>,
    rows: Memo<Vec<Value>>,
    retry: Signal<u64>,
) -> Signal<Images> {
    let files = signal(Images::new());
    let current_scope = stored(None::<String>);
    effect(move || {
        let scope_now = scope.get();
        if current_scope.replace(Some(scope_now.clone())).as_ref() != Some(&scope_now) {
            files.set(HashMap::new());
        }
        retry.get();
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
                files.insert(source.clone(), Image::Loading);
            });
            let expected = scope.get();
            let client = connection.client.get();
            let host = connection.host.get_untracked();
            let cwd = connection
                .composer
                .with_untracked(|p| p.controls.navigation.cwd.clone());
            spawn(async move {
                let result = match crate::media::load_image(client, host, cwd, source.clone()).await
                {
                    Ok(file) => {
                        let path = file.path.clone();
                        match blocking(move || image_png(&path)).await {
                            Ok(Ok(data)) => Image::Ready(file, data),
                            Ok(Err(error)) => Image::Failed(error),
                            Err(error) => Image::Failed(error.to_string()),
                        }
                    }
                    Err(error) => Image::Failed(error),
                };
                if scope.try_get().as_ref() == Some(&expected) {
                    files.update(|files| {
                        files.insert(source, result);
                    });
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
    if opening && item.source.data.deferred {
        request_item(connection, item);
    }
}
fn request_item(connection: Connection, item: crate::activity::Item) {
    if let (Some(thread_id), Some(item_id)) = (
        connection
            .context
            .with_untracked(|p| p.navigation().thread_id),
        item.source.data.native_id.clone(),
    ) {
        connection.dispatch(Intent::ReadItem(op::ReadItem {
            thread_id,
            turn_id: item.turn,
            item_id,
        }));
    }
}

fn setup_action(
    workspace: crate::workbench::Workbench,
    connection: Connection,
    action: &str,
    setup: agent_core::presentation::conversation::PreparationPresentation,
) {
    match action {
        "setupTerminal" => {
            if let Some(terminal) = setup.terminal {
                workspace.open_setup_terminal(terminal);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_preparation_keeps_steps_details_terminal_cancel_and_retry_actions() {
        use agent_core::presentation::conversation::{
            PreparationDetail, PreparationPresentation, PreparationStep, SetupTerminal,
        };
        let thread_id = agent_protocol::session::SessionRef::new(
            agent_protocol::session::ProviderKind::Codex,
            "thread".into(),
        )
        .unwrap();
        let retry = agent_protocol::operations::RetryPreparation {
            thread_id: thread_id.clone(),
            work_locally: false,
        };
        let mut row = Row::new(
            "setup".into(),
            Kind::Activity,
            Shape::Preparation,
            "ワークツリーを準備しています",
        );
        row.retry = Some(retry.clone());
        row.preparation = Some(PreparationPresentation {
            text: row.body.to_string(),
            in_progress: true,
            shimmer: true,
            retry: Some(retry.clone()),
            steps: vec![PreparationStep {
                id: "setup".into(),
                label: "Run setup script".into(),
                state: "running".into(),
                detail: Some("installing".into()),
                tail: vec!["npm install".into()],
                started_at_ms: Some(1000),
                ended_at_ms: None,
            }],
            show_steps: true,
            details: vec![PreparationDetail {
                label: "Workspace".into(),
                value: "/repo/worktree".into(),
            }],
            started_at_ms: Some(1000),
            ended_at_ms: None,
            cancel: Some(agent_protocol::operations::Interrupt {
                thread_id,
                turn_id: "turn".into(),
            }),
            terminal: Some(SetupTerminal {
                handle: "setup-terminal".into(),
                cwd: "/repo/worktree".into(),
            }),
            work_locally: Some(retry),
            error: Some("Setup failure remains visible".into()),
        });
        let closed = project_row(&row, false, "target", "/repo", None, true, None);
        let actions: Vec<_> = closed["buttons"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|button| button["action"].as_str())
            .collect();
        for action in [
            "setupDetails",
            "setupTerminal",
            "cancelSetup",
            "workLocally",
            "retry",
        ] {
            assert!(actions.contains(&action), "{action}");
        }
        let visible = closed["parts"].to_string();
        assert!(
            visible.contains("Run setup script")
                && visible.contains("npm install")
                && visible.contains("Setup failure remains visible")
        );
        assert!(!visible.contains("/repo/worktree"));
        let opened = project_row(&row, true, "target", "/repo", None, true, None);
        assert!(opened["parts"].to_string().contains("/repo/worktree"));
    }

    #[test]
    fn a_new_scope_replaces_identical_row_ids_and_targets() {
        let previous = json!({"id":"same", "document":{"target":"old"}});
        let delivered = BTreeMap::from([("same".into(), previous)]);
        let row = json!({"id":"same", "document":{"target":"new"}});
        let delta = changes(
            "old",
            &["same".into()],
            &delivered,
            &json!({"scope":"new", "rows":[row]}),
        );
        assert_eq!(delta["reset"], true);
        assert_eq!(delta["order"], json!(["same"]));
        assert_eq!(delta["changed"][0]["document"]["target"], "new");
    }

    #[test]
    fn native_updates_include_only_changed_rows_and_explicit_removals() {
        let a = json!({"id":"a", "body":"unchanged"});
        let b = json!({"id":"b", "body":"previous"});
        let delivered = BTreeMap::from([("a".into(), a.clone()), ("b".into(), b)]);
        let next = json!({"scope":"same", "rows":[a], "following":false, "inset":42});
        let delta = changes("same", &["a".into(), "b".into()], &delivered, &next);
        assert_eq!(delta["changed"], json!([]));
        assert_eq!(delta["removed"], json!(["b"]));
        assert_eq!(delta["order"], json!(["a"]));
        assert_eq!(delta["following"], false);
        assert_eq!(delta["inset"], 42);
        let updated = json!({"id":"b","body":"streamed text"});
        let changed = changes(
            "same",
            &["a".into(), "b".into()],
            &delivered,
            &json!({"scope":"same","rows":[next["rows"][0],updated]}),
        );
        assert_eq!(changed["changed"], json!([updated]));
        assert!(changed["order"].is_null());
        let reset = changes("another", &["a".into()], &delivered, &next);
        assert_eq!(reset["changed"], next["rows"]);
        let unchanged = changes("same", &["a".into()], &delivered, &next);
        assert!(unchanged["order"].is_null());
    }

    #[test]
    fn native_parts_keep_table_copy_formats_code_and_image_sources() {
        let row = Row::new(
            "row".into(),
            Kind::Response,
            Shape::Message,
            "| Name | Value |\n| --- | --- |\n| 日本語 | **value** |\n\n```rust\nlet x = 1;\n```\n\n![image](output.png)",
        );
        let value = project_row(&row, false, "target", "/repo", None, true, None);
        let parts = value["parts"].as_array().unwrap();
        let table = parts.iter().find_map(|part| part.get("Table")).unwrap();
        assert!(table["source"].as_str().unwrap().contains("**value**"));
        assert!(
            table["csv"]
                .as_str()
                .unwrap()
                .contains("\"日本語\",\"value\"")
        );
        let code = parts.iter().find_map(|part| part.get("Code")).unwrap();
        assert!(code["source"].as_str().unwrap().contains("let x = 1;"));
        assert!(parts.iter().any(|part| {
            part["Image"]["source"]
                .as_str()
                .is_some_and(|source| source.ends_with("output.png"))
        }));
    }
}

fn image_png(path: &std::path::Path) -> Result<String, String> {
    use base64::Engine;
    let data = std::fs::read(path).map_err(|error| error.to_string())?;
    let image = image::load_from_memory(&data).map_err(|error| error.to_string())?;
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png.into_inner()))
}

fn operation_display(
    connection: Connection,
    row: &Row,
    opened: Signal<HashSet<String>>,
    value: &mut Value,
) {
    let mut notices = Vec::new();
    let mut retry = None;
    connection.publication.with(|p| {
        if let Some(turn) = &row.fork {
            let busy = p.context.fork_phase(turn.clone()) == Some(op::OperationPhase::Running);
            for button in value["buttons"].as_array_mut().into_iter().flatten() {
                if button["action"] == "fork" {
                    button["enabled"] = json!(!busy);
                }
            }
            if busy {
                notices.push(crate::rich_text::plain(tr("分岐を作成中…").into(), None));
            }
            if let Some(error) = p.context.fork_error(turn.clone()) {
                notices.push(crate::rich_text::plain(error.to_string(), Some(0xff8e86)));
            }
        }
        for button in value["buttons"].as_array_mut().into_iter().flatten() {
            if button["action"] == "edit" {
                button["enabled"] =
                    json!(p.composer.controls.running_turn.is_none() && !p.context.rewinding());
            }
            if button["action"] == "planMenu" {
                for item in button["menuItems"].as_array_mut().into_iter().flatten() {
                    if item["id"] == "savePlan" {
                        item["enabled"] = json!(
                            p.composer.controls.connected
                                && !p.composer.controls.navigation.cwd.is_empty()
                        );
                    }
                }
            }
        }
        if matches!(row.shape, Shape::Fold { open: true })
            && let Some(load) = row
                .activity
                .as_ref()
                .and_then(|activity| activity.load_items.as_ref())
            && let Some(error) = p.context.turn_items_error(load.turn_id.clone())
        {
            notices.push(crate::rich_text::plain(error.to_string(), Some(0xff8e86)));
            retry = Some("retryWork");
        }
        if let Some(item) = &row.detail
            && opened.with(|all| all.contains(&item.source.data.id))
            && let (Some(thread_id), Some(item_id)) = (
                p.context.navigation().thread_id,
                item.source.data.native_id.clone(),
            )
        {
            let key = op::OperationKey::Item {
                item: op::ReadItem {
                    thread_id,
                    turn_id: item.turn.clone(),
                    item_id,
                },
            };
            if p.snapshot.operation_running(key.clone()) {
                value["busy"] = json!(true);
            }
            if let Some(error) = p.snapshot.operation_error(key) {
                notices.push(crate::rich_text::plain(error.to_string(), Some(0xff8e86)));
                retry = Some("retryItem");
            }
        }
    });
    if let Some(parts) = value["parts"].as_array_mut() {
        parts.extend(notices.into_iter().map(|part| json!(part)));
    }
    if let Some(action) = retry
        && let Some(buttons) = value["buttons"].as_array_mut()
    {
        buttons.push(button(action, tr("再試行")));
    }
}
