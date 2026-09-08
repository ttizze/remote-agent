use super::*;
use gpui_kit::component::{
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem},
    tab::{Tab as UiTab, TabBar},
};

const CHAT_WIDTH: f32 = 780.;

impl Desktop {
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        let label = label.into();
        Button::new(id.into())
            .accessibility_label(label.clone())
            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
            .small()
            .ghost()
            .on_click(cx.listener(move |s, _, w, cx| {
                action(s, w, cx);
                cx.notify();
            }))
    }
    fn icon_button(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .icon(icon)
            .small()
            .ghost()
            .tooltip(label)
            .accessibility_label(label)
            .on_click(cx.listener(move |s, _, w, cx| {
                action(s, w, cx);
                cx.notify();
            }))
    }
    fn image(&mut self, source: &str, _cx: &mut Context<Self>) -> AnyElement {
        let key = format!("{}:{}:{}", self.remote, self.cwd, source);
        if !self.images.contains_key(&key) {
            self.images.insert(
                key.clone(),
                ImageState {
                    path: None,
                    error: None,
                },
            );
            let source = source.to_owned();
            let cwd = self.cwd.clone();
            let remote = self.remote.clone();
            let manager = self.manager.clone();
            let destination = self
                .image_dir
                .path()
                .join(format!("image-{}", self.images.len()));
            let returned = key.clone();
            self.work(false,move||{
                let result=(||->Result<String,String>{
                    if source.starts_with("https://")||source.starts_with("http://"){return Ok(source);}
                    if source.starts_with("data:image/"){let (header,data)=source.split_once(',').ok_or("画像データが不正です")?;if !header.ends_with(";base64"){return Err("未対応の画像データです".into());}let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;std::fs::write(&destination,bytes).map_err(|e|e.to_string())?;return Ok(destination.to_string_lossy().into_owned());}
                    let path=if source.starts_with("file:"){url::Url::parse(&source).map_err(|e|e.to_string())?.to_file_path().map_err(|_|"画像パスが不正です")?}else{if url::Url::parse(&source).is_ok(){return Err("未対応の画像URLです".into());}Path::new(&cwd).join(&source)};
                    if remote.is_empty(){return Ok(path.to_string_lossy().into_owned());}
                    manager.request("host/transfer",json!({"profileId":remote,"direction":"download","source":path,"destination":destination}))?;Ok(destination.to_string_lossy().into_owned())
                })();Ok(match result{Ok(path)=>json!({"path":path}),Err(error)=>json!({"error":error})})
            },move|s,v,_,_|{if let Some(image)=s.images.get_mut(&returned){image.path=v["path"].as_str().map(|s|if s.starts_with("/"){std::path::PathBuf::from(s).into()}else{ImageSource::from(s.to_owned())});image.error=v["error"].as_str().map(str::to_owned);}s.list.remeasure();});
        }
        let state = &self.images[&key];
        if let Some(path) = &state.path {
            img(path.clone())
                .w_full()
                .h(px(320.))
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            div()
                .text_sm()
                .text_color(rgb(0xaaaaaa))
                .child(
                    state
                        .error
                        .clone()
                        .unwrap_or_else(|| "画像を読み込み中…".into()),
                )
                .into_any_element()
        }
    }
    fn markdown(&mut self, id: String, source: &str, cx: &mut Context<Self>) -> AnyElement {
        if self
            .markdown_cache
            .get(&id)
            .is_none_or(|cached| cached.source != source)
        {
            let mut images = Vec::new();
            let mut rendered = String::with_capacity(source.len());
            let mut previous = 0;
            let mut image_start = None;
            for (event, range) in
                pulldown_cmark::Parser::new_ext(source, pulldown_cmark::Options::all())
                    .into_offset_iter()
            {
                match event {
                    pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image {
                        dest_url, ..
                    }) => {
                        images.push(dest_url.into_string());
                        image_start = Some(range.start);
                    }
                    pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Image) => {
                        if let Some(start) = image_start.take() {
                            rendered.push_str(&source[previous..start]);
                            previous = range.end;
                        }
                    }
                    _ => {}
                }
            }
            rendered.push_str(&source[previous..]);
            self.markdown_cache.insert(
                id.clone(),
                MarkdownContent {
                    source: source.to_owned(),
                    rendered: rendered.into(),
                    images: images.into(),
                },
            );
        }
        let cached = &self.markdown_cache[&id];
        let images = cached.images.clone();
        let mut body = v_flex().gap_3().w_full().child(
            TextView::markdown(SharedString::from(id), cached.rendered.clone())
                .selectable(true)
                .on_link_click(|url, _, _, cx| {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        cx.open_url(url);
                    }
                }),
        );
        for source in images.iter() {
            body = body.child(self.image(&source, cx));
        }
        body.into_any_element()
    }
    fn diff(&mut self, id: String, patch: &str, cx: &mut Context<Self>) -> AnyElement {
        let view = self
            .diffs
            .entry(id.clone())
            .or_insert_with(|| {
                cx.new(|_| DiffView::new(patch.to_owned().into(), id == "workspace-patch"))
            })
            .clone();
        view.update(cx, |s, cx| s.set_source(patch, cx));
        view.into_any_element()
    }
    fn item(&mut self, item: &Value, cx: &mut Context<Self>) -> AnyElement {
        let id = text(item, "id").to_owned();
        let kind = text(item, "type");
        let expanded = self.expanded_items.contains(&id);
        match kind {
            "agentMessage" | "exitedReviewMode" => self.markdown(
                id,
                item["text"]
                    .as_str()
                    .or(item["review"].as_str())
                    .unwrap_or(""),
                cx,
            ),
            "userMessage" => {
                let mut body = v_flex()
                    .gap_3()
                    .max_w(px(560.))
                    .p_4()
                    .rounded(px(18.))
                    .bg(rgb(0x303030));
                if item["content"].is_array() {
                    for (i, part) in array(&item["content"]).iter().enumerate() {
                        body = body.child(match text(part, "type") {
                            "text" => TextView::markdown(
                                SharedString::from(format!("{id}-{i}")),
                                literal(text(part, "text")),
                            )
                            .selectable(true)
                            .into_any_element(),
                            "localImage" => self.image(text(part, "path"), cx),
                            "image" => self.image(text(part, "url"), cx),
                            _ => {
                                let path = text(part, "path").to_owned();
                                self.button(
                                    format!("{id}-{i}"),
                                    if path.is_empty() {
                                        part.to_string()
                                    } else {
                                        basename(&path)
                                    },
                                    cx,
                                    move |s, _, _| {
                                        if !path.is_empty() {
                                            s.download(path.clone());
                                        }
                                    },
                                )
                                .into_any_element()
                            }
                        });
                    }
                } else {
                    body = body.child(
                        TextView::markdown(
                            SharedString::from(id.clone()),
                            literal(text(item, "text")),
                        )
                        .selectable(true),
                    );
                }
                h_flex()
                    .w_full()
                    .justify_end()
                    .my_4()
                    .child(body)
                    .into_any_element()
            }
            "commandExecution" | "reasoning" => {
                let label = if kind == "reasoning" {
                    "思考"
                } else {
                    item["command"].as_str().unwrap_or("コマンド")
                };
                let toggle = id.clone();
                let mut body = v_flex().gap_2().child(self.button(
                    format!("expand-{id}"),
                    format!("{} {label}", if expanded { "⌄" } else { "›" }),
                    cx,
                    move |s, _, _| {
                        toggle_set(&mut s.expanded_items, &toggle);
                        s.list.pause_following_tail();
                        s.list.remeasure();
                    },
                ));
                if expanded {
                    let content = if kind == "reasoning" {
                        if item["summary"].is_array() {
                            array(&item["summary"])
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            item["summary"]
                                .as_str()
                                .or(item["text"].as_str())
                                .unwrap_or("")
                                .to_owned()
                        }
                    } else {
                        text(item, "aggregatedOutput").into()
                    };
                    body = body
                        .child(
                            TextView::markdown(
                                SharedString::from(format!("output-{id}")),
                                fenced(&content, ""),
                            )
                            .selectable(true),
                        )
                        .child(div().text_sm().text_color(rgb(0x999999)).child(format!(
                                "{} {}",
                                text(item, "status"),
                                item.get("exitCode")
                                    .map(|v| format!("exit {v}"))
                                    .unwrap_or_default()
                            )));
                }
                body.into_any_element()
            }
            "fileChange" => {
                let mut body = v_flex().gap_3();
                for (i, change) in array(&item["changes"]).iter().enumerate() {
                    let path = text(change, "path").to_owned();
                    let toggle = id.clone();
                    let row = h_flex()
                        .gap_2()
                        .child(self.button(
                            format!("change-{id}-{i}"),
                            format!("{} {}", if expanded { "⌄" } else { "›" }, basename(&path)),
                            cx,
                            move |s, _, _| {
                                toggle_set(&mut s.expanded_items, &toggle);
                                s.list.pause_following_tail();
                                s.list.remeasure();
                            },
                        ))
                        .child(self.button(
                            format!("download-{id}-{i}"),
                            "ダウンロード",
                            cx,
                            move |s, _, _| s.download(path.clone()),
                        ));
                    body = body.child(row);
                    if expanded {
                        body = body.child(self.diff(
                            format!("diff-{id}-{i}"),
                            text(change, "diff"),
                            cx,
                        ));
                    }
                }
                body.into_any_element()
            }
            _ => {
                let toggle = id.clone();
                v_flex()
                    .gap_2()
                    .child(self.button(
                        format!("unknown-{id}"),
                        format!("{} {kind}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            s.list.pause_following_tail();
                            s.list.remeasure();
                        },
                    ))
                    .when(expanded, |body| {
                        body.child(
                            TextView::markdown(
                                SharedString::from(format!("json-{id}")),
                                fenced(
                                    &serde_json::to_string_pretty(item).unwrap_or_default(),
                                    "json",
                                ),
                            )
                            .selectable(true),
                        )
                    })
                    .into_any_element()
            }
        }
    }
    fn turn(&mut self, turn: &Value, cx: &mut Context<Self>) -> AnyElement {
        let projection = conversation::project(turn, &self.conversation.requests);
        let id = text(turn, "id").to_owned();
        let expanded = self
            .expanded_work
            .get(&id)
            .copied()
            .unwrap_or(!projection.collapsible);
        let mut body = v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4();
        for item in projection.users {
            body = body.child(self.item(item, cx));
        }
        if !projection.work.is_empty() {
            let toggle = id.clone();
            body = body.child(
                h_flex()
                    .w_full()
                    .border_b_1()
                    .border_color(rgb(0x303030))
                    .pb_2()
                    .child(
                        self.button(
                            format!("work-{id}"),
                            format!("{} {}", projection.label, if expanded { "⌄" } else { "›" }),
                            cx,
                            move |s, _, _| {
                                s.expanded_work.insert(toggle.clone(), !expanded);
                                s.list.pause_following_tail();
                                s.list.remeasure();
                            },
                        )
                        .text_color(rgb(0xa0a0a0)),
                    ),
            );
        }
        if expanded || !projection.collapsible {
            for item in projection.work {
                body = body.child(self.item(item, cx));
            }
        }
        for item in projection.finals {
            let value = text(item, "text").to_owned();
            body = body.child(self.item(item, cx)).child(
                h_flex().child(
                    Button::new(SharedString::from(format!("copy-{}", text(item, "id"))))
                        .icon(IconName::Copy)
                        .small()
                        .ghost()
                        .tooltip("回答をコピー")
                        .accessibility_label("回答をコピー")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()))
                        }),
                ),
            );
        }
        if projection.thinking {
            body = body.child(div().text_color(rgb(0x999999)).child("作業中…"));
        }
        if !turn["error"].is_null() {
            body = body.child(
                div().text_color(rgb(0xff8e86)).child(
                    turn["error"]["message"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| turn["error"].to_string()),
                ),
            );
        }
        h_flex()
            .justify_center()
            .w_full()
            .px_6()
            .pb_8()
            .child(body)
            .into_any_element()
    }
    fn request_card(&mut self, request: &Value, cx: &mut Context<Self>) -> AnyElement {
        let key = request["id"].to_string();
        let id = request["id"].clone();
        let params = &request["params"];
        let Some(inputs) = self.requests.get(&key) else {
            return div().into_any_element();
        };
        let mut body = v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x30312a))
            .child("操作の確認・回答")
            .child(
                TextView::markdown(
                    SharedString::from(format!("request-{key}")),
                    fenced(
                        &serde_json::to_string_pretty(params).unwrap_or_default(),
                        "json",
                    ),
                )
                .selectable(true),
            );
        if inputs.sent {
            return body
                .child("回答を送信しました。Host の確認を待っています。")
                .into_any_element();
        }
        if !inputs.questions.is_empty() {
            for q in &inputs.questions {
                let input = q.input.clone();
                body = body.child(q.prompt.clone());
                for (i, option) in q.options.iter().enumerate() {
                    let input = input.clone();
                    let value = option.clone();
                    body = body.child(self.button(
                        format!("answer-{key}-{}-{i}", q.id),
                        option.clone(),
                        cx,
                        move |_, w, cx| input.update(cx, |s, cx| s.set_value(value.clone(), w, cx)),
                    ));
                }
                body = body.child(Input::new(&q.input));
            }
            body = body.child(self.button(
                format!("respond-{key}"),
                "回答を送信",
                cx,
                move |s, _, cx| {
                    let inputs = &s.requests[&key];
                    let mut answers = serde_json::Map::new();
                    for q in &inputs.questions {
                        let value = q.input.read(cx).value();
                        if value.trim().is_empty() {
                            s.error = "すべての質問に回答してください".into();
                            return;
                        }
                        answers.insert(q.id.clone(), json!({"answers":[value.as_ref()]}));
                    }
                    s.respond(id.clone(), json!({"answers":answers}));
                },
            ));
        } else if matches!(
            text(request, "method"),
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        ) {
            let defaults = json!(["accept", "acceptForSession", "decline", "cancel"]);
            let decisions = if params["availableDecisions"].is_array() {
                &params["availableDecisions"]
            } else {
                &defaults
            };
            let mut row = h_flex().gap_2().flex_wrap();
            for (i, decision) in array(decisions).iter().enumerate() {
                let id = id.clone();
                let decision = decision.clone();
                let label = match decision.as_str() {
                    Some("accept") => "許可".into(),
                    Some("acceptForSession") => "このセッションで許可".into(),
                    Some("decline") => "拒否".into(),
                    Some("cancel") => "中止".into(),
                    _ => decision.to_string(),
                };
                row = row.child(self.button(
                    format!("decision-{key}-{i}"),
                    label,
                    cx,
                    move |s, _, _| s.respond(id.clone(), json!({"decision":decision})),
                ));
            }
            body = body.child(row);
        } else if request["method"] == "item/permissions/requestApproval" {
            let denied = id.clone();
            let permissions = params["permissions"].clone();
            body = body
                .child(self.button(
                    format!("allow-{key}"),
                    "今回の権限を許可",
                    cx,
                    move |s, _, _| {
                        s.respond(
                            id.clone(),
                            json!({"permissions":permissions,"scope":"turn"}),
                        )
                    },
                ))
                .child(
                    self.button(format!("deny-{key}"), "拒否", cx, move |s, _, _| {
                        s.respond(denied.clone(), json!({"permissions":{},"scope":"turn"}))
                    }),
                );
        } else {
            body = body
                .child("要求の形式に合わせて回答JSONを入力")
                .child(Textarea::new(&inputs.raw))
                .child(self.button(
                    format!("raw-{key}"),
                    "回答を送信",
                    cx,
                    move |s, _, cx| match serde_json::from_str(
                        s.requests[&key].raw.read(cx).value().as_ref(),
                    ) {
                        Ok(value) => s.respond(id.clone(), value),
                        Err(e) => s.error = e.to_string(),
                    },
                ));
        }
        h_flex()
            .w_full()
            .justify_center()
            .px_6()
            .pb_5()
            .child(body.max_w(px(CHAT_WIDTH)))
            .into_any_element()
    }
    fn model_menu(&self, cx: &Context<Self>) -> AnyElement {
        let models = self.models.clone();
        let current = self.model.clone();
        let entity = cx.entity().downgrade();
        let label = models
            .iter()
            .find(|m| m["model"] == current)
            .map(|m| text(m, "displayName"))
            .filter(|s| !s.is_empty())
            .unwrap_or(if current.is_empty() {
                "モデル"
            } else {
                &current
            })
            .to_owned();
        Button::new("model-select")
            .label(label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                for model in &models {
                    let entity = entity.clone();
                    let value = text(model, "model").to_owned();
                    let label = model["displayName"].as_str().unwrap_or(&value).to_owned();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(value == current)
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |s, cx| {
                                    s.model = value.clone();
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }
    fn host_menu(&self, cx: &Context<Self>) -> AnyElement {
        let mut hosts = vec![(String::new(), "この Mac".to_owned())];
        hosts.extend(
            self.hosts
                .iter()
                .map(|h| (text(h, "id").to_owned(), text(h, "hostName").to_owned())),
        );
        let current = self.remote.clone();
        let entity = cx.entity().downgrade();
        let label = hosts
            .iter()
            .find(|(id, _)| id == &current)
            .map(|(_, name)| name.as_str())
            .unwrap_or("Host")
            .to_owned();
        Button::new("host-select")
            .label(label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, _| {
                for (id, name) in &hosts {
                    let entity = entity.clone();
                    let id = id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(id == current)
                            .on_click(move |_, w, cx| {
                                let _ = entity.update(cx, |s, cx| {
                                    s.switch_host(id.clone(), w, cx);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }
    fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut navigation = SidebarMenu::new()
            .gap_1()
            .child(
                SidebarMenuItem::new("新しいチャット")
                    .icon(IconName::Plus)
                    .disable(!self.connected)
                    .on_click(cx.listener(|s, _, w, cx| {
                        let cwd = if s.cwd.is_empty() {
                            s.projects.first().map(project_root).unwrap_or_default()
                        } else {
                            s.cwd.clone()
                        };
                        s.new_thread(cwd, w, cx);
                        cx.notify();
                    })),
            )
            .child(
                SidebarMenuItem::new("ファイル")
                    .icon(IconName::Folder)
                    .active(self.panel_open && self.panel == Panel::Files)
                    .disable(!self.connected || self.cwd.is_empty())
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.browse(s.cwd.clone());
                        cx.notify();
                    })),
            )
            .child(
                SidebarMenuItem::new("変更")
                    .icon(IconName::Replace)
                    .active(self.panel_open && self.panel == Panel::Diff)
                    .disable(!self.connected || self.cwd.is_empty())
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.panel = Panel::Diff;
                        s.panel_open = true;
                        s.tab = Tab::Chat;
                        s.refresh_review();
                        cx.notify();
                    })),
            );
        navigation = navigation.child(
            SidebarMenuItem::new("設定")
                .icon(IconName::Settings)
                .active(self.tab == Tab::Settings)
                .on_click(cx.listener(|s, _, _, cx| {
                    s.tab = Tab::Settings;
                    s.refresh_manager();
                    s.refresh_worktree_settings();
                    cx.notify();
                })),
        );
        let mut projects = SidebarMenu::new().gap_1();
        for project in &self.projects {
            let id = text(project, "id").to_owned();
            let expanded =
                self.expanded_projects.contains(&id) || !text(&self.query, "searchTerm").is_empty();
            let toggle = id.clone();
            let new_root = project_root(project);
            let entity = cx.entity().downgrade();
            let connected = self.connected;
            projects = projects.child(
                SidebarMenuItem::new(text(project, "name").to_owned())
                    .icon(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::FolderClosed
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        toggle_set(&mut s.expanded_projects, &toggle);
                        cx.notify();
                    }))
                    .suffix(move |_, _| {
                        let entity = entity.clone();
                        let path = new_root.clone();
                        Button::new("new-project-chat")
                            .icon(IconName::Plus)
                            .xsmall()
                            .ghost()
                            .tooltip("このプロジェクトで新しいチャット")
                            .accessibility_label("このプロジェクトで新しいチャット")
                            .disabled(!connected)
                            .on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |s, cx| {
                                    s.new_thread(path.clone(), w, cx);
                                    cx.notify();
                                });
                            })
                    }),
            );
            if expanded {
                for thread in self
                    .threads
                    .iter()
                    .filter(|t| t["projectId"] == project["id"])
                {
                    projects =
                        projects.child(self.thread_button(thread, cx).icon(Icon::empty().size_4()));
                }
                if array(&self.more["moreProjectIds"])
                    .iter()
                    .any(|v| v == &project["id"])
                {
                    projects = projects.child(
                        SidebarMenuItem::new("もっと表示する")
                            .icon(Icon::empty().size_4())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                let n = s.query["projectThreadLimits"][&id].as_u64().unwrap_or(5);
                                s.query["projectThreadLimits"][&id] = json!(n + 10);
                                s.refresh_threads();
                                cx.notify();
                            })),
                    );
                }
            }
        }
        if self.more["hasMoreProjects"] == true {
            projects = projects.child(SidebarMenuItem::new("もっとプロジェクトを表示").on_click(
                cx.listener(|s, _, _, cx| {
                    s.query["projectLimit"] =
                        json!(s.query["projectLimit"].as_u64().unwrap_or(10) + 10);
                    s.refresh_threads();
                    cx.notify();
                }),
            ));
        }
        let mut chats = SidebarMenu::new().gap_1();
        for thread in self.threads.iter().filter(|t| t["projectId"].is_null()) {
            chats = chats.child(self.thread_button(thread, cx));
        }
        if self.more["hasMoreChats"] == true {
            chats = chats.child(SidebarMenuItem::new("もっと表示する").on_click(cx.listener(
                |s, _, _, cx| {
                    s.query["chatLimit"] = json!(s.query["chatLimit"].as_u64().unwrap_or(5) + 10);
                    s.refresh_threads();
                    cx.notify();
                },
            )));
        }
        let navigation = navigation
            .render("primary-navigation", window, cx)
            .into_any_element();
        Sidebar::new("desktop-sidebar")
            .w(px(272.))
            .bg(rgb(0x242424))
            .header(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        h_flex()
                            .h_8()
                            .pl(px(72.))
                            .justify_end()
                            .child(self.icon_button(
                                "collapse-sidebar",
                                IconName::PanelLeftClose,
                                "サイドバーを閉じる",
                                cx,
                                |s, _, _| s.sidebar = false,
                            )),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .child(div().flex_1().text_lg().font_semibold().child("Bex"))
                            .child(
                                self.icon_button(
                                    "choose-project",
                                    IconName::FolderOpen,
                                    "プロジェクトを追加",
                                    cx,
                                    |s, _, _| s.pick_folder(),
                                )
                                .disabled(!self.remote.is_empty()),
                            ),
                    )
                    .child(navigation)
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(IconName::Search)
                            .appearance(false)
                            .aria_label("会話を検索"),
                    ),
            )
            .child(SidebarGroup::new("プロジェクト").child(projects))
            .child(SidebarGroup::new("チャット").child(chats))
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(0x383838))
                    .py_2()
                    .child(div().size_2().rounded_full().bg(rgb(if self.connected {
                        0x37cf77
                    } else {
                        0x999999
                    })))
                    .child(div().flex_1().min_w_0().child(self.host_menu(cx)))
                    .child(self.icon_button(
                        "refresh-threads",
                        IconName::RotateCw,
                        "会話を更新",
                        cx,
                        |s, _, _| s.refresh_threads(),
                    )),
            )
            .into_any_element()
    }
    fn thread_button(&self, thread: &Value, cx: &Context<Self>) -> SidebarMenuItem {
        let id = text(thread, "id").to_owned();
        SidebarMenuItem::new(
            thread["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("新しいチャット")
                .to_owned(),
        )
        .active(id == self.selected && self.tab != Tab::Settings)
        .disable(self.busy > 0)
        .on_click(cx.listener(move |s, _, w, cx| {
            s.open_thread(id.clone(), w, cx);
            cx.notify();
        }))
    }
    fn chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let history = list(self.list.clone(), move |ix, _w, cx| {
            entity
                .update(cx, |s, cx| {
                    let turns = array(&s.conversation.thread["turns"]);
                    if ix < turns.len() {
                        let turn = s.conversation.thread["turns"][ix].take();
                        let row = s.turn(&turn, cx);
                        s.conversation.thread["turns"][ix] = turn;
                        row
                    } else {
                        let request = s.visible_requests().nth(ix - turns.len()).cloned();
                        request
                            .map(|r| s.request_card(&r, cx))
                            .unwrap_or_else(|| div().into_any_element())
                    }
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .flex_1()
        .min_h_0();
        let mut body = v_flex().flex_1().min_w_0().h_full();
        if self.conversation.thread.is_null() {
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(div().text_2xl().child("何から始めましょうか？"))
                    .child(if self.cwd.is_empty() {
                        "プロジェクトを選んで、作業を始めましょう。".into()
                    } else {
                        basename(&self.cwd)
                    }),
            );
        } else {
            body = body.child(history);
        }
        let key = self.draft_key();
        let attachments = array(&self.cache["attachments"][&key]);
        let mut files = h_flex().gap_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            files = files.child(self.button(
                format!("attachment-{i}"),
                format!("{} ×", text(file, "name")),
                cx,
                move |s, _, _| {
                    if let Some(files) = s.cache["attachments"][&key].as_array_mut() {
                        if i < files.len() {
                            files.remove(i);
                        }
                    }
                    s.refresh_sources();
                    s.persist();
                },
            ));
        }
        let running = self.conversation.active();
        let empty = self.composer.read(cx).value().trim().is_empty() && attachments.is_empty();
        let send = if let Some(turn) = running.filter(|_| empty) {
            let id = turn["id"].clone();
            self.icon_button(
                "stop",
                IconName::Pause,
                "生成を停止",
                cx,
                move |s, _, _| {
                    s.request(
                        false,
                        "turn/interrupt",
                        json!({"threadId":s.selected,"turnId":id}),
                        true,
                        |_, _, _, _| {},
                    )
                },
            )
        } else {
            self.icon_button("send", IconName::ArrowUp, "送信", cx, |s, _, cx| {
                s.send(cx)
            })
            .disabled(!self.connected || self.busy > 0 || empty || self.cwd.is_empty())
        };
        let composer = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .p_3()
            .gap_2()
            .rounded(px(24.))
            .bg(rgb(0x2b2b2b))
            .border_1()
            .border_color(rgb(0x363636))
            .child(files)
            .child(
                Textarea::new(&self.composer)
                    .appearance(false)
                    .bordered(false)
                    .aria_label("Codex に依頼する"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        self.icon_button(
                            "attach",
                            IconName::Plus,
                            "ファイルを添付",
                            cx,
                            |s, _, _| s.attach(),
                        )
                        .disabled(!self.connected || self.cwd.is_empty() || self.busy > 0),
                    )
                    .child(
                        self.button(
                            "composer-folder",
                            if self.cwd.is_empty() {
                                "フォルダを選択".into()
                            } else {
                                basename(&self.cwd)
                            },
                            cx,
                            |s, _, _| s.pick_folder(),
                        )
                        .disabled(!self.remote.is_empty()),
                    )
                    .child(div().flex_1())
                    .child(self.model_menu(cx))
                    .child(send.rounded(px(18.)).w_8().h_8().primary()),
            );
        body.child(
            h_flex()
                .justify_center()
                .px_6()
                .pt_2()
                .pb_4()
                .child(composer),
        )
        .into_any_element()
    }
    fn files(&self, cx: &Context<Self>) -> AnyElement {
        let mut entries = v_flex().gap_1();
        for (i, entry) in self.entries.iter().enumerate() {
            let path = text(entry, "path").to_owned();
            let download = path.clone();
            let directory = entry["directory"] == true;
            entries = entries.child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        self.button(
                            format!("file-{i}"),
                            text(entry, "name").to_owned(),
                            cx,
                            move |s, _, _| {
                                if directory {
                                    s.browse(path.clone());
                                } else {
                                    s.edit(path.clone(), false);
                                }
                            },
                        )
                        .icon(if directory {
                            IconName::Folder
                        } else {
                            IconName::FileText
                        })
                        .flex_1(),
                    )
                    .when(!directory, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("download-{i}")))
                                .icon(IconName::ArrowDown)
                                .small()
                                .ghost()
                                .tooltip("ダウンロード")
                                .accessibility_label("ダウンロード")
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.download(download.clone());
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        let mut editor = v_flex().flex_1().min_h_0().min_w_0().gap_2();
        if !self.editor.is_null() {
            editor = editor
                .child(
                    h_flex().gap_2().child(IconName::FileText).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(text(&self.editor, "path").to_owned()),
                    ),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        Editor::new(&self.editor_input)
                            .h_full()
                            .aria_label("ファイル編集"),
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            self.button("save-file", "保存", cx, |s, _, cx| s.save_file(cx))
                                .primary()
                                .disabled(self.busy > 0),
                        )
                        .child(self.icon_button(
                            "reload-file",
                            IconName::RotateCw,
                            "再読み込み・下書きを破棄",
                            cx,
                            |s, _, _| s.edit(text(&s.editor, "path").into(), true),
                        ))
                        .child(
                            self.button("ai-edit", "AI に編集を依頼", cx, |s, w, cx| {
                                let key = s.draft_key();
                                s.cache["messages"][key] = json!(format!(
                                    "ファイル {} を編集してください。\n\n",
                                    text(&s.editor, "path")
                                ));
                                s.persist();
                                s.restore_draft(w, cx);
                                s.tab = Tab::Chat;
                            })
                            .icon(IconName::Bot),
                        ),
                );
        } else {
            editor = editor
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    Icon::new(IconName::FileText)
                        .size_8()
                        .text_color(rgb(0x777777)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x999999))
                        .child("ファイルを選択して編集"),
                );
        }
        v_flex()
            .size_full()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(self.icon_button(
                        "parent-directory",
                        IconName::ArrowUp,
                        "親フォルダ",
                        cx,
                        |s, _, cx| {
                            let path = s.path.read(cx).value();
                            s.browse(
                                Path::new(path.as_ref())
                                    .parent()
                                    .unwrap_or(Path::new("/"))
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        },
                    ))
                    .child(Input::new(&self.path).small().aria_label("フォルダのパス"))
                    .child(self.icon_button(
                        "browse",
                        IconName::ArrowRight,
                        "フォルダを開く",
                        cx,
                        |s, _, cx| s.browse(s.path.read(cx).value().to_string()),
                    )),
            )
            .child(
                div()
                    .id("file-list")
                    .h(px(160.))
                    .flex_shrink_0()
                    .overflow_y_scroll()
                    .child(entries),
            )
            .child(div().h(px(1.)).bg(rgb(0x303030)))
            .child(editor)
            .into_any_element()
    }
    fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .gap_4()
            .p_7()
            .child(div().text_2xl().child("設定"))
            .child(self.host_menu(cx));
        body = body.child(
            v_flex().gap_3()
                .child(div().text_xl().child("ワークツリー"))
                .child("選択中の Host に保存し、Mac・iPhone からの新規セッションに適用します。")
                .child(switch::Switch::new("worktree-create")
                    .label("新規セッションをワークツリーで開始")
                    .checked(self.worktree_settings["createOnNewSession"] == true)
                    .disabled(!self.connected || self.worktree_settings.is_null() || self.busy > 0)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.worktree_settings["createOnNewSession"] = json!(*checked);
                        s.worktree_saved = false;
                        cx.notify();
                    })))
                .child(switch::Switch::new("worktree-copy")
                    .label("ワークツリー作成時にファイルをコピー")
                    .checked(self.worktree_settings["copyOnCreate"] == true)
                    .disabled(!self.connected || self.worktree_settings.is_null() || self.busy > 0)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.worktree_settings["copyOnCreate"] = json!(*checked);
                        s.worktree_saved = false;
                        cx.notify();
                    })))
                .child("コピー対象（リポジトリからの相対パスを1行に1つ）")
                .child(Textarea::new(&self.worktree_copy_paths).aria_label("コピー対象")
                    .readonly(!self.connected || self.worktree_settings.is_null() || self.busy > 0))
                .child("例: .env、.env.local、config/local。存在しないパスはスキップします。指定したファイルはコピー元の内容で置き換えます。シンボリックリンクはコピーできません。")
                .child("最初のメッセージ送信時に現在の HEAD から作成します。既存セッションを開き直しても作成・コピーしません。")
                .child(h_flex().gap_3()
                    .child(self.button("worktree-save", "ワークツリー設定を保存", cx, |s, _, cx| s.save_worktree_settings(cx))
                        .disabled(!self.connected || self.worktree_settings.is_null() || self.busy > 0))
                    .when(self.worktree_saved, |row| row.child("保存しました")))
        );
        if !self.manager_connected {
            body=body.child("この Mac の Host を起動").child(Input::new(&self.relay_url)).child(Input::new(&self.relay_token)).child(Input::new(&self.runner)).child(self.button("start-host","接続して起動",cx,|s,_,cx|{let endpoint=json!({"relayUrl":s.relay_url.read(cx).value().as_ref(),"relayToken":s.relay_token.read(cx).value().as_ref(),"runnerId":s.runner.read(cx).value().as_ref()});s.work(true,move||platform::start_host(Some(endpoint)).map(|_|Value::Null),|_,_,_,_|{});}));
        } else {
            body = body
                .child(format!(
                    "{} · relay {}",
                    text(&self.status, "hostName"),
                    if self.status["relayConnected"] == true {
                        "接続中"
                    } else {
                        "再接続中"
                    }
                ))
                .child(self.button("invite", "iPhone・Mac を招待", cx, |s, _, _| s.invite()));
            if let Some(qr) = &self.invitation_qr {
                body = body
                    .child(img(qr.clone()).w(px(320.)).h(px(320.)))
                    .child("1回限りの招待です。相手端末で読み取るか、招待を貼り付けてください。")
                    .child(Textarea::new(&self.invitation_input).readonly(true))
                    .child(self.button("copy-invite", "招待をコピー", cx, |s, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            s.invitation_input.read(cx).value().to_string(),
                        ))
                    }));
            }
            for (i, device) in array(&self.status["devices"]).iter().enumerate() {
                let identity = device["identity"].clone();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(text(device, "name").to_owned())
                        .child(self.button(
                            format!("revoke-{i}"),
                            "接続を解除",
                            cx,
                            move |s, _, _| {
                                s.request(
                                    true,
                                    "host/revoke",
                                    json!({"identity":identity}),
                                    true,
                                    |s, _, _, _| s.refresh_manager(),
                                )
                            },
                        )),
                );
            }
            body = body
                .child("別の Mac に接続")
                .child(Textarea::new(&self.pairing).aria_label("ペアリング招待"))
                .child(self.button("pair", "ペアリング", cx, |s, _, cx| {
                    match serde_json::from_str::<Value>(s.pairing.read(cx).value().as_ref()) {
                        Ok(invitation) => s.request(
                            true,
                            "host/pairRemote",
                            json!({"invitation":invitation,"deviceName":"Mac"}),
                            true,
                            |s, v, w, cx| {
                                s.pairing.update(cx, |i, cx| i.set_value("", w, cx));
                                s.refresh_manager();
                                s.switch_host(text(&v, "id").into(), w, cx);
                            },
                        ),
                        Err(e) => s.error = e.to_string(),
                    }
                }));
            for host in &self.hosts {
                let id = text(host, "id").to_owned();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(text(host, "hostName").to_owned())
                        .child(self.button(
                            format!("remove-{id}"),
                            "保存した接続を削除",
                            cx,
                            move |s, _, _| {
                                let id = id.clone();
                                s.request(
                                    true,
                                    "host/removeRemote",
                                    json!({"id":id}),
                                    true,
                                    move |s, _, w, cx| {
                                        if s.remote == id {
                                            s.switch_host(String::new(), w, cx);
                                        }
                                        s.refresh_manager();
                                    },
                                );
                            },
                        )),
                );
            }
        }
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }
    fn workspace_card(&self, cx: &Context<Self>) -> AnyElement {
        let enabled = self.connected && !self.cwd.is_empty();
        let mut sources = v_flex().gap_1();
        for (i, path) in self.source_paths.iter().enumerate() {
            let path = path.clone();
            sources = sources.child(
                self.button(
                    format!("source-{i}"),
                    basename(&path),
                    cx,
                    move |s, _, _| s.download(path.clone()),
                )
                .icon(IconName::FileText)
                .w_full(),
            );
        }
        if self.source_paths.is_empty() {
            sources = sources.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x909090))
                    .py_2()
                    .child("添付ファイルがここに表示されます"),
            );
        }
        v_flex()
            .id("workspace-card")
            .w_full()
            .h_full()
            .p_4()
            .gap_4()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded(px(18.))
                    .bg(rgb(0x282828))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("ワークスペース"),
                            )
                            .child(
                                self.icon_button(
                                    "refresh-context",
                                    IconName::RotateCw,
                                    "変更を更新",
                                    cx,
                                    |s, _, _| s.refresh_review(),
                                )
                                .disabled(!enabled),
                            ),
                    )
                    .child(
                        self.button(
                            "context-files",
                            if self.cwd.is_empty() {
                                "フォルダを選択".into()
                            } else {
                                basename(&self.cwd)
                            },
                            cx,
                            |s, _, _| {
                                if s.cwd.is_empty() {
                                    s.pick_folder();
                                } else {
                                    s.browse(s.cwd.clone());
                                }
                            },
                        )
                        .icon(IconName::FolderOpen)
                        .w_full(),
                    )
                    .when(!text(&self.review, "branch").is_empty(), |body| {
                        body.child(
                            div()
                                .pl_2()
                                .text_sm()
                                .text_color(rgb(0x999999))
                                .child(text(&self.review, "branch").to_owned()),
                        )
                    })
                    .child(
                        self.button(
                            "context-changes",
                            format!(
                                "変更  +{}  −{}",
                                self.review["additions"].as_u64().unwrap_or(0),
                                self.review["deletions"].as_u64().unwrap_or(0)
                            ),
                            cx,
                            |s, _, _| {
                                s.panel = Panel::Diff;
                                s.panel_open = true;
                                s.tab = Tab::Chat;
                                s.refresh_review();
                            },
                        )
                        .icon(IconName::Replace)
                        .w_full()
                        .disabled(!enabled),
                    )
                    .child(div().h(px(1.)).my_2().bg(rgb(0x3a3a3a)))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("ソース"),
                            )
                            .child(
                                self.icon_button(
                                    "context-attach",
                                    IconName::Plus,
                                    "ソースを追加",
                                    cx,
                                    |s, _, _| s.attach(),
                                )
                                .disabled(!enabled),
                            ),
                    )
                    .child(sources)
                    .child(
                        self.button("all-files", "すべてのファイル", cx, |s, _, _| {
                            s.browse(s.cwd.clone())
                        })
                        .icon(IconName::Folder)
                        .w_full()
                        .disabled(!enabled),
                    ),
            )
            .when(!self.review_error.is_empty(), |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xff8e86))
                        .child(self.review_error.clone()),
                )
            })
            .into_any_element()
    }
}
impl Desktop {
    fn workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const TOOLS: [(Panel, IconName, &str); 4] = [
            (Panel::Terminal, IconName::SquareTerminal, "ターミナル"),
            (Panel::SideChat, IconName::Bot, "サイドチャット"),
            (Panel::Browser, IconName::Globe, "ブラウザ"),
            (Panel::Files, IconName::Folder, "ファイラ"),
        ];
        let selected = TOOLS.iter().position(|(panel, _, _)| *panel == self.panel);
        let tabs = TabBar::new("workbench-tabs")
            .small()
            .underline()
            .when_some(selected, |bar, ix| bar.selected_index(ix))
            .children(
                TOOLS
                    .iter()
                    .map(|(_, icon, label)| UiTab::new().icon(icon.clone()).aria_label(*label)),
            )
            .on_click(cx.listener(|s, ix: &usize, w, cx| {
                let panel = TOOLS[*ix].0;
                if panel == Panel::Files {
                    s.browse(s.cwd.clone());
                } else {
                    s.open_panel(panel, w, cx);
                }
            }));
        let toolbar = h_flex()
            .h_10()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(rgb(0x303030))
            .child(self.icon_button(
                "panel-home",
                IconName::LayoutDashboard,
                "パネルのホーム",
                cx,
                |s, _, _| s.panel = Panel::Home,
            ))
            .child(tabs)
            .child(div().flex_1())
            .when(self.panel == Panel::Terminal, |bar| {
                bar.child(self.icon_button(
                    "new-terminal",
                    IconName::Plus,
                    "新しいターミナル",
                    cx,
                    |s, w, cx| {
                        s.terminal = None;
                        s.open_panel(Panel::Terminal, w, cx);
                    },
                ))
            })
            .when(self.panel == Panel::SideChat, |bar| {
                bar.child(self.icon_button(
                    "new-side-chat",
                    IconName::Plus,
                    "新しいサイドチャット",
                    cx,
                    |s, w, cx| {
                        if let Some(chat) = &s.side_chat {
                            chat.update(cx, |chat, cx| chat.new_thread(s.cwd.clone(), w, cx));
                        }
                    },
                ))
            })
            .child(self.icon_button(
                "close-panel",
                IconName::Close,
                "右パネルを閉じる",
                cx,
                |s, _, _| s.panel_open = false,
            ));
        let body = match self.panel {
            Panel::Home => {
                let mut chooser = v_flex().w_full().max_w(px(400.)).gap_2();
                for (ix, (panel, icon, label)) in TOOLS.iter().enumerate() {
                    let panel = *panel;
                    chooser = chooser.child(
                        self.button(format!("tool-{ix}"), *label, cx, move |s, w, cx| {
                            if panel == Panel::Files {
                                s.browse(s.cwd.clone());
                            } else {
                                s.open_panel(panel, w, cx);
                            }
                        })
                        .icon(icon.clone())
                        .w_full()
                        .h_10()
                        .bg(rgb(0x232323))
                        .disabled(
                            panel != Panel::Browser && (!self.connected || self.cwd.is_empty()),
                        ),
                    );
                }
                v_flex()
                    .size_full()
                    .p_8()
                    .items_center()
                    .justify_center()
                    .child(chooser)
                    .into_any_element()
            }
            Panel::Files => self.files(cx),
            Panel::Diff => v_flex()
                .size_full()
                .min_h_0()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .child(div().flex_1().child(format!(
                            "変更  +{} −{}",
                            self.review["additions"].as_u64().unwrap_or(0),
                            self.review["deletions"].as_u64().unwrap_or(0)
                        )))
                        .child(self.icon_button(
                            "refresh-diff",
                            IconName::RotateCw,
                            "変更を更新",
                            cx,
                            |s, _, _| s.refresh_review(),
                        )),
                )
                .when(!self.review_error.is_empty(), |v| {
                    v.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xff8e86))
                            .child(self.review_error.clone()),
                    )
                })
                .child(div().flex_1().min_h_0().child(self.diff(
                    "workspace-patch".into(),
                    &text(&self.review, "diff").to_owned(),
                    cx,
                )))
                .into_any_element(),
            Panel::SideChat => self
                .side_chat
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
            Panel::Terminal => self
                .terminal
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
            Panel::Browser => self
                .browser
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(toolbar)
            .child(div().flex_1().min_h_0().flex().child(body))
            .into_any_element()
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.panel_open && self.tab == Tab::Chat;
        if let Some(view) = self.browser.clone() {
            view.update(cx, |v, cx| {
                v.set_visible(active && self.panel == Panel::Browser, cx)
            });
        }
        if let Some(view) = self.terminal.clone() {
            view.update(cx, |v, cx| {
                v.set_visible(active && self.panel == Panel::Terminal, cx)
            });
        }
        let wide = window.viewport_size().width >= px(1080.);
        let title = if self.tab == Tab::Settings {
            "設定"
        } else {
            self.conversation.thread["name"]
                .as_str()
                .or_else(|| {
                    self.threads
                        .iter()
                        .find(|t| t["id"] == self.selected)
                        .and_then(|t| t["name"].as_str())
                })
                .unwrap_or("新しいチャット")
        }
        .to_owned();
        let mut header = h_flex().h(px(48.)).flex_shrink_0().px_4().gap_2();
        if !self.sidebar {
            header = header.pl(px(88.)).child(self.icon_button(
                "expand-sidebar",
                IconName::PanelLeftOpen,
                "サイドバーを開く",
                cx,
                |s, _, _| s.sidebar = true,
            ));
        }
        header = header.child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_ellipsis()
                .child(title),
        );
        if self.tab == Tab::Settings {
            header = header.child(self.icon_button(
                "back-chat",
                IconName::ArrowLeft,
                "チャットに戻る",
                cx,
                |s, _, _| s.tab = Tab::Chat,
            ));
        }
        header = header
            .child(self.icon_button(
                "header-new-thread",
                IconName::Plus,
                "新しいチャット",
                cx,
                |s, w, cx| s.new_thread(s.cwd.clone(), w, cx),
            ))
            .child(
                self.icon_button(
                    "panel-toggle",
                    if self.panel_open {
                        IconName::PanelRightClose
                    } else {
                        IconName::PanelRightOpen
                    },
                    "右パネルを切り替え",
                    cx,
                    |s, _, _| s.panel_open = !s.panel_open,
                )
                .selected(self.panel_open),
            );
        let content = if self.side_chat_mode {
            self.chat(cx)
        } else if self.tab == Tab::Settings {
            self.settings(cx)
        } else if self.panel_open {
            let right = self.workbench(cx);
            if wide {
                h_resizable("chat-workbench-split")
                    .child(
                        resizable_panel()
                            .size_range(px(360.)..px(2400.))
                            .child(self.chat(cx)),
                    )
                    .child(
                        resizable_panel()
                            .size(px(540.))
                            .size_range(px(320.)..px(1100.))
                            .child(right),
                    )
                    .into_any_element()
            } else {
                right
            }
        } else {
            h_flex()
                .size_full()
                .items_stretch()
                .child(self.chat(cx))
                .when(window.viewport_size().width >= px(1280.), |v| {
                    v.child(
                        div()
                            .w(px(300.))
                            .flex_shrink_0()
                            .child(self.workspace_card(cx)),
                    )
                })
                .into_any_element()
        };
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!self.side_chat_mode, |v| v.child(header))
            .when(!self.error.is_empty(), |body| {
                body.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .bg(rgb(0x352523))
                        .child(Icon::new(IconName::TriangleAlert).text_color(rgb(0xff8e86)))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(rgb(0xff8e86))
                                .child(self.error.clone()),
                        )
                        .child(self.icon_button(
                            "dismiss-error",
                            IconName::Close,
                            "エラーを閉じる",
                            cx,
                            |s, _, _| s.error.clear(),
                        )),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_stretch()
                    .child(content),
            );
        h_flex()
            .size_full()
            .items_stretch()
            .bg(rgb(0x181818))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .font_family("Hiragino Sans")
            .font_weight(FontWeight::NORMAL)
            .when(self.sidebar && !self.side_chat_mode, |body| {
                body.child(self.sidebar(window, cx))
            })
            .child(main)
    }
}
