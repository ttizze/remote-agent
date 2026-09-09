use super::*;
use std::path::PathBuf;

const CHAT_WIDTH: f32 = 780.;

fn review_counts(additions: Option<u64>, deletions: Option<u64>) -> AnyElement {
    let counts = h_flex().gap_1().text_xs();
    match (additions, deletions) {
        (Some(added), Some(deleted)) => counts
            .child(div().text_color(rgb(0x37cf77)).child(format!("+{added}")))
            .child(div().text_color(rgb(0xff6259)).child(format!("−{deleted}")))
            .into_any_element(),
        _ => counts
            .text_color(rgb(0x999999))
            .child("バイナリ")
            .into_any_element(),
    }
}

fn model_effort_slider(
    state: &Entity<slider::SliderState>,
    effort_count: usize,
    cx: &App,
) -> impl IntoElement {
    let disabled = effort_count < 2;
    let steps = effort_count.saturating_sub(1).max(1) as f32;
    let position = state.read(cx).percentage().end;
    let track = base::SliderIndicator::new(state)
        .relative()
        .w_full()
        .h(px(24.))
        .child(
            div()
                .absolute()
                .left(px(-14.))
                .right(relative(1. - position))
                .h_full()
                .rounded_full()
                .bg(rgb(0x3982f7)),
        )
        .children((0..effort_count).map(|i| {
            div()
                .absolute()
                .left(relative(i as f32 / steps))
                .ml(px(-2.))
                .top(px(10.))
                .size(px(4.))
                .rounded_full()
                .bg(rgb(0x9c9c9c))
        }))
        .child(
            base::SliderThumb::new(state)
                .disabled(disabled)
                .absolute()
                .left(relative(position))
                .ml(px(-14.))
                .top(px(-2.))
                .size(px(28.))
                .rounded_full()
                .bg(rgb(0xffffff)),
        );
    base::Slider::new(state)
        .disabled(disabled)
        .w_full()
        .py_1()
        .child(
            base::SliderTrack::new(state)
                .disabled(disabled)
                .w_full()
                .h(px(24.))
                .px(px(14.))
                .rounded_full()
                .bg(rgb(0x454545))
                .child(track),
        )
}

fn conversation_file_path(source: &str, cwd: &str) -> Result<PathBuf, String> {
    let source = source
        .rsplit_once(':')
        .filter(|(_, line)| !line.is_empty() && line.bytes().all(|c| c.is_ascii_digit()))
        .map_or(source, |(path, _)| path);
    let base = url::Url::from_directory_path(cwd).map_err(|_| "作業フォルダが不正です")?;
    let mut url = base.join(source).map_err(|e| e.to_string())?;
    if url.scheme() != "file" {
        return Err("未対応のリンクです".into());
    }
    url.set_fragment(None);
    url.set_query(None);
    url.to_file_path()
        .map_err(|_| "ファイルパスが不正です".into())
}

#[cfg(test)]
mod model_slider_tests {
    use super::model_effort_slider;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Modifiers, MouseButton, ParentElement, Render,
        Styled, TestAppContext, Window, component::slider, div, point, px,
    };

    struct SliderView {
        state: Entity<slider::SliderState>,
        effort_count: usize,
    }

    impl Render for SliderView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(280.))
                .p_4()
                .child(model_effort_slider(&self.state, self.effort_count, cx))
        }
    }

    #[gpui::test]
    fn effort_thumb_drags_both_ways_and_single_option_is_inert(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        for effort_count in [4, 1] {
            let state = cx.new(|_| {
                slider::SliderState::new()
                    .max(3.)
                    .step(1.)
                    .default_value(1.)
            });
            let owner = state.clone();
            let (_, cx) = cx.add_window_view(move |_, _| SliderView {
                state: owner,
                effort_count,
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.update(|_, cx| state.read(cx).bounds());
            let at = |fraction| {
                point(
                    bounds.left() + bounds.size.width * fraction,
                    bounds.center().y,
                )
            };
            let mut from = 1. / 3.;
            for to in [1., 0.] {
                cx.simulate_mouse_move(at(from), None, Modifiers::default());
                cx.simulate_mouse_down(at(from), MouseButton::Left, Modifiers::default());
                for step in 1..=8 {
                    cx.simulate_mouse_move(
                        at(from + (to - from) * step as f32 / 8.),
                        MouseButton::Left,
                        Modifiers::default(),
                    );
                }
                cx.simulate_mouse_up(at(to), MouseButton::Left, Modifiers::default());
                cx.update(|_, cx| {
                    assert_eq!(
                        state.read(cx).value(),
                        slider::SliderValue::Single(if effort_count > 1 { to * 3. } else { 1. })
                    )
                });
                from = to;
            }
        }
    }
}

#[cfg(test)]
mod link_tests {
    use super::conversation_file_path;

    #[test]
    fn file_links_resolve_on_the_selected_host_with_spaces_and_line_numbers() {
        for source in [
            "/tmp/project/a%20b.png",
            "file:///tmp/project/a%20b.png",
            "a%20b.png",
            "a%20b.png:12",
            "a%20b.png#L12",
        ] {
            assert_eq!(
                conversation_file_path(source, "/tmp/project").unwrap(),
                std::path::Path::new("/tmp/project/a b.png")
            );
        }
        assert!(conversation_file_path("https://example.com", "/tmp/project").is_err());
        assert!(conversation_file_path("javascript:alert(1)", "/tmp/project").is_err());
        assert!(conversation_file_path("file://other-host/private.png", "/tmp/project").is_err());
    }
}

impl ConversationView {
    fn image(
        &mut self,
        source: &str,
        encoded: bool,
        height: f32,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!(
            "{}:{}:{encoded}:{}",
            self.session.host.remote, self.session.cwd, source
        );
        if !self.images.contains_key(&key) {
            let source = std::sync::Arc::new(source.to_owned());
            self.images.insert(
                key.clone(),
                ImageState {
                    source: source.clone(),
                    path: None,
                    error: None,
                },
            );
            let cwd = self.session.cwd.clone();
            let remote = self.session.host.remote.clone();
            let manager = self.manager.clone();
            let destination = self
                .image_dir
                .path()
                .join(format!("image-{}", self.images.len()));
            let returned = key.clone();
            self.work(false,move||{
                let result=(||->Result<String,String>{
                    if encoded { let bytes=base64::engine::general_purpose::STANDARD.decode(if source.starts_with("data:") { source.split_once(",").map(|(_, data)| data).unwrap_or(&source).as_bytes() } else { source.as_bytes() }).map_err(|e|e.to_string())?; std::fs::write(&destination,bytes).map_err(|e|e.to_string())?;return Ok(destination.to_string_lossy().into_owned()); }
                    if source.starts_with("https://")||source.starts_with("http://"){return Ok(source.as_ref().clone());}
                    if source.starts_with("data:image/"){let (header,data)=source.split_once(',').ok_or("画像データが不正です")?;if !header.ends_with(";base64"){return Err("未対応の画像データです".into());}let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|e|e.to_string())?;std::fs::write(&destination,bytes).map_err(|e|e.to_string())?;return Ok(destination.to_string_lossy().into_owned());}
                    let path=conversation_file_path(&source,&cwd)?;
                    if remote.is_empty(){return Ok(path.to_string_lossy().into_owned());}
                    manager.request("host/transfer",json!({"profileId":remote,"direction":"download","source":path,"destination":destination}))?;Ok(destination.to_string_lossy().into_owned())
                })();Ok(match result{Ok(path)=>json!({"path":path}),Err(error)=>json!({"error":error})})
            },move|s,v,_,_|{if let Some(image)=s.images.get_mut(&returned){image.path=v["path"].as_str().map(|s|if s.starts_with("/"){std::path::PathBuf::from(s).into()}else{ImageSource::from(s.to_owned())});image.error=v["error"].as_str().map(str::to_owned);}s.list.remeasure();});
        }
        let state = &self.images[&key];
        if let Some(path) = &state.path {
            let image = img(path.clone())
                .w_full()
                .h(px(height))
                .min_h(px(height))
                .max_h(px(height))
                .object_fit(ObjectFit::Contain);
            if clickable {
                let source = state.source.clone();
                div()
                    .id(SharedString::from(key))
                    .w_full()
                    .h(px(height))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.open_image_gallery(source.clone(), encoded, cx)
                    }))
                    .child(image)
                    .into_any_element()
            } else {
                image.into_any_element()
            }
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
            let mut rendered = String::new();
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
            if !images.is_empty() {
                rendered.push_str(&source[previous..]);
            }
            let source: SharedString = source.to_owned().into();
            let rendered = if images.is_empty() {
                source.clone()
            } else {
                rendered.into()
            };
            self.markdown_cache.insert(
                id.clone(),
                MarkdownContent {
                    source,
                    rendered,
                    images: images.into(),
                },
            );
        }
        let cached = &self.markdown_cache[&id];
        let images = cached.images.clone();
        let view = cx.entity().downgrade();
        let mut body = v_flex().gap_3().w_full().child(
            TextView::markdown(SharedString::from(id), cached.rendered.clone())
                .selectable(true)
                .on_link_click(move |url, _, _, cx| {
                    let _ = view.update(cx, |s, cx| s.open_conversation_link(url, cx));
                }),
        );
        for source in images.iter() {
            body = body.child(self.image(source, false, 320., true, cx));
        }
        body.into_any_element()
    }

    fn open_conversation_link(&mut self, source: &str, cx: &mut Context<Self>) {
        if source.starts_with("https://") || source.starts_with("http://") {
            cx.open_url(source);
            return;
        }
        let path = match conversation_file_path(source, &self.session.cwd) {
            Ok(path) => path,
            Err(error) => {
                self.error = error;
                cx.notify();
                return;
            }
        };
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(image::ImageFormat::from_extension)
            .is_some()
        {
            self.open_image_gallery(
                std::sync::Arc::new(path.to_string_lossy().into_owned()),
                false,
                cx,
            );
            return;
        }
        let remote = self.session.host.remote.clone();
        let manager = self.manager.clone();
        let directory = self.image_dir.path().to_owned();
        self.work(false, move || {
            let path = if remote.is_empty() {
                path
            } else {
                let download = tempfile::Builder::new().prefix("link-").tempdir_in(directory).map_err(|e| e.to_string())?.keep();
                let destination = download.join(path.file_name().ok_or("ファイル名がありません")?);
                manager.request("host/transfer", json!({"profileId":remote,"direction":"download","source":path,"destination":destination}))?;
                destination
            };
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            let url = url::Url::from_file_path(path).map_err(|_| "ファイルパスが不正です")?;
            Ok(json!({"url":url.as_str()}))
        }, |_, result, _, cx| cx.open_url(text(&result, "url")));
    }

    fn open_image_gallery(
        &mut self,
        source: std::sync::Arc<String>,
        encoded: bool,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4();
        self.image_gallery = Some(ImageGallery {
            id,
            entries: Vec::new(),
            initial: (source, encoded),
            selected: None,
            list: ListState::new(0, ListAlignment::Top, px(160.)),
            loading: true,
            saving: false,
            saved: false,
            error: String::new(),
        });
        let thread = self.session.selected.clone();
        let done = self.complete::<Vec<agent_client::operations::SessionImage>>(
            false,
            move |s, result, _, _| {
                let Some(gallery) = s.image_gallery.as_mut().filter(|gallery| gallery.id == id)
                else {
                    return;
                };
                gallery.loading = false;
                let images = match result {
                    Ok(images) => images,
                    Err(error) => {
                        gallery.error = error;
                        return;
                    }
                };
                let initial = gallery.current_image().clone();
                let entries: Vec<_> = images
                    .into_iter()
                    .map(|image| (std::sync::Arc::new(image.source), image.encoded))
                    .collect();
                gallery.selected = entries.iter().position(|entry| entry == &initial);
                gallery
                    .list
                    .splice(0..gallery.list.item_count(), entries.len());
                gallery.entries = entries;
                if let Some(selected) = gallery.selected {
                    gallery.list.scroll_to_reveal_item(selected);
                }
            },
        );
        self.session.host.rpc.agent_async(
            move |client| async move { client.session_images(&thread).await },
            done,
        );
        cx.notify();
    }

    fn gallery_thumbnail(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(gallery) = &self.image_gallery else {
            return div().into_any_element();
        };
        let Some((source, encoded)) = gallery.entries.get(index).cloned() else {
            return div().into_any_element();
        };
        let selected = gallery.selected == Some(index);
        let image = self.image(&source, encoded, 64., false, cx);
        Button::new(format!("gallery-thumbnail-{index}"))
            .accessibility_label(format!("生成画像 {}", index + 1))
            .ghost()
            .child(image)
            .w(px(80.))
            .h(px(80.))
            .p_2()
            .selected(selected)
            .on_click(cx.listener(move |s, _, _, cx| {
                if let Some(gallery) = s.image_gallery.as_mut().filter(|gallery| !gallery.saving) {
                    gallery.selected = Some(index);
                    gallery.saved = false;
                    gallery.error.clear();
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn image_gallery_view(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let gallery = self.image_gallery.as_ref().unwrap();
        let (source, encoded) = gallery.current_image().clone();
        let list_state = gallery.list.clone();
        let saving = gallery.saving;
        let saved = gallery.saved;
        let loading = gallery.loading;
        let label = gallery
            .selected
            .map(|index| format!("{} / {}", index + 1, gallery.entries.len()))
            .unwrap_or_else(|| "画像".into());
        let error = gallery.error.clone();
        let image = self.image(
            &source,
            encoded,
            (f32::from(window.viewport_size().height) - 148.).max(120.),
            false,
            cx,
        );
        let key = format!(
            "{}:{}:{encoded}:{}",
            self.session.host.remote, self.session.cwd, source
        );
        let ready = self
            .images
            .get(&key)
            .is_some_and(|image| image.path.is_some());
        let entity = cx.entity().downgrade();
        let thumbnails = list(list_state, move |index, _, cx| {
            entity
                .update(cx, |s, cx| s.gallery_thumbnail(index, cx))
                .unwrap_or_else(|_| div().into_any_element())
        })
        .size_full();
        v_flex()
            .size_full()
            .p_4()
            .pt(px(44.))
            .gap_3()
            .child(
                h_flex()
                    .justify_end()
                    .gap_3()
                    .child(div().flex_1().child(label))
                    .when(loading, |row| row.child("画像一覧を読み込み中…"))
                    .child(
                        button(
                            "gallery-save",
                            if saved { "保存済み" } else { "保存" },
                            cx,
                            |s, _, cx| s.save_gallery_image(cx),
                        )
                        .disabled(!ready || saving || saved),
                    )
                    .child(
                        button("gallery-close", "閉じる", cx, |s, _, _| {
                            s.image_gallery = None
                        })
                        .disabled(saving),
                    ),
            )
            .when(!error.is_empty(), |view| {
                view.child(div().text_color(rgb(0xff8e86)).child(error))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .child(div().w(px(88.)).h_full().child(thumbnails))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .justify_center()
                            .child(image),
                    ),
            )
            .into_any_element()
    }

    fn save_gallery_image(&mut self, cx: &mut Context<Self>) {
        let Some(gallery) = self
            .image_gallery
            .as_mut()
            .filter(|gallery| !gallery.saving)
        else {
            return;
        };
        let (source, encoded) = gallery.current_image();
        let key = format!(
            "{}:{}:{encoded}:{}",
            self.session.host.remote, self.session.cwd, source
        );
        let Some(ImageSource::Resource(resource)) =
            self.images.get(&key).and_then(|image| image.path.clone())
        else {
            return;
        };
        gallery.saving = true;
        gallery.error.clear();
        let id = gallery.id;
        let http = cx.http_client();
        cx.spawn(async move |view, cx| {
            enum Original {
                File(std::sync::Arc<Path>),
                Bytes(Vec<u8>),
            }
            let result = async {
                let original = match resource {
                    Resource::Path(path) => Original::File(path),
                    Resource::Uri(uri) => {
                        use futures_util::AsyncReadExt;
                        let mut response = http
                            .get(
                                uri.as_ref(),
                                gpui_kit::http_client::AsyncBody::empty(),
                                true,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                        if !response.status().is_success() {
                            return Err(format!("画像の取得に失敗しました: {}", response.status()));
                        }
                        let mut bytes = Vec::new();
                        response
                            .body_mut()
                            .read_to_end(&mut bytes)
                            .await
                            .map_err(|error| error.to_string())?;
                        Original::Bytes(bytes)
                    }
                    _ => return Err("保存できない画像です".into()),
                };
                cx.background_executor()
                    .spawn(async move {
                        let format = match &original {
                            Original::File(path) => image::ImageReader::open(path)
                                .and_then(|reader| reader.with_guessed_format())
                                .map_err(|error| error.to_string())?
                                .format()
                                .ok_or("画像形式が不明です")?,
                            Original::Bytes(bytes) => {
                                image::guess_format(bytes).map_err(|error| error.to_string())?
                            }
                        };
                        let mode = format!("download:image.{}", format.extensions_str()[0]);
                        let Some(destination) = platform::choose(&mode)? else {
                            return Ok(false);
                        };
                        match original {
                            Original::File(path) => {
                                let source = std::fs::canonicalize(&path)
                                    .map_err(|error| error.to_string())?;
                                if std::fs::canonicalize(&destination).ok().as_deref()
                                    != Some(source.as_path())
                                {
                                    std::fs::copy(source, destination)
                                        .map_err(|error| error.to_string())?;
                                }
                            }
                            Original::Bytes(bytes) => std::fs::write(destination, bytes)
                                .map_err(|error| error.to_string())?,
                        }
                        Ok::<_, String>(true)
                    })
                    .await
            }
            .await;
            let _ = view.update(cx, |s, cx| {
                if let Some(gallery) = s.image_gallery.as_mut().filter(|gallery| gallery.id == id) {
                    gallery.saving = false;
                    match result {
                        Ok(saved) => gallery.saved = saved,
                        Err(error) => gallery.error = error,
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn activity_text(id: String, content: &str, language: &str) -> TextView {
        // Reserve the output viewport before asynchronous Markdown parsing.
        // Long tool output scrolls inside it instead of moving the conversation.
        let lines = content.lines().take(18).count().max(2);
        TextView::markdown(SharedString::from(id), fenced(content, language))
            .selectable(true)
            .scrollable(true)
            .h(px(lines as f32 * 22. + 32.))
    }

    fn item(&mut self, item: &Value, turn: &Value, cx: &mut Context<Self>) -> AnyElement {
        let id = text(item, "id").to_owned();
        let kind = text(item, "type");
        let expanded = self.expanded_items.contains(&id);
        let deferred = array(&turn["deferredItemIds"])
            .iter()
            .any(|value| value == &id);
        if expanded && deferred {
            let key = (text(turn, "id").to_owned(), id.clone());
            let label = match self
                .session
                .item_details
                .get(&key)
                .map(|state| state.error.as_deref())
            {
                Some(None) => "詳細を読み込み中…".to_owned(),
                Some(Some(error)) => format!("{error} · 再試行"),
                None => "詳細を読み込む".to_owned(),
            };
            let title = conversation_presentation::item_presentation(item).title;
            let toggle = id.clone();
            return v_flex()
                .gap_2()
                .child(button(
                    format!("collapse-{id}"),
                    format!("⌄ {title}"),
                    cx,
                    move |s, _, _| {
                        s.expanded_items.remove(&toggle);
                        s.remeasure_item(&toggle);
                    },
                ))
                .child(button(format!("detail-{id}"), label, cx, move |s, _, _| {
                    s.load_detail(key.0.clone(), key.1.clone());
                    s.remeasure_item(&key.0);
                }))
                .into_any_element();
        }
        let turn_id = text(turn, "id").to_owned();
        match kind {
            "agentMessage" => self.markdown(id, text(item, "text"), cx),
            "imageGeneration" => {
                let path = text(item, "savedPath");
                let result = text(item, "result");
                let title = conversation_presentation::item_presentation(item).title;
                let mut body = v_flex()
                    .gap_2()
                    .w_full()
                    .child(div().text_sm().child(title));
                if !path.is_empty() {
                    body = body.child(self.image(path, false, 320., true, cx));
                    let path = path.to_owned();
                    body = body.child(button(
                        format!("open-image-{id}"),
                        "画像を開く",
                        cx,
                        move |s, _, cx| {
                            s.open_image_gallery(std::sync::Arc::new(path.clone()), false, cx)
                        },
                    ));
                } else if !result.is_empty() {
                    body = body.child(self.image(result, true, 320., true, cx));
                } else if text(item, "status") == "inProgress" {
                    body = body.child(spinner::Spinner::new().small());
                }
                body.into_any_element()
            }
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
                            "localImage" => self.image(text(part, "path"), false, 320., true, cx),
                            "image" => self.image(text(part, "url"), false, 320., true, cx),
                            _ => {
                                let path = text(part, "path").to_owned();
                                button(
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
                let label = conversation_presentation::item_presentation(item).title;
                let toggle = id.clone();
                let mut body = v_flex().gap_2().child(button(
                    format!("expand-{id}"),
                    format!("{} {label}", if expanded { "⌄" } else { "›" }),
                    cx,
                    move |s, _, _| {
                        toggle_set(&mut s.expanded_items, &toggle);
                        if s.expanded_items.contains(&toggle) {
                            s.load_detail(turn_id.clone(), toggle.clone());
                        }
                        s.pause_tail();
                        s.remeasure_item(&toggle);
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
                        format!(
                            "$ {}\n\n{}",
                            text(item, "command"),
                            text(item, "aggregatedOutput")
                        )
                    };
                    body = body
                        .child(Self::activity_text(format!("output-{id}"), &content, ""))
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
                    let turn_id = turn_id.clone();
                    let row = h_flex()
                        .gap_2()
                        .child(button(
                            format!("change-{id}-{i}"),
                            format!("{} {}", if expanded { "⌄" } else { "›" }, basename(&path)),
                            cx,
                            move |s, _, _| {
                                toggle_set(&mut s.expanded_items, &toggle);
                                if s.expanded_items.contains(&toggle) {
                                    s.load_detail(turn_id.clone(), toggle.clone());
                                }
                                s.pause_tail();
                                s.remeasure_item(&toggle);
                            },
                        ))
                        .child(button(
                            format!("download-{id}-{i}"),
                            "ダウンロード",
                            cx,
                            move |s, _, _| s.download(path.clone()),
                        ));
                    body = body.child(row);
                    if expanded {
                        body = body.child(diff(
                            &mut self.diffs,
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
                    .child(button(
                        format!("unknown-{id}"),
                        format!(
                            "{} {}",
                            if expanded { "⌄" } else { "›" },
                            conversation_presentation::item_presentation(item).title
                        ),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            if s.expanded_items.contains(&toggle) {
                                s.load_detail(turn_id.clone(), toggle.clone());
                            }
                            s.pause_tail();
                            s.remeasure_item(&toggle);
                        },
                    ))
                    .when(expanded, |body| {
                        body.child(Self::activity_text(
                            format!("json-{id}"),
                            &serde_json::to_string_pretty(item).unwrap_or_default(),
                            "json",
                        ))
                    })
                    .into_any_element()
            }
        }
    }

    fn turn(&mut self, index: usize, turn: &Value, cx: &mut Context<Self>) -> AnyElement {
        let projected = self.project_turn(index, turn);
        let outgoing = std::mem::take(&mut self.session.outgoing);
        let items = array(&turn["items"]);
        let item_at = |index: usize| {
            let source = projected
                .sources
                .as_ref()
                .map_or(index, |sources| sources[index]);
            if source < items.len() {
                &items[source]
            } else {
                &outgoing[source - items.len()].item
            }
        };
        let mut body = v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4();
        let opening = &turn["openingUserMessage"];
        if opening.is_object() && !items.iter().any(|item| item["id"] == opening["id"]) {
            body = body.child(self.item(opening, turn, cx));
        }
        for projection in &projected.segments {
            let id = &projection.id;
            let status = text(turn, "status");
            let expanded = self
                .expanded_work
                .get(id)
                .filter(|(previous, _)| previous == status)
                .map_or(projection.initially_expanded, |(_, expanded)| *expanded);
            for item in (projection.start..projection.end)
                .filter(|&index| {
                    projection.role(index, item_at(index)) == conversation_presentation::Role::User
                })
                .map(&item_at)
            {
                body = body.child(self.item(item, turn, cx));
            }
            if let Some(label) = &projection.label {
                let header = if projection.collapsible {
                    let toggle = id.clone();
                    let status = status.to_owned();
                    let turn_id = text(turn, "id").to_owned();
                    button(
                        format!("work-{id}"),
                        format!("{label} {}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |s, _, _| {
                            s.expanded_work
                                .insert(toggle.clone(), (status.clone(), !expanded));
                            s.pause_tail();
                            s.remeasure_item(&turn_id);
                        },
                    )
                    .text_color(rgb(0xa0a0a0))
                    .into_any_element()
                } else {
                    div()
                        .text_color(rgb(0xa0a0a0))
                        .child(label.clone())
                        .into_any_element()
                };
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(header)
                        .when(projection.last && status == "inProgress", |row| {
                            row.child(spinner::Spinner::new().small())
                        }),
                );
            }
            if expanded {
                for item in (projection.start..projection.end)
                    .filter(|&index| {
                        projection.role(index, item_at(index))
                            == conversation_presentation::Role::Activity
                    })
                    .map(&item_at)
                {
                    body = body.child(self.item(item, turn, cx));
                }
            }
            if projection.last && !turn["error"].is_null() {
                body = body.child(
                    div().text_color(rgb(0xff8e86)).child(
                        turn["error"]["message"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| turn["error"].to_string()),
                    ),
                );
            }
            for item in (projection.start..projection.end)
                .filter(|&index| {
                    projection.role(index, item_at(index))
                        == conversation_presentation::Role::Response
                })
                .map(&item_at)
            {
                body = body.child(self.item(item, turn, cx));
                if item["type"] == "agentMessage" && item["phase"] != "commentary" {
                    let turn_id = text(turn, "id").to_owned();
                    let item_id = text(item, "id").to_owned();
                    body = body.child(
                        h_flex().child(
                            Button::new(SharedString::from(format!("copy-{}", text(item, "id"))))
                                .icon(IconName::Copy)
                                .small()
                                .ghost()
                                .tooltip("回答をコピー")
                                .accessibility_label("回答をコピー")
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    if let Some(item) =
                                        array(&s.session.conversation.thread["turns"])
                                            .iter()
                                            .find(|turn| turn["id"] == turn_id)
                                            .and_then(|turn| {
                                                array(&turn["items"])
                                                    .iter()
                                                    .find(|item| item["id"] == item_id)
                                            })
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text(item, "text").to_owned(),
                                        ));
                                    }
                                })),
                        ),
                    );
                }
            }
        }
        self.session.outgoing = outgoing;
        h_flex()
            .justify_center()
            .w_full()
            .px_6()
            .pb_8()
            .child(body)
            .into_any_element()
    }

    fn request_card(&mut self, request: &Value, cx: &mut Context<Self>) -> AnyElement {
        let key = &request["id"];
        let id = request["id"].clone();
        let params = &request["params"];
        let Some(inputs) = self.requests.get(key) else {
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
                    body = body.child(button(
                        format!("answer-{key}-{}-{i}", q.id),
                        option.clone(),
                        cx,
                        move |_, w, cx| input.update(cx, |s, cx| s.set_value(value.clone(), w, cx)),
                    ));
                }
                body = body.child(Input::new(&q.input));
            }
            body = body.child(button(
                format!("respond-{key}"),
                "回答を送信",
                cx,
                move |s, _, cx| {
                    let inputs = &s.requests[&id];
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
                row = row.child(button(
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
                .child(button(
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
                .child(button(
                    format!("deny-{key}"),
                    "拒否",
                    cx,
                    move |s, _, _| {
                        s.respond(denied.clone(), json!({"permissions":{},"scope":"turn"}))
                    },
                ));
        } else {
            body = body
                .child("要求の形式に合わせて回答JSONを入力")
                .child(Textarea::new(&inputs.raw))
                .child(button(
                    format!("raw-{key}"),
                    "回答を送信",
                    cx,
                    move |s, _, cx| match serde_json::from_str(
                        s.requests[&id].raw.read(cx).value().as_ref(),
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
        let entity = cx.entity().downgrade();
        popover::Popover::new("model-controls")
            .bg(rgb(0x2b2b2b))
            .rounded(px(16.))
            .border_color(rgb(0x3b3b3b))
            // Open toward the conversation. Native terminal/browser views in
            // the right panel sit above GPUI's in-window popup layer.
            .anchor(Anchor::BottomRight)
            .trigger(
                Button::new("model-select")
                    .icon(Icon::default().path("bex/gauge.svg").size(px(23.)))
                    .accessibility_label("モデル設定")
                    .tooltip("モデル設定")
                    .large()
                    .w(px(44.))
                    .h(px(44.))
                    .ghost(),
            )
            .content(move |_, _, cx| {
                entity
                    .update(cx, |s, cx| s.model_controls(cx))
                    .unwrap_or_else(|_| div().into_any_element())
            })
            .into_any_element()
    }

    fn model_controls(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let model = self.selected_model(cx);
        let effort_label = match self.effort.as_str() {
            "none" => "なし",
            "minimal" => "最小",
            "low" => "低",
            "medium" => "中",
            "high" => "高",
            "xhigh" => "非常に高",
            "max" => "最大",
            "ultra" => "最高",
            value => value,
        };
        let model_label = format!(
            "{} {}",
            model.map(|m| text(m, "displayName")).unwrap_or("モデル"),
            effort_label
        );
        let models = Button::new("model-choice")
            .label(model_label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let s = owner.read(cx);
                    for model in &s.session.host.state.read(cx).models {
                        let value = text(model, "model").to_owned();
                        let entity = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(text(model, "displayName").to_owned())
                                .checked(value == s.model)
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |s, cx| {
                                        s.select_model(&value, cx);
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                menu
            });
        let entity = cx.entity().downgrade();
        let speed_label = model
            .and_then(|m| {
                array(&m["serviceTiers"])
                    .iter()
                    .find(|t| t["id"] == self.service_tier)
            })
            .map(|t| text(t, "name"))
            .unwrap_or("標準");
        let speed = Button::new("model-speed")
            .label(format!("⚡︎ {speed_label}"))
            .accessibility_label("速度")
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let s = owner.read(cx);
                    let standard = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new("標準")
                            .checked(s.service_tier == "default")
                            .on_click(move |_, _, cx| {
                                let _ = standard.update(cx, |s, cx| {
                                    s.service_tier = "default".into();
                                    cx.notify();
                                });
                            }),
                    );
                    if let Some(model) = s.selected_model(cx) {
                        for tier in array(&model["serviceTiers"])
                            .iter()
                            .filter(|t| t["id"] != "default")
                        {
                            let value = text(tier, "id").to_owned();
                            let entity = entity.clone();
                            menu = menu.item(
                                PopupMenuItem::new(text(tier, "name").to_owned())
                                    .checked(value == s.service_tier)
                                    .on_click(move |_, _, cx| {
                                        let _ = entity.update(cx, |s, cx| {
                                            s.service_tier = value.clone();
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                    }
                }
                menu
            });
        let efforts = model
            .map(|m| array(&m["supportedReasoningEfforts"]))
            .unwrap_or_default();
        v_flex()
            .w(px(280.))
            .gap_2()
            .child(h_flex().justify_between().child(speed).child(models))
            .child(model_effort_slider(&self.effort_slider, efforts.len(), cx))
            .into_any_element()
    }

    fn composer_folder(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        Button::new("composer-folder")
            .label(if self.session.cwd.is_empty() {
                "チャット".into()
            } else {
                basename(&self.session.cwd)
            })
            .accessibility_label(if self.session.cwd.is_empty() {
                "フォルダ: チャット".into()
            } else {
                format!("フォルダ: {}", self.session.cwd)
            })
            .icon(IconName::Folder)
            .dropdown_caret(true)
            .h(px(44.))
            .ghost()
            .disabled(self.busy > 0 || self.dictation.is_some())
            .dropdown_menu(move |mut menu, _, cx| {
                let Some(owner) = entity.upgrade() else {
                    return menu;
                };
                let state = owner.read(cx);
                let unassigned = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new("チャット")
                        .checked(state.session.cwd.is_empty())
                        .on_click(move |_, w, cx| {
                            let _ = unassigned.update(cx, |s, cx| {
                                s.new_thread(String::new(), w, cx);
                                cx.notify();
                            });
                        }),
                );
                for project in &state.session.host.state.read(cx).projects {
                    for root in array(&project["roots"]) {
                        let Some(path) = root["path"].as_str() else {
                            continue;
                        };
                        let path = path.to_owned();
                        let label = if array(&project["roots"]).len() == 1 {
                            text(project, "name").to_owned()
                        } else {
                            path.clone()
                        };
                        let target = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(path == state.session.cwd)
                                .on_click(move |_, w, cx| {
                                    let _ = target.update(cx, |s, cx| {
                                        s.new_thread(path.clone(), w, cx);
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                if state.session.host.state.read(cx).more["hasMoreProjects"] == true {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("さらにプロジェクトを読み込む").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, cx| {
                                host::send(&s.session.host, host::CatalogueInput::MoreProjects);
                                cx.notify();
                            });
                        },
                    ));
                }
                if state.session.host.remote.is_empty() {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("別のフォルダを選択…").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, _| s.pick_folder());
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }

    fn chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let history = list(self.list.clone(), move |ix, _w, cx| {
            entity
                .update(cx, |s, cx| {
                    let ix = if !s.session.conversation.thread.is_null() {
                        if ix == 0 {
                            if s.session.conversation.older_page().is_none() {
                                return div().into_any_element();
                            }
                            let label = if s.session.history_loading {
                                "履歴を読み込み中…".to_owned()
                            } else if !s.session.history_error.is_empty() {
                                format!("{} · 再試行", s.session.history_error)
                            } else {
                                "以前の履歴を読み込む".to_owned()
                            };
                            return h_flex()
                                .justify_center()
                                .p_4()
                                .child(button("older-history", label, cx, |s, w, cx| {
                                    s.load_older(w, cx)
                                }))
                                .into_any_element();
                        }
                        ix - 1
                    } else {
                        ix
                    };
                    let turns = array(&s.session.conversation.thread["turns"]);
                    if ix < turns.len() {
                        let turn = s.session.conversation.thread["turns"][ix].take();
                        let row = s.turn(ix, &turn, cx);
                        s.session.conversation.thread["turns"][ix] = turn;
                        row
                    } else {
                        let pending_index = ix - turns.len();
                        let pending = s
                            .visible_outgoing()
                            .nth(pending_index)
                            .map(|(index, message)| (index, message.accepted));
                        if let Some((index, accepted)) = pending {
                            let item = s.session.outgoing[index].item.take();
                            let row = s.item(&item, &Value::Null, cx);
                            s.session.outgoing[index].item = item;
                            let label = if accepted {
                                "送信済み"
                            } else {
                                "送信中…"
                            };
                            return h_flex()
                                .justify_center()
                                .w_full()
                                .px_6()
                                .pb_8()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .max_w(px(CHAT_WIDTH))
                                        .gap_4()
                                        .child(row)
                                        .child(
                                            div().text_sm().text_color(rgb(0x999999)).child(label),
                                        ),
                                )
                                .into_any_element();
                        }
                        let request = s
                            .visible_requests()
                            .nth(pending_index - s.visible_outgoing().count())
                            .cloned();
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
        if self.session.conversation.thread.is_null() && self.visible_outgoing().next().is_none() {
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(div().text_2xl().child("何から始めましょうか？"))
                    .child(if self.session.cwd.is_empty() {
                        "メッセージを入力して、作業を始めましょう。".into()
                    } else {
                        basename(&self.session.cwd)
                    }),
            );
        } else {
            body = body.child(history);
        }
        let key = self.draft_key();
        let attachments = array(&self.drafts.read(cx).cache(self.draft_scope)["attachments"][&key]);
        let mut files = h_flex().gap_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            files = files.child(button(
                format!("attachment-{i}"),
                format!("{} ×", text(file, "name")),
                cx,
                move |s, _, cx| {
                    let scope = s.draft_scope;
                    drafts::update(&s.drafts, scope, cx, |mut cache| {
                        if let Some(files) = cache["attachments"][&key].as_array_mut()
                            && i < files.len()
                        {
                            files.remove(i);
                        }
                        cache
                    });
                    s.refresh_sources(cx);
                },
            ));
        }
        let running = self.session.conversation.active();
        let empty = self.composer.read(cx).value().trim().is_empty() && attachments.is_empty();
        let phase = self.dictation.as_ref().map(|d| d.phase);
        let recording = phase == Some(Phase::Recording);
        let processing = matches!(phase, Some(Phase::Permission | Phase::Transcribing));
        let send = if let Some(turn) = running.filter(|_| empty && phase.is_none()) {
            let id = turn["id"].clone();
            icon_button("stop", IconName::Pause, "停止", cx, move |s, _, _| {
                s.request(
                    false,
                    "turn/interrupt",
                    json!({"threadId":s.session.selected,"turnId":id}),
                    true,
                    |_, _, _, _| {},
                )
            })
            .icon(Icon::default().path("bex/stop.svg"))
            .disabled(!self.session.connected || self.busy > 0)
        } else {
            icon_button(
                "send",
                IconName::ArrowUp,
                if recording {
                    "文字起こしして送信"
                } else {
                    "送信"
                },
                cx,
                |s, _, cx| s.send(cx),
            )
            .disabled(
                !self.session.connected || self.busy > 0 || (empty && !recording) || processing,
            )
        };
        let microphone = Button::new("dictation-toggle")
            .icon(
                Icon::default()
                    .path(if recording {
                        "bex/stop.svg"
                    } else {
                        "bex/microphone.svg"
                    })
                    .size(px(23.)),
            )
            .ghost()
            .w(px(40.))
            .h(px(40.))
            .large()
            .tooltip(if recording {
                "録音を終了して文字起こし"
            } else {
                "音声をCodexで文字起こし"
            })
            .accessibility_label(if recording {
                "録音を終了して文字起こし"
            } else {
                "音声をCodexで文字起こし"
            })
            .when(recording, |button| button.text_color(rgb(0xff6666)))
            .disabled(!self.session.connected || self.busy > 0 || processing)
            .on_click(cx.listener(|s, _, _, cx| {
                if s.dictation
                    .as_ref()
                    .is_some_and(|d| d.phase == Phase::Recording)
                {
                    s.finish_dictation(false, cx);
                } else {
                    s.start_dictation();
                }
                cx.notify();
            }));
        let composer = v_flex()
            .key_context("ChatComposer")
            .capture_action(cx.listener(Self::paste_image))
            .capture_action(cx.listener(Self::composer_enter))
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .p(px(7.))
            .gap_2()
            .rounded(px(30.))
            .bg(rgb(0x2b2b2b))
            .border_1()
            .border_color(rgb(0x363636))
            .child(
                div().px_2().pt(px(10.)).pb_1().child(
                    Textarea::new(&self.composer)
                        .appearance(false)
                        .bordered(false)
                        .text_size(px(18.))
                        .aria_label("Codex に依頼する"),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        icon_button(
                            "attach",
                            IconName::Plus,
                            "ファイルを添付",
                            cx,
                            |s, _, _| s.attach(),
                        )
                        .w(px(40.))
                        .h(px(40.))
                        .large()
                        .disabled(
                            !self.session.connected
                                || (!self.session.host.remote.is_empty()
                                    && self.session.cwd.is_empty())
                                || self.busy > 0
                                || phase.is_some(),
                        ),
                    )
                    .child(div().flex_1())
                    .child(self.model_menu(cx))
                    .child(microphone)
                    .child(
                        send.large()
                            .rounded(px(22.))
                            .w(px(44.))
                            .h(px(44.))
                            .primary(),
                    ),
            );
        let controls = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .gap_3()
            .when(self.session.selected.is_empty(), |column| {
                column.child(
                    v_flex()
                        .items_start()
                        .gap_1()
                        .child(host_menu(
                            "composer-host",
                            &self.manager_session,
                            &self.session.host.remote,
                            self.busy > 0 || self.dictation.is_some(),
                            cx,
                            |_, id, _, cx| cx.emit(Intent::SelectHost(id)),
                        ))
                        .child(self.composer_folder(cx)),
                )
            })
            .when(
                !self.session.selected.is_empty()
                    && self
                        .review
                        .as_ref()
                        .is_some_and(|review| !review.files.is_empty()),
                |column| column.child(self.review_card(cx)),
            )
            .when(phase.is_some(), |column| {
                column.child(
                    div()
                        .text_sm()
                        .text_color(if recording {
                            rgb(0xff6666)
                        } else {
                            rgb(0xaaaaaa)
                        })
                        .child(match phase {
                            Some(Phase::Permission) => "マイクの許可を確認中…",
                            Some(Phase::Recording) => "録音中",
                            _ => "文字起こし中…",
                        }),
                )
            })
            .child(files)
            .child(composer);
        body.child(
            h_flex()
                .justify_center()
                .px_6()
                .pt_2()
                .pb_4()
                .child(controls),
        )
        .into_any_element()
    }

    fn review_card(&self, cx: &Context<Self>) -> AnyElement {
        let Some(review) = &self.review else {
            return div().into_any_element();
        };
        let files = &review.files;
        let visible = if self.review_expanded {
            files.len()
        } else {
            files.len().min(3)
        };
        v_flex()
            .w_full()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(0x383838))
            .overflow_hidden()
            .bg(rgb(0x191919))
            .child(
                h_flex()
                    .gap_3()
                    .p_3()
                    .bg(rgb(0x232323))
                    .border_b_1()
                    .border_color(rgb(0x383838))
                    .child(
                        div()
                            .p_2()
                            .rounded(px(9.))
                            .bg(rgb(0x141414))
                            .child(Icon::new(IconName::Replace).size_4()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!("{} 件のファイルを変更", files.len())),
                            )
                            .child(review_counts(
                                Some(review.additions),
                                Some(review.deletions),
                            )),
                    )
                    .child(
                        button("review-changes", "レビューする", cx, |_, _, cx| {
                            cx.emit(Intent::Review)
                        })
                        .border_1()
                        .rounded(px(8.))
                        .disabled(!self.session.connected),
                    ),
            )
            .child(
                v_flex()
                    .id("review-file-list")
                    .max_h(px(252.))
                    .overflow_y_scroll()
                    .py_1()
                    .children(files.iter().take(visible).map(|file| {
                        h_flex()
                            .px_3()
                            .h(px(36.))
                            .flex_shrink_0()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(file.path.clone()),
                            )
                            .child(review_counts(file.additions, file.deletions))
                    })),
            )
            .when(files.len() > 3, |card| {
                card.child(
                    div().px_2().py_1().bg(rgb(0x232323)).child(
                        button(
                            "expand-review-files",
                            if self.review_expanded {
                                "折りたたむ".to_owned()
                            } else {
                                format!("あと {} 個のファイルを表示", files.len() - 3)
                            },
                            cx,
                            |s, _, _| s.review_expanded = !s.review_expanded,
                        )
                        .icon(if self.review_expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        }),
                    ),
                )
            })
            .into_any_element()
    }
}

impl Render for ConversationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.image_gallery.is_some() {
            self.cancel_recording();
            return self.image_gallery_view(window, cx);
        }
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .when(!self.error.is_empty(), |body| {
                body.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .bg(rgb(0x352523))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(rgb(0xff8e86))
                                .child(self.error.clone()),
                        )
                        .child(icon_button(
                            "dismiss-chat-error",
                            IconName::Close,
                            "エラーを閉じる",
                            cx,
                            |s, _, _| s.error.clear(),
                        )),
                )
            })
            .child(self.chat(cx))
            .into_any_element()
    }
}
