use super::*;
use gpui_kit::component::{
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem},
    tab::{Tab as UiTab, TabBar},
};
use std::path::PathBuf;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn extra<'a>(item: &'a Item, key: &str) -> &'a Value {
    item.extra.get(key).unwrap_or(&Value::Null)
}
fn field<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

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
            self.remote_key(),
            self.snapshot.navigation.cwd,
            source
        );
        if !self.images.contains_key(&key) {
            let source = Arc::new(source.to_owned());
            self.images.insert(
                key.clone(),
                ImageState {
                    source: source.clone(),
                    path: None,
                    error: None,
                },
            );
            let cwd = self.snapshot.navigation.cwd.clone();
            let remote = self.remote.is_some();
            let store = self.store.clone();
            let destination = self
                .image_dir
                .path()
                .join(format!("image-{}", self.images.len()));
            let returned = key.clone();
            self.effect(
                async move {
                    if encoded || source.starts_with("data:image/") {
                        let data = if encoded {
                            source.as_str()
                        } else {
                            let (header, data) =
                                source.split_once(',').ok_or("画像データが不正です")?;
                            if !header.ends_with(";base64") {
                                return Err("未対応の画像データです".into());
                            }
                            data
                        };
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .map_err(|error| error.to_string())?;
                        tokio::fs::write(&destination, bytes)
                            .await
                            .map_err(|error| error.to_string())?;
                        return Ok(destination.to_string_lossy().into_owned());
                    }
                    if source.starts_with("https://") || source.starts_with("http://") {
                        return Ok(source.as_ref().clone());
                    }
                    let path = conversation_file_path(&source, &cwd)?;
                    if !remote {
                        return Ok(path.to_string_lossy().into_owned());
                    }
                    store
                        .ok_or("Host に接続していません")?
                        .dispatch(Intent::DownloadFile {
                            source: path,
                            destination: destination.clone(),
                        })
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(destination.to_string_lossy().into_owned())
                },
                move |view, result, _, _| {
                    if let Some(image) = view.images.get_mut(&returned) {
                        match result {
                            Ok(path) => {
                                image.path = Some(if Path::new(&path).is_absolute() {
                                    PathBuf::from(path).into()
                                } else {
                                    ImageSource::from(path)
                                })
                            }
                            Err(error) => image.error = Some(error),
                        }
                    }
                    view.list.remeasure();
                },
            );
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
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.open_image_gallery(source.clone(), encoded, cx)
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
        let path = match conversation_file_path(source, &self.snapshot.navigation.cwd) {
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
            self.open_image_gallery(Arc::new(path.to_string_lossy().into_owned()), false, cx);
            return;
        }
        let store = self.store.clone();
        let remote = self.remote.is_some();
        let directory = self.image_dir.path().to_owned();
        self.effect(
            async move {
                let path = if remote {
                    let download = tempfile::Builder::new()
                        .prefix("link-")
                        .tempdir_in(directory)
                        .map_err(|error| error.to_string())?
                        .keep();
                    let destination =
                        download.join(path.file_name().ok_or("ファイル名がありません")?);
                    store
                        .ok_or("Host に接続していません")?
                        .dispatch(Intent::DownloadFile {
                            source: path,
                            destination: destination.clone(),
                        })
                        .await
                        .map_err(|error| error.to_string())?;
                    destination
                } else {
                    path
                };
                let path = tokio::fs::canonicalize(path)
                    .await
                    .map_err(|error| error.to_string())?;
                url::Url::from_file_path(path).map_err(|_| "ファイルパスが不正です".into())
            },
            |view, result, _, cx| match result {
                Ok(url) => cx.open_url(url.as_str()),
                Err(error) => view.error = error,
            },
        );
    }

    fn open_image_gallery(&mut self, source: Arc<String>, encoded: bool, cx: &mut Context<Self>) {
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
        self.perform(
            Intent::LoadSessionImages(self.selected().into()),
            move |view, result, _, _| {
                let Some(gallery) = view
                    .image_gallery
                    .as_mut()
                    .filter(|gallery| gallery.id == id)
                else {
                    return;
                };
                gallery.loading = false;
                match result {
                    Ok(Outcome::SessionImages(images)) => {
                        let initial = gallery.current_image().clone();
                        let mut seen = HashSet::new();
                        let entries: Vec<_> = images
                            .into_iter()
                            .filter_map(|image| {
                                let entry = (Arc::new(image.source), image.encoded);
                                seen.insert(entry.clone()).then_some(entry)
                            })
                            .collect();
                        gallery.selected = entries.iter().position(|entry| entry == &initial);
                        gallery
                            .list
                            .splice(0..gallery.list.item_count(), entries.len());
                        gallery.entries = entries;
                        if let Some(index) = gallery.selected {
                            gallery.list.scroll_to_reveal_item(index);
                        }
                    }
                    Err(error) => gallery.error = error,
                    _ => unreachable!("session image outcome"),
                }
            },
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
            self.remote_key(),
            self.snapshot.navigation.cwd,
            source
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
                        self.button(
                            "gallery-save",
                            if saved { "保存済み" } else { "保存" },
                            cx,
                            |s, _, cx| s.save_gallery_image(cx),
                        )
                        .disabled(!ready || saving || saved),
                    )
                    .child(
                        self.button("gallery-close", "閉じる", cx, |s, _, _| {
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
            self.remote
                .as_ref()
                .map_or("local", |remote| remote.id.as_str()),
            self.snapshot.navigation.cwd,
            source
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
                        let mode = format!("image.{}", format.extensions_str()[0]);
                        let Some(destination) = platform::choose_destination(&mode) else {
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

    fn diff(
        diffs: &mut HashMap<String, Entity<DiffView>>,
        id: String,
        patch: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = diffs
            .entry(id.clone())
            .or_insert_with(|| {
                cx.new(|_| DiffView::new(patch.to_owned().into(), id == "workspace-patch"))
            })
            .clone();
        view.update(cx, |s, cx| s.set_source(patch, cx));
        view.into_any_element()
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
    fn item(&mut self, item: &Item, turn: Option<&Turn>, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let kind = item.kind.as_deref().unwrap_or_default();
        let expanded = self.expanded_items.contains(&id);
        let deferred = turn
            .and_then(|turn| turn.deferred_item_ids.as_ref())
            .is_some_and(|ids| ids.contains(&id));
        if expanded && deferred {
            let key = (
                turn.map(|turn| turn.id.clone()).unwrap_or_default(),
                id.clone(),
            );
            let label = match self
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
                .child(self.button(
                    format!("collapse-{id}"),
                    format!("⌄ {title}"),
                    cx,
                    move |s, _, _| {
                        s.expanded_items.remove(&toggle);
                        s.remeasure_item(&toggle);
                    },
                ))
                .child(
                    self.button(format!("detail-{id}"), label, cx, move |s, _, _| {
                        s.detail(key.0.clone(), key.1.clone());
                        s.remeasure_item(&key.0);
                    }),
                )
                .into_any_element();
        }
        let turn_id = turn.map(|turn| turn.id.clone()).unwrap_or_default();
        match kind {
            "agentMessage" => self.markdown(id, item.text.as_deref().unwrap_or_default(), cx),
            "imageGeneration" => {
                let path = item.saved_path.as_deref().unwrap_or_default();
                let result = item
                    .result
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let title = conversation_presentation::item_presentation(item).title;
                let mut body = v_flex()
                    .gap_2()
                    .w_full()
                    .child(div().text_sm().child(title));
                if !path.is_empty() {
                    body = body.child(self.image(path, false, 320., true, cx));
                    let path = path.to_owned();
                    body = body.child(self.button(
                        format!("open-image-{id}"),
                        "画像を開く",
                        cx,
                        move |s, _, cx| {
                            s.open_image_gallery(std::sync::Arc::new(path.clone()), false, cx)
                        },
                    ));
                } else if !result.is_empty() {
                    body = body.child(self.image(result, true, 320., true, cx));
                } else if item.status.as_deref().unwrap_or_default() == "inProgress" {
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
                if extra(item, "content").is_array() {
                    for (i, part) in array(extra(item, "content")).iter().enumerate() {
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
                                self.button(
                                    format!("{id}-{i}"),
                                    if path.is_empty() {
                                        part.to_string()
                                    } else {
                                        file_name(&path)
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
                            literal(item.text.as_deref().unwrap_or_default()),
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
                let mut body = v_flex().gap_2().child(self.button(
                    format!("expand-{id}"),
                    format!("{} {label}", if expanded { "⌄" } else { "›" }),
                    cx,
                    move |s, _, _| {
                        toggle_set(&mut s.expanded_items, &toggle);
                        if s.expanded_items.contains(&toggle) {
                            s.detail(turn_id.clone(), toggle.clone());
                        }
                        s.pause_tail();
                        s.remeasure_item(&toggle);
                    },
                ));
                if expanded {
                    let content = if kind == "reasoning" {
                        if extra(item, "summary").is_array() {
                            array(extra(item, "summary"))
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            extra(item, "summary")
                                .as_str()
                                .or(item.text.as_deref())
                                .unwrap_or("")
                                .to_owned()
                        }
                    } else {
                        format!(
                            "$ {}\n\n{}",
                            item.command.as_deref().unwrap_or_default(),
                            item.aggregated_output.as_deref().unwrap_or_default()
                        )
                    };
                    body = body
                        .child(Self::activity_text(format!("output-{id}"), &content, ""))
                        .child(div().text_sm().text_color(rgb(0x999999)).child(format!(
                                "{} {}",
                                item.status.as_deref().unwrap_or_default(),
                                item.extra.get("exitCode")
                                    .map(|v| format!("exit {v}"))
                                    .unwrap_or_default()
                            )));
                }
                body.into_any_element()
            }
            "fileChange" => {
                let mut body = v_flex().gap_3();
                for (i, change) in array(extra(item, "changes")).iter().enumerate() {
                    let path = text(change, "path").to_owned();
                    let toggle = id.clone();
                    let turn_id = turn_id.clone();
                    let row = h_flex()
                        .gap_2()
                        .child(self.button(
                            format!("change-{id}-{i}"),
                            format!("{} {}", if expanded { "⌄" } else { "›" }, file_name(&path)),
                            cx,
                            move |s, _, _| {
                                toggle_set(&mut s.expanded_items, &toggle);
                                if s.expanded_items.contains(&toggle) {
                                    s.detail(turn_id.clone(), toggle.clone());
                                }
                                s.pause_tail();
                                s.remeasure_item(&toggle);
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
                        body = body.child(Self::diff(
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
                    .child(self.button(
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
                                s.detail(turn_id.clone(), toggle.clone());
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
    fn turn(&mut self, turn: &Arc<Turn>, cx: &mut Context<Self>) -> AnyElement {
        let projected = self.project_turn(turn);
        let items = turn.items.as_deref().unwrap_or_default();
        let mut body = v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4();
        if let Some(opening) = &turn.opening_user_message
            && !items.iter().any(|item| item.id == opening.id)
        {
            body = body.child(self.item(opening, Some(turn), cx));
        }
        for projection in &projected.segments {
            let id = &projection.id;
            let status = turn.status.as_deref().unwrap_or_default();
            let expanded = self
                .expanded_work
                .get(id)
                .filter(|(previous, _)| previous == status)
                .map_or(projection.initially_expanded, |(_, expanded)| *expanded);
            for index in (projection.start..projection.end).filter(|&index| {
                projection.role(index, projected.metadata(index))
                    == conversation_presentation::Role::User
            }) {
                body = body.child(self.projected_item(&projected, index, cx));
            }
            if let Some(label) = &projection.label {
                let header = if projection.collapsible {
                    let toggle = id.clone();
                    let status = status.to_owned();
                    let turn_id = turn.id.clone();
                    self.button(
                        format!("work-{id}"),
                        format!("{label} {}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |view, _, _| {
                            view.expanded_work
                                .insert(toggle.clone(), (status.clone(), !expanded));
                            view.pause_tail();
                            view.remeasure_item(&turn_id);
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
                for index in (projection.start..projection.end).filter(|&index| {
                    projection.role(index, projected.metadata(index))
                        == conversation_presentation::Role::Activity
                }) {
                    body = body.child(self.projected_item(&projected, index, cx));
                }
            }
            if projection.last
                && let Some(error) = turn.error.as_ref().filter(|error| !error.is_null())
            {
                body = body.child(
                    div().text_color(rgb(0xff8e86)).child(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| error.to_string()),
                    ),
                );
            }
            for index in (projection.start..projection.end).filter(|&index| {
                projection.role(index, projected.metadata(index))
                    == conversation_presentation::Role::Response
            }) {
                body = body.child(self.projected_item(&projected, index, cx));
                if let Some(item) = items.get(projected.sources[index])
                    && item.kind.as_deref() == Some("agentMessage")
                    && extra(item, "phase") != "commentary"
                {
                    let item = item.clone();
                    body = body.child(
                        h_flex().child(
                            Button::new(format!("copy-{}", item.id))
                                .icon(IconName::Copy)
                                .small()
                                .ghost()
                                .tooltip("回答をコピー")
                                .accessibility_label("回答をコピー")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        item.text.clone().unwrap_or_default(),
                                    ));
                                }),
                        ),
                    );
                }
            }
        }
        h_flex()
            .justify_center()
            .w_full()
            .px_6()
            .pb_8()
            .child(body)
            .into_any_element()
    }
    fn projected_item(
        &mut self,
        projected: &ProjectedTurn,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items = projected.turn.items.as_deref().unwrap_or_default();
        let source = projected.sources[index];
        if let Some(item) = items.get(source) {
            self.item(item, Some(&projected.turn), cx)
        } else {
            let (id, pending) = &projected.pending[source - items.len()];
            self.pending_item(id, &pending.draft, cx)
        }
    }
    fn pending_item(&mut self, id: &str, draft: &Draft, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .gap_3()
            .max_w(px(560.))
            .p_4()
            .rounded(px(18.))
            .bg(rgb(0x303030));
        if !draft.text.is_empty() {
            body = body.child(
                TextView::markdown(
                    SharedString::from(format!("pending-{id}")),
                    literal(&draft.text),
                )
                .selectable(true),
            );
        }
        for (index, attachment) in draft.attachments.iter().enumerate() {
            if attachment.is_image {
                body = body.child(self.image(&attachment.path, false, 320., true, cx));
            } else {
                let path = attachment.path.clone();
                body = body.child(self.button(
                    format!("pending-{id}-file-{index}"),
                    attachment.name.clone(),
                    cx,
                    move |view, _, _| view.download(path.clone()),
                ));
            }
        }
        h_flex()
            .w_full()
            .justify_end()
            .my_4()
            .child(body)
            .into_any_element()
    }

    fn request_card(
        &mut self,
        key: &str,
        request: &ServerRequest,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &request.id;
        let params = &request.params;
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
            for question in &inputs.questions {
                body = body.child(question.prompt.clone());
                for (index, option) in question.options.iter().enumerate() {
                    let input = question.input.clone();
                    let value = option.clone();
                    body = body.child(self.button(
                        format!("answer-{key}-{}-{index}", question.id),
                        option.clone(),
                        cx,
                        move |_, window, cx| {
                            input.update(cx, |input, cx| input.set_value(value.clone(), window, cx))
                        },
                    ));
                }
                body = body.child(Input::new(&question.input));
            }
            let key = key.to_owned();
            let id = id.clone();
            body = body.child(self.button(
                format!("respond-{key}"),
                "回答を送信",
                cx,
                move |view, _, cx| {
                    let Some(inputs) = view.requests.get(&key) else {
                        return;
                    };
                    let answers = inputs
                        .questions
                        .iter()
                        .map(|question| {
                            (
                                question.id.clone(),
                                question.input.read(cx).value().to_string(),
                            )
                        })
                        .collect();
                    view.respond(key.clone(), id.clone(), Answer::Questions(answers));
                },
            ));
        } else if matches!(
            request.method.as_str(),
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        ) {
            let defaults = ["accept", "acceptForSession", "decline", "cancel"];
            let decisions = field(params, "availableDecisions").as_array();
            let mut row = h_flex().gap_2().flex_wrap();
            for index in 0..decisions.map_or(defaults.len(), Vec::len) {
                let decision = decisions.and_then(|decisions| decisions.get(index));
                let value = decision
                    .and_then(Value::as_str)
                    .or_else(|| decisions.is_none().then(|| defaults[index]));
                let label = match value {
                    Some("accept") => "許可".into(),
                    Some("acceptForSession") => "このセッションで許可".into(),
                    Some("decline") => "拒否".into(),
                    Some("cancel") => "中止".into(),
                    _ => decision.map(Value::to_string).unwrap_or_default(),
                };
                let key = key.to_owned();
                let id = id.clone();
                row = row.child(self.button(
                    format!("decision-{key}-{index}"),
                    label,
                    cx,
                    move |view, _, _| {
                        view.respond(key.clone(), id.clone(), Answer::Decision(index))
                    },
                ));
            }
            body = body.child(row);
        } else if request.method == "item/permissions/requestApproval" {
            for (allow, label) in [(true, "今回の権限を許可"), (false, "拒否")] {
                let key = key.to_owned();
                let id = id.clone();
                body = body.child(self.button(
                    format!("permissions-{key}-{allow}"),
                    label,
                    cx,
                    move |view, _, _| {
                        view.respond(key.clone(), id.clone(), Answer::Permissions(allow))
                    },
                ));
            }
        } else {
            let key = key.to_owned();
            let id = id.clone();
            body = body
                .child("要求の形式に合わせて回答JSONを入力")
                .child(Textarea::new(&inputs.raw))
                .child(self.button(
                    format!("raw-{key}"),
                    "回答を送信",
                    cx,
                    move |view, _, cx| {
                        let Some(inputs) = view.requests.get(&key) else {
                            return;
                        };
                        match serde_json::value::RawValue::from_string(
                            inputs.raw.read(cx).value().to_string(),
                        ) {
                            Ok(value) => view.respond(key.clone(), id.clone(), Answer::Raw(value)),
                            Err(error) => view.error = error.to_string(),
                        }
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
    pub(super) fn sync_request_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.requests
            .retain(|key, _| self.snapshot.requests.contains_key(key));
        for (key, request) in self.snapshot.requests.iter() {
            if self.requests.contains_key(key) {
                continue;
            }
            let questions = array(field(&request.params, "questions"))
                .iter()
                .map(|question| Question {
                    id: text(question, "id").into(),
                    prompt: text(question, "question").into(),
                    options: array(&question["options"])
                        .iter()
                        .map(|option| text(option, "label").into())
                        .collect(),
                    input: cx.new(|cx| InputState::new(window, cx).placeholder("回答を入力")),
                })
                .collect();
            self.requests.insert(
                key.clone(),
                RequestInputs {
                    questions,
                    raw: cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .default_value("{}")
                            .auto_grow(3, 8)
                    }),
                    sent: false,
                },
            );
        }
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
        let model = self.selected_model();
        let effort = self
            .draft()
            .effort
            .as_deref()
            .or_else(|| model.map(|model| model.default_reasoning_effort.as_str()))
            .unwrap_or_default();
        let effort_label = match effort {
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
            "{} {effort_label}",
            model.map_or("モデル", |model| model.display_name.as_str())
        );
        let models = Button::new("model-choice")
            .label(model_label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    for model in view.snapshot.models.iter() {
                        let value = model.model.clone();
                        let entity = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(model.display_name.clone())
                                .checked(
                                    view.selected_model()
                                        .is_some_and(|model| model.model == value),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        view.dispatch(Intent::SelectModel {
                                            thread_id: view.draft_key().into(),
                                            model: value.clone(),
                                        });
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                menu
            });
        let entity = cx.entity().downgrade();
        let tier = self
            .draft()
            .service_tier
            .as_deref()
            .or_else(|| model.and_then(|model| model.default_service_tier.as_deref()))
            .unwrap_or("default");
        let speed_label = model
            .and_then(|model| model.service_tiers.as_deref())
            .and_then(|tiers| tiers.iter().find(|value| value.id == tier))
            .map(|tier| {
                tier.extra
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&tier.id)
            })
            .unwrap_or("標準");
        let speed = Button::new("model-speed")
            .label(format!("⚡︎ {speed_label}"))
            .accessibility_label("速度")
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    let standard = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new("標準")
                            .checked(view.draft().service_tier.as_deref() == Some("default"))
                            .on_click(move |_, _, cx| {
                                let _ = standard.update(cx, |view, cx| {
                                    view.dispatch(Intent::SelectServiceTier {
                                        thread_id: view.draft_key().into(),
                                        service_tier: "default".into(),
                                    });
                                    cx.notify();
                                });
                            }),
                    );
                    if let Some(model) = view.selected_model() {
                        for tier in model
                            .service_tiers
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .filter(|tier| tier.id != "default")
                        {
                            let value = tier.id.clone();
                            let entity = entity.clone();
                            let label = tier
                                .extra
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or(&tier.id)
                                .to_owned();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(
                                        view.draft()
                                            .service_tier
                                            .as_deref()
                                            .or(model.default_service_tier.as_deref())
                                            == Some(&value),
                                    )
                                    .on_click(move |_, _, cx| {
                                        let _ = entity.update(cx, |view, cx| {
                                            view.dispatch(Intent::SelectServiceTier {
                                                thread_id: view.draft_key().into(),
                                                service_tier: value.clone(),
                                            });
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                    }
                }
                menu
            });
        v_flex()
            .w(px(280.))
            .gap_2()
            .child(h_flex().justify_between().child(speed).child(models))
            .child(model_effort_slider(
                &self.effort_slider,
                model.map_or(0, |model| model.supported_reasoning_efforts.len()),
                cx,
            ))
            .into_any_element()
    }

    fn host_menu(&self, id: &'static str, cx: &Context<Self>) -> AnyElement {
        if let Some(hosts) = &self.hosts {
            Hosts::menu(
                hosts,
                id,
                self.remote.as_ref().map(|remote| remote.id.as_str()),
                self.busy > 0 || self.dictation.is_some(),
                cx,
            )
            .into_any_element()
        } else {
            div()
                .child(
                    self.remote
                        .as_ref()
                        .map_or("この端末", |remote| remote.name.as_str())
                        .to_owned(),
                )
                .into_any_element()
        }
    }

    fn composer_folder(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        Button::new("composer-folder")
            .label(if self.snapshot.navigation.cwd.is_empty() {
                "チャット".into()
            } else {
                file_name(&self.snapshot.navigation.cwd)
            })
            .accessibility_label(if self.snapshot.navigation.cwd.is_empty() {
                "フォルダ: チャット".into()
            } else {
                format!("フォルダ: {}", self.snapshot.navigation.cwd)
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
                        .checked(state.snapshot.navigation.cwd.is_empty())
                        .on_click(move |_, _, cx| {
                            let _ = unassigned.update(cx, |s, cx| {
                                s.new_chat(String::new());
                                cx.notify();
                            });
                        }),
                );
                for project in state
                    .snapshot
                    .threads
                    .as_ref()
                    .map(|page| page.projects.as_slice())
                    .unwrap_or_default()
                {
                    for root in &project.roots {
                        let path = root.path.clone();
                        let label = if project.roots.len() == 1 {
                            project.name.clone()
                        } else {
                            path.clone()
                        };
                        let target = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(path == state.snapshot.navigation.cwd)
                                .on_click(move |_, _, cx| {
                                    let _ = target.update(cx, |s, cx| {
                                        s.new_chat(path.clone());
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                if state
                    .snapshot
                    .threads
                    .as_ref()
                    .is_some_and(|page| page.has_more_projects)
                {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("さらにプロジェクトを読み込む").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, cx| {
                                let mut query = (*s.snapshot.list_query).clone();
                                query.project_limit += 10;
                                s.dispatch(Intent::ListThreads(query));
                                cx.notify();
                            });
                        },
                    ));
                }
                if state.remote.is_none() {
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
    fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let navigation = SidebarMenu::new().gap_1().child(
            SidebarMenuItem::new("新しいチャット")
                .icon(IconName::Plus)
                .disable(!self.snapshot.connected)
                .on_click(cx.listener(|s, _, _, cx| {
                    s.new_chat(String::new());
                    cx.notify();
                })),
        );
        let mut projects = SidebarMenu::new().gap_1();
        for project in self
            .snapshot
            .threads
            .as_ref()
            .map(|page| page.projects.as_slice())
            .unwrap_or_default()
        {
            let id = project.id.clone();
            let expanded = self.expanded_projects.contains(&id)
                || !self.snapshot.list_query.search_term.is_empty();
            let toggle = id.clone();
            let new_root = project
                .roots
                .first()
                .map(|root| root.path.clone())
                .unwrap_or_default();
            let entity = cx.entity().downgrade();
            let connected = self.snapshot.connected;
            projects = projects.child(
                SidebarMenuItem::new(project.name.clone())
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
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |s, cx| {
                                    s.new_chat(path.clone());
                                    cx.notify();
                                });
                            })
                    }),
            );
            if expanded {
                for thread in self
                    .snapshot
                    .threads
                    .as_ref()
                    .map(|page| page.data.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter(|thread| {
                        thread.project_id.as_ref().and_then(Option::as_deref) == Some(&project.id)
                    })
                {
                    projects =
                        projects.child(self.thread_button(thread, cx).icon(Icon::empty().size_4()));
                }
                if self
                    .snapshot
                    .threads
                    .as_ref()
                    .is_some_and(|page| page.more_project_ids.contains(&project.id))
                {
                    projects = projects.child(
                        SidebarMenuItem::new("もっと表示する")
                            .icon(Icon::empty().size_4())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                let mut query = (*s.snapshot.list_query).clone();
                                *query.project_thread_limits.entry(id.clone()).or_insert(5) += 10;
                                s.dispatch(Intent::ListThreads(query));
                                cx.notify();
                            })),
                    );
                }
            }
        }
        if self
            .snapshot
            .threads
            .as_ref()
            .is_some_and(|page| page.has_more_projects)
        {
            projects = projects.child(SidebarMenuItem::new("もっとプロジェクトを表示").on_click(
                cx.listener(|s, _, _, cx| {
                    let mut query = (*s.snapshot.list_query).clone();
                    query.project_limit += 10;
                    s.dispatch(Intent::ListThreads(query));
                    cx.notify();
                }),
            ));
        }
        let mut chats = SidebarMenu::new().gap_1();
        for thread in self
            .snapshot
            .threads
            .as_ref()
            .map(|page| page.data.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|thread| {
                thread
                    .project_id
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_none()
            })
        {
            chats = chats.child(self.thread_button(thread, cx));
        }
        if self
            .snapshot
            .threads
            .as_ref()
            .is_some_and(|page| page.has_more_chats)
        {
            chats = chats.child(SidebarMenuItem::new("もっと表示する").on_click(cx.listener(
                |s, _, _, cx| {
                    let mut query = (*s.snapshot.list_query).clone();
                    query.chat_limit += 10;
                    s.dispatch(Intent::ListThreads(query));
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
                                .disabled(!self.snapshot.connected || self.remote.is_some()),
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
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .bg(rgb(if self.snapshot.connected {
                                0x37cf77
                            } else {
                                0x999999
                            })),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            self.button(
                                "sidebar-settings",
                                self.remote
                                    .as_ref()
                                    .map_or("この端末", |remote| remote.name.as_str())
                                    .to_owned(),
                                cx,
                                |s, _, _| {
                                    s.tab = Tab::Settings;
                                    s.dispatch(Intent::ReadWorktreeSettings);
                                },
                            )
                            .accessibility_label("設定を開く")
                            .tooltip("設定を開く")
                            .selected(self.tab == Tab::Settings),
                        ),
                    )
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
    fn thread_button(&self, thread: &Thread, cx: &Context<Self>) -> SidebarMenuItem {
        let id = thread.id.clone().unwrap_or_default();
        let active = self
            .snapshot
            .activity
            .active
            .get(&id)
            .copied()
            .unwrap_or_else(|| {
                thread
                    .status
                    .as_ref()
                    .is_some_and(|status| status.kind == "active")
            });
        SidebarMenuItem::new(
            thread
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or("新しいチャット")
                .to_owned(),
        )
        .active(id == self.selected() && self.tab != Tab::Settings)
        .disable(self.busy > 0)
        .when(active, |item| {
            item.suffix(|_, _| spinner::Spinner::new().small())
        })
        .when(
            !active && self.snapshot.activity.unread.contains(&id),
            |item| item.suffix(|_, _| div().size(px(8.)).rounded_full().bg(rgb(0xffffff))),
        )
        .on_click(cx.listener(move |view, _, _, cx| {
            view.open_chat(id.clone());
            cx.notify();
        }))
    }

    fn chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let history = list(self.list.clone(), move |index, _, cx| {
            entity
                .update(cx, |view, cx| match view.rows.get(index).cloned() {
                    Some(ConversationRow::History) => {
                        if view.older_page().is_none() {
                            return div().into_any_element();
                        }
                        let label = if view.history_loading {
                            "履歴を読み込み中…".into()
                        } else if !view.history_error.is_empty() {
                            format!("{} · 再試行", view.history_error)
                        } else {
                            "以前の履歴を読み込む".into()
                        };
                        h_flex()
                            .justify_center()
                            .p_4()
                            .child(view.button("older-history", label, cx, |view, window, cx| {
                                view.older(window, cx)
                            }))
                            .into_any_element()
                    }
                    Some(ConversationRow::Turn(turn)) => view.turn(&turn, cx),
                    Some(ConversationRow::Pending(id, pending)) => {
                        let row = view.pending_item(&id, &pending.draft, cx);
                        h_flex()
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
                                    .child(div().text_sm().text_color(rgb(0x999999)).child(
                                        if pending.accepted {
                                            "送信済み"
                                        } else {
                                            "送信中…"
                                        },
                                    )),
                            )
                            .into_any_element()
                    }
                    Some(ConversationRow::Request(key, request)) => {
                        view.request_card(&key, &request, cx)
                    }
                    None => div().into_any_element(),
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .flex_1()
        .min_h_0();
        let mut body = v_flex().flex_1().min_w_0().h_full();
        if self.thread().is_none() && self.pending_rows().next().is_none() {
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(div().text_2xl().child("何から始めましょうか？"))
                    .child(if self.snapshot.navigation.cwd.is_empty() {
                        "メッセージを入力して、作業を始めましょう。".into()
                    } else {
                        file_name(&self.snapshot.navigation.cwd)
                    }),
            );
        } else {
            body = body.child(history);
        }
        let key = self.draft_key().to_owned();
        let attachments = &self.draft().attachments;
        let mut files = h_flex().gap_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            files = files.child(self.button(
                format!("attachment-{i}"),
                format!("{} ×", file.name),
                cx,
                move |s, _, _| {
                    s.dispatch(Intent::RemoveAttachment {
                        draft_key: key.clone(),
                        index: i,
                    });
                },
            ));
        }
        let running = self.active_turn();
        let empty = self.composer.read(cx).value().trim().is_empty() && attachments.is_empty();
        let phase = self.dictation.as_ref().map(|d| d.phase);
        let recording = phase == Some(Phase::Recording);
        let processing = matches!(phase, Some(Phase::Permission | Phase::Transcribing));
        let send = if let Some(turn) = running.filter(|_| empty && phase.is_none()) {
            let id = turn.id.clone();
            self.icon_button("stop", IconName::Pause, "停止", cx, move |s, _, _| {
                s.dispatch(Intent::Interrupt {
                    thread_id: s.selected().into(),
                    turn_id: id.clone(),
                });
            })
            .icon(Icon::default().path("bex/stop.svg"))
            .disabled(!self.snapshot.connected || self.busy > 0)
        } else {
            self.icon_button(
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
                !self.snapshot.connected || self.busy > 0 || (empty && !recording) || processing,
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
            .disabled(!self.snapshot.connected || self.busy > 0 || processing)
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
                        .aria_label("Codex に依頼する")
                        .readonly(!self.snapshot.connected),
                ),
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
                        .w(px(40.))
                        .h(px(40.))
                        .large()
                        .disabled(
                            !self.snapshot.connected
                                || (!self.remote.is_none()
                                    && self.snapshot.navigation.cwd.is_empty())
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
            .when(self.selected().is_empty(), |column| {
                column.child(
                    v_flex()
                        .items_start()
                        .gap_1()
                        .child(self.host_menu("composer-host", cx))
                        .child(self.composer_folder(cx)),
                )
            })
            .when(
                !self.selected().is_empty()
                    && self
                        .snapshot
                        .workspace
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
        let review = self.snapshot.workspace.review.as_deref();
        let files = review
            .map(|review| review.files.as_slice())
            .unwrap_or_default();
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
                                Some(review.map_or(0, |review| review.additions)),
                                Some(review.map_or(0, |review| review.deletions)),
                            )),
                    )
                    .child(
                        self.button("review-changes", "レビューする", cx, |s, _, _| {
                            s.panel = Panel::Diff;
                            s.panel_open = true;
                            s.tab = Tab::Chat;
                            s.refresh_review();
                        })
                        .border_1()
                        .rounded(px(8.))
                        .disabled(!self.snapshot.connected),
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
                        self.button(
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
    fn files(&self, cx: &Context<Self>) -> AnyElement {
        let mut entries = v_flex().gap_1();
        for (i, entry) in self
            .snapshot
            .workspace
            .directory
            .as_ref()
            .map(|directory| directory.entries.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let path = entry.path.clone();
            let download = path.clone();
            let directory = entry.directory;
            entries = entries.child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        self.button(
                            format!("file-{i}"),
                            entry.name.clone(),
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
        if let Some(file) = &self.snapshot.workspace.file {
            editor = editor
                .child(
                    h_flex().gap_2().child(IconName::FileText).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(file.path.clone()),
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
                            self.button("save-file", "保存", cx, |s, _, _| s.save_file())
                                .primary()
                                .disabled(self.busy > 0),
                        )
                        .child(self.icon_button(
                            "reload-file",
                            IconName::RotateCw,
                            "再読み込み・下書きを破棄",
                            cx,
                            |s, _, _| {
                                if let Some(file) = &s.snapshot.workspace.file {
                                    s.edit(file.path.clone(), true);
                                }
                            },
                        ))
                        .child(
                            self.button("ai-edit", "AI に編集を依頼", cx, |s, _, _| {
                                if let Some(file) = &s.snapshot.workspace.file {
                                    s.dispatch(Intent::SetDraftText {
                                        thread_id: s.draft_key().into(),
                                        text: format!(
                                            "ファイル {} を編集してください。\n\n",
                                            file.path
                                        ),
                                    });
                                }
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
            .child(self.host_menu("settings-host", cx));
        body = body.child(
            v_flex().gap_3()
                .child(div().text_xl().child("ワークツリー"))
                .child("選択中の Host に保存し、すべての端末 からの新規セッションに適用します。")
                .child(switch::Switch::new("worktree-create")
                    .label("新規セッションをワークツリーで開始")
                    .checked(self.snapshot.workspace.settings.as_ref().is_some_and(|settings| settings.create_on_new_session))
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some((true, *checked)), cx);
                        cx.notify();
                    })))
                .child("ワークツリーの保存先")
                .child(Input::new(&self.worktree_directory).aria_label("ワークツリーの保存先")
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0))
                .child("指定フォルダ内にセッションごとのフォルダを作ります。空欄ならリポジトリのGit管理領域に保存します。既存のワークツリーは移動しません。")
                .child(switch::Switch::new("worktree-copy")
                    .label("ワークツリー作成時にファイルをコピー")
                    .checked(self.snapshot.workspace.settings.as_ref().is_some_and(|settings| settings.copy_on_create))
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some((false, *checked)), cx);
                        cx.notify();
                    })))
                .child("コピー対象（リポジトリからの相対パスを1行に1つ）")
                .child(Textarea::new(&self.worktree_copy_paths).aria_label("コピー対象")
                    .readonly(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0))
                .child("例: .env、.env.local、config/local。存在しないパスはスキップします。指定したファイルはコピー元の内容で置き換えます。シンボリックリンクはコピーできません。")
                .child("最初のメッセージ送信時に現在の HEAD から作成します。既存セッションを開き直しても作成・コピーしません。")
                .child(div().text_sm().text_color(rgb(0x999999)).child(if self.worktree_saved {
                    "保存しました"
                } else {
                    "スイッチは切り替え時、入力欄は入力を終えると自動保存します。"
                }))
        );
        if let Some(hosts) = &self.hosts {
            body = body.child(hosts.clone());
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
        let enabled = self.snapshot.connected && !self.snapshot.navigation.cwd.is_empty();
        let mut sources = v_flex().gap_1();
        for (i, path) in self.source_paths.iter().enumerate() {
            let path = path.clone();
            sources = sources.child(
                self.button(
                    format!("source-{i}"),
                    file_name(&path),
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
                            if self.snapshot.navigation.cwd.is_empty() {
                                "フォルダを選択".into()
                            } else {
                                file_name(&self.snapshot.navigation.cwd)
                            },
                            cx,
                            |s, _, _| {
                                if s.snapshot.navigation.cwd.is_empty() {
                                    s.pick_folder();
                                } else {
                                    s.browse(s.snapshot.navigation.cwd.clone());
                                }
                            },
                        )
                        .icon(IconName::FolderOpen)
                        .w_full(),
                    )
                    .when(
                        self.snapshot
                            .workspace
                            .review
                            .as_ref()
                            .is_some_and(|review| !review.branch.is_empty()),
                        |body| {
                            body.child(
                                div().pl_2().text_sm().text_color(rgb(0x999999)).child(
                                    self.snapshot
                                        .workspace
                                        .review
                                        .as_ref()
                                        .map(|review| review.branch.clone())
                                        .unwrap_or_default(),
                                ),
                            )
                        },
                    )
                    .child(
                        self.button(
                            "context-changes",
                            format!(
                                "変更  +{}  −{}",
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.additions),
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.deletions)
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
                            s.browse(s.snapshot.navigation.cwd.clone())
                        })
                        .icon(IconName::Folder)
                        .w_full()
                        .disabled(!enabled),
                    ),
            )
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
                    s.browse(s.snapshot.navigation.cwd.clone());
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
                    |s, _, cx| {
                        if let Some(chat) = &s.side_chat {
                            chat.update(cx, |chat, _| chat.new_chat(String::new()));
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
                                s.browse(s.snapshot.navigation.cwd.clone());
                            } else {
                                s.open_panel(panel, w, cx);
                            }
                        })
                        .icon(icon.clone())
                        .w_full()
                        .h_10()
                        .bg(rgb(0x232323))
                        .disabled(
                            panel != Panel::Browser
                                && (!self.snapshot.connected
                                    || self.snapshot.navigation.cwd.is_empty()),
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
                        .child(
                            div().flex_1().child(format!(
                                "変更  +{} −{}",
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.additions),
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.deletions)
                            )),
                        )
                        .child(self.icon_button(
                            "refresh-diff",
                            IconName::RotateCw,
                            "変更を更新",
                            cx,
                            |s, _, _| s.refresh_review(),
                        )),
                )
                .child(
                    div().flex_1().min_h_0().child(Self::diff(
                        &mut self.diffs,
                        "workspace-patch".into(),
                        self.snapshot
                            .workspace
                            .review
                            .as_ref()
                            .map_or("", |review| review.diff.as_str()),
                        cx,
                    )),
                )
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
        let active = self.image_gallery.is_none() && self.panel_open && self.tab == Tab::Chat;
        let composer_visible = self.image_gallery.is_none()
            && self.tab == Tab::Chat
            && (self.side_chat_mode
                || !self.panel_open
                || window.viewport_size().width >= px(1080.));
        if !composer_visible {
            self.cancel_recording();
        }
        if let Some(chat) = &self.side_chat
            && (!active || self.panel != Panel::SideChat)
        {
            chat.update(cx, |chat, _| chat.cancel_recording());
        }
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
        if self.image_gallery.is_some() {
            let gallery = self.image_gallery_view(window, cx);
            return h_flex()
                .size_full()
                .bg(rgb(0x181818))
                .text_color(rgb(0xececec))
                .text_size(px(14.))
                .child(gallery);
        }
        let wide = window.viewport_size().width >= px(1080.);
        let title = if self.tab == Tab::Settings {
            "設定"
        } else {
            self.thread()
                .and_then(|thread| thread.name.as_deref())
                .or_else(|| {
                    self.snapshot
                        .threads
                        .as_ref()
                        .and_then(|page| {
                            page.data
                                .iter()
                                .find(|thread| thread.id.as_deref() == Some(self.selected()))
                        })
                        .and_then(|thread| thread.name.as_deref())
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
        header = header.child(
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
            .font_weight(FontWeight::NORMAL)
            .when(self.sidebar && !self.side_chat_mode, |body| {
                body.child(self.sidebar(window, cx))
            })
            .child(main)
    }
}
