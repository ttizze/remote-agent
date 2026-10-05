use super::*;

impl Desktop {
    pub(super) fn image(
        &mut self,
        source: &str,
        encoded: bool,
        height: f32,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = self.image_key(source, encoded);
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
            let store = self.session.as_ref().map(|session| session.store.clone());
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
                        .dispatch(Intent::DownloadFile(op::DownloadFile {
                            source: path
                                .into_os_string()
                                .into_string()
                                .map_err(|_| "download source is not UTF-8")?,
                            destination: destination
                                .to_str()
                                .ok_or("download destination is not UTF-8")?
                                .into(),
                        }))
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(destination.to_string_lossy().into_owned())
                },
                move |result| Update::Image {
                    key: returned,
                    result,
                },
            );
        }
        let state = &self.images[&key];
        if let Some(path) = &state.path {
            let background = cx.theme().secondary;
            let id = SharedString::from(key);
            let image = img(path.clone())
                .id(id.clone())
                .w_full()
                .min_w_0()
                .h(px(height))
                .min_h(px(height))
                .max_h(px(height))
                .object_fit(ObjectFit::Contain)
                .debug_selector(|| "chat-image".into())
                .with_loading(move || image_skeleton(height, background).into_any_element())
                .with_fallback(|| {
                    div()
                        .text_sm()
                        .text_color(rgb(0xaaaaaa))
                        .child("画像を表示できません")
                        .into_any_element()
                });
            if clickable {
                let source = state.source.clone();
                div()
                    .id(id)
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
        } else if let Some(error) = &state.error {
            div()
                .text_sm()
                .text_color(rgb(0xaaaaaa))
                .child(error.clone())
                .into_any_element()
        } else {
            image_skeleton(height, cx.theme().secondary).into_any_element()
        }
    }

    pub(super) fn markdown(
        &mut self,
        id: String,
        source: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self
            .markdown_cache
            .get(&id)
            .is_none_or(|cached| cached.source != source)
        {
            let (rendered, images) =
                agent_core::presentation::markdown::markdown_without_images(source);
            let source: SharedString = source.to_owned().into();
            let rendered = if images.is_empty() {
                source.clone()
            } else {
                rendered.into_owned().into()
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
    pub(super) fn open_conversation_link(&mut self, source: &str, cx: &mut Context<Self>) {
        if source.starts_with("https://") || source.starts_with("http://") {
            cx.open_url(source);
            return;
        }
        let path = match conversation_file_path(source, &self.snapshot.navigation.cwd) {
            Ok(path) => path,
            Err(error) => {
                self.set_error(error);
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
        if let Some(parent) = path.parent() {
            self.browse(parent.to_string_lossy().into_owned());
        }
        self.edit(path.to_string_lossy().into_owned(), false);
        cx.notify();
    }

    pub(super) fn open_image_gallery(
        &mut self,
        source: Arc<String>,
        encoded: bool,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4();
        self.image_gallery = Some(ImageGallery {
            zoom: 1.,
            id,
            entries: Vec::new(),
            initial: (source, encoded),
            selected: None,
            list: ListState::new(0, ListAlignment::Top, px(160.)),
            loading: !self.selected().is_none(),
            saving: false,
            saved: false,
            error: String::new(),
        });
        if !self.selected().is_none() {
            self.perform(
                Intent::LoadSessionImages(op::LoadSessionImages {
                    thread_id: self.selected().expect("selected conversation").clone(),
                }),
                OperationCompletion::Gallery(id),
            );
        }
        cx.notify();
    }

    pub(super) fn gallery_thumbnail(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
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
                    gallery.zoom = 1.;
                    gallery.saved = false;
                    gallery.error.clear();
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    pub(super) fn image_gallery_view(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gallery = self.image_gallery.as_ref().unwrap();
        let (source, encoded) = gallery.current_image().clone();
        let list_state = gallery.list.clone();
        let zoom = gallery.zoom;
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
            (f32::from(window.viewport_size().height) - 200.).max(120.) * zoom,
            false,
            cx,
        );
        let key = self.image_key(&source, encoded);
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
                        Self::icon_button(
                            "gallery-save",
                            if saved {
                                Icon::new(IconName::Check)
                            } else {
                                Icon::default().path("bex/download.svg")
                            },
                            if saved { "保存済み" } else { "保存" },
                            cx,
                            |s, _, cx| s.save_gallery_image(cx),
                        )
                        .disabled(!ready || saving || saved),
                    )
                    .child(
                        Self::icon_button(
                            "gallery-close",
                            IconName::Close,
                            "閉じる",
                            cx,
                            |s, _, _| s.image_gallery = None,
                        )
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
                        div()
                            .id("gallery-viewport")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_scroll()
                            .child(
                                div()
                                    .w(px((f32::from(window.viewport_size().width) - 152.)
                                        .max(120.)
                                        * zoom))
                                    .child(image),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .justify_center()
                    .gap_3()
                    .child(
                        self.button("gallery-zoom-out", "−", cx, |s, _, _| {
                            if let Some(gallery) = &mut s.image_gallery {
                                gallery.zoom = (gallery.zoom - 0.25).max(0.25);
                            }
                        })
                        .accessibility_label("縮小")
                        .disabled(zoom <= 0.25),
                    )
                    .child(
                        self.button(
                            "gallery-zoom-reset",
                            format!("{:.0}%", zoom * 100.),
                            cx,
                            |s, _, _| {
                                if let Some(gallery) = &mut s.image_gallery {
                                    gallery.zoom = 1.;
                                }
                            },
                        )
                        .accessibility_label("倍率をリセット"),
                    )
                    .child(
                        self.button("gallery-zoom-in", "+", cx, |s, _, _| {
                            if let Some(gallery) = &mut s.image_gallery {
                                gallery.zoom = (gallery.zoom + 0.25).min(4.);
                            }
                        })
                        .accessibility_label("拡大")
                        .disabled(zoom >= 4.),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn save_gallery_image(&mut self, cx: &mut Context<Self>) {
        let Some((source, encoded)) = self
            .image_gallery
            .as_ref()
            .filter(|gallery| !gallery.saving)
            .map(|gallery| gallery.current_image())
        else {
            return;
        };
        let key = self.image_key(source, *encoded);
        let Some(ImageSource::Resource(resource)) =
            self.images.get(&key).and_then(|image| image.path.clone())
        else {
            return;
        };
        let gallery = self.image_gallery.as_mut().expect("gallery checked above");
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
                        Err(error) => {
                            tracing::error!(target: "bex", operation = "gallery.save", message = %error);
                            gallery.error = error;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod file_panel_tests {
    use super::*;
    use core::prelude::v1::test;

    #[gpui::test]
    fn side_chat_links_and_changes_are_visible_and_return_to_the_same_draft(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
                preferences: Arc::default(),
            });
        });
        let mut desktop = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid-fixture-ticket".into(),
                        }),
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                )
            });
            desktop = Some(view.clone());
            Root::new(view, window, cx)
        });
        let desktop = desktop.unwrap();
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).navigation).cwd =
                    "/fixture".into();
                view.composer.update(cx, |input, cx| {
                    input.set_value("Keep this side draft", window, cx)
                });
                view.open_conversation_link("readme.txt:1", cx);
                assert!(view.panel_open && view.panel == Panel::Files);
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("conversation-files").is_some(),
            "file links must show the file panel"
        );
        let back = window
            .debug_bounds("back-side-chat")
            .expect("return to side chat");
        window.simulate_click(back.center(), Modifiers::default());
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert_eq!(
                    view.composer.read(cx).value().as_ref(),
                    "Keep this side draft"
                );
                assert!(!view.panel_open);
                view.open_review(None, cx);
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("diff-file-navigation").is_some(),
            "change summaries must show the diff panel"
        );
        let back = window.debug_bounds("back-side-chat").unwrap();
        window.simulate_click(back.center(), Modifiers::default());
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert_eq!(
                    view.composer.read(cx).value().as_ref(),
                    "Keep this side draft"
                );
                assert!(!view.panel_open);
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use agent_core::presentation::markdown::{
        MarkdownBlock, markdown_blocks, markdown_without_images,
    };
    use gpui_kit as gpui;
    use gpui_kit::{AppContext, TestAppContext, component::text::TextViewState};

    #[gpui::test]
    fn markdown_tables_keep_every_shared_cell_in_desktop_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/agent-core/tests/fixtures/markdown/table.md"
        ));
        let (rendered, images) = markdown_without_images(source);
        assert!(images.is_empty());
        let state = cx.new(|cx| TextViewState::markdown(&rendered, cx));
        cx.run_until_parked();
        state.update(cx, |state, cx| state.select_all(cx));
        let selected = state.read_with(cx, |state, _| state.selected_text());
        let MarkdownBlock::Table { rows, .. } = &markdown_blocks(source.into())[0] else {
            panic!("expected shared table")
        };
        let expected = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| {
                        cell.runs
                            .iter()
                            .map(|run| run.text.as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(selected.trim_end(), expected);
        state.update(cx, |state, cx| state.clear_selection(cx));
        assert_eq!(state.read_with(cx, |state, _| state.selected_text()), "");
    }
}
