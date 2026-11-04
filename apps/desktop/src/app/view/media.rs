use super::*;
use gpui_kit::component::menu::ContextMenuExt;

impl Desktop {
    pub(super) fn image(
        &mut self,
        source: &str,
        encoded: bool,
        height: f32,
        clickable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let key = self.image_key(source, encoded);
        if !self.media.images.contains_key(&key) {
            let source = Arc::new(source.to_owned());
            self.media.images.insert(
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
                .media
                .image_dir
                .path()
                .join(format!("image-{}", self.media.images.len()));
            let returned = key.clone();
            self.effect(
                async move {
                    if encoded || source.starts_with("data:image/") {
                        let data = if encoded {
                            source.as_str()
                        } else {
                            let (header, data) = source
                                .split_once(',')
                                .ok_or(language.text("画像データが不正です"))?;
                            if !header.ends_with(";base64") {
                                return Err(language.text("未対応の画像データです").into());
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
                    if !remote && path.is_absolute() {
                        return Ok(path.to_string_lossy().into_owned());
                    }
                    let store = store.ok_or(language.text("ホストに接続していません"))?;
                    if !remote {
                        let result = store
                            .dispatch(Intent::ResolveFile(op::ResolveFile {
                                path: path.to_string_lossy().into_owned(),
                            }))
                            .await
                            .map_err(|error| error.to_string())?;
                        let Outcome::ResolvedFile { entry } = result else {
                            unreachable!("resolved file outcome")
                        };
                        return Ok(entry.path);
                    }
                    files::download_to(
                        &store,
                        path.into_os_string()
                            .into_string()
                            .map_err(|_| "download source is not UTF-8")?,
                        destination.clone(),
                    )
                    .await?;
                    Ok(destination.to_string_lossy().into_owned())
                },
                move |result| Update::Image {
                    key: returned,
                    result,
                },
            );
        }
        let state = &self.media.images[&key];
        if let Some(path) = &state.path {
            let background = cx.theme().secondary;
            let id = SharedString::from(key.clone());
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
                .with_fallback(move || {
                    div()
                        .text_sm()
                        .text_color(rgb(appearance::MUTED))
                        .child(language.text("画像を表示できません"))
                        .into_any_element()
                });
            let source = state.source.clone();
            let menu_source = source.clone();
            let owner = cx.entity().downgrade();
            let saving = self
                .media
                .gallery
                .as_ref()
                .is_some_and(|gallery| gallery.saving);
            let image_menu = div()
                .id(id)
                .w_full()
                .h(px(height))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .when(clickable, |element| {
                    element
                        .cursor_pointer()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.open_image_gallery(source.clone(), encoded, cx)
                        }))
                })
                .child(image)
                .context_menu(move |menu, _, _| {
                    let owner = owner.clone();
                    let source = menu_source.clone();
                    menu.item(
                        PopupMenuItem::new(language.text("画像を名前を付けて保存…"))
                            .icon(Icon::default().path("icons/download.svg"))
                            .disabled(saving)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, cx| {
                                    view.save_image(&source, encoded, cx);
                                });
                            }),
                    )
                });
            div()
                .id(SharedString::from(format!("image-menu-boundary-{key}")))
                .w_full()
                .h(px(height))
                .flex_shrink_0()
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(image_menu)
                .into_any_element()
        } else if let Some(error) = &state.error {
            div()
                .text_sm()
                .text_color(rgb(appearance::MUTED))
                .child(error.clone())
                .into_any_element()
        } else {
            image_skeleton(height, cx.theme().secondary).into_any_element()
        }
    }

    pub(super) fn click_conversation_link(
        &mut self,
        source: &str,
        control: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source.starts_with("https://") || source.starts_with("http://") {
            if control {
                self.open_browser(self.panel_scope(), source, cx);
            } else {
                cx.open_url(source);
            }
            return;
        }
        if let Some((file, lookup_path)) =
            agent_core::presentation::markdown::desktop_file_reference(
                source,
                &self.snapshot.navigation.cwd,
                self.snapshot.host_home_directory.as_deref(),
            )
        {
            self.activate_reference(
                &file,
                lookup_path.as_deref(),
                false,
                window.mouse_position(),
                window,
                cx,
            );
        }
    }

    pub(super) fn open_image_gallery(
        &mut self,
        source: Arc<String>,
        encoded: bool,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4();
        self.media.gallery = Some(ImageGallery {
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
        let language = crate::localization::language(cx);
        let Some(gallery) = &self.media.gallery else {
            return div().into_any_element();
        };
        let Some((source, encoded)) = gallery.entries.get(index).cloned() else {
            return div().into_any_element();
        };
        let selected = gallery.selected == Some(index);
        let image = self.image(&source, encoded, 64., false, cx);
        Button::new(format!("gallery-thumbnail-{index}"))
            .accessibility_label(agent_core::localization::localized_ui_format(
                language,
                "生成画像 {0}".into(),
                vec![(index + 1).to_string()],
            ))
            .ghost()
            .child(image)
            .w(px(80.))
            .h(px(80.))
            .p_2()
            .selected(selected)
            .on_click(cx.listener(move |s, _, _, cx| {
                if let Some(gallery) = s.media.gallery.as_mut().filter(|gallery| !gallery.saving) {
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
        let language = crate::localization::language(cx);
        let gallery = self.media.gallery.as_ref().unwrap();
        let (source, encoded) = gallery.current_image().clone();
        let list_state = gallery.list.clone();
        let zoom = gallery.zoom;
        let saving = gallery.saving;
        let saved = gallery.saved;
        let loading = gallery.loading;
        let label = gallery
            .selected
            .map(|index| format!("{} / {}", index + 1, gallery.entries.len()))
            .unwrap_or_else(|| language.text("画像").into());
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
            .media
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
                    .when(loading, |row| {
                        row.child(language.text("画像一覧を読み込み中…"))
                    })
                    .child(
                        icon_button(
                            "gallery-save",
                            if saved {
                                Icon::new(IconName::Check)
                            } else {
                                Icon::default().path("icons/download.svg")
                            },
                            if saved {
                                language.text("保存済み")
                            } else {
                                language.text("保存")
                            },
                            cx,
                            |s, _, cx| s.save_gallery_image(cx),
                        )
                        .disabled(!ready || saving || saved),
                    )
                    .child(
                        icon_button(
                            "gallery-close",
                            IconName::Close,
                            language.text("閉じる"),
                            cx,
                            |s, _, _| s.media.gallery = None,
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
                        button("gallery-zoom-out", "−", cx, |s, _, _| {
                            if let Some(gallery) = &mut s.media.gallery {
                                gallery.zoom = (gallery.zoom - 0.25).max(0.25);
                            }
                        })
                        .accessibility_label(language.text("縮小"))
                        .disabled(zoom <= 0.25),
                    )
                    .child(
                        button(
                            "gallery-zoom-reset",
                            format!("{:.0}%", zoom * 100.),
                            cx,
                            |s, _, _| {
                                if let Some(gallery) = &mut s.media.gallery {
                                    gallery.zoom = 1.;
                                }
                            },
                        )
                        .accessibility_label(language.text("倍率をリセット")),
                    )
                    .child(
                        button("gallery-zoom-in", "+", cx, |s, _, _| {
                            if let Some(gallery) = &mut s.media.gallery {
                                gallery.zoom = (gallery.zoom + 0.25).min(4.);
                            }
                        })
                        .accessibility_label(language.text("拡大"))
                        .disabled(zoom >= 4.),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn save_gallery_image(&mut self, cx: &mut Context<Self>) {
        let Some((source, encoded)) = self
            .media
            .gallery
            .as_ref()
            .filter(|gallery| !gallery.saving)
            .map(|gallery| gallery.current_image().clone())
        else {
            return;
        };
        self.save_image(&source, encoded, cx);
    }

    fn save_image(&mut self, source: &str, encoded: bool, cx: &mut Context<Self>) {
        let language = crate::localization::language(cx);
        let key = self.image_key(source, encoded);
        let Some(ImageSource::Resource(resource)) = self
            .media
            .images
            .get(&key)
            .and_then(|image| image.path.clone())
        else {
            return;
        };
        let id = if let Some(gallery) = self.media.gallery.as_mut().filter(|gallery| {
            let current = gallery.current_image();
            current.0.as_str() == source && current.1 == encoded
        }) {
            if gallery.saving {
                return;
            }
            gallery.saving = true;
            gallery.error.clear();
            Some(gallery.id)
        } else {
            None
        };
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
                            return Err(agent_core::localization::localized_ui_format(language, "画像の取得に失敗しました: {0}".into(), vec![response.status().to_string()]));
                        }
                        let mut bytes = Vec::new();
                        response
                            .body_mut()
                            .read_to_end(&mut bytes)
                            .await
                            .map_err(|error| error.to_string())?;
                        Original::Bytes(bytes)
                    }
                    _ => return Err(language.text("保存できない画像です").into()),
                };
                cx.background_executor()
                    .spawn(async move {
                        let format = match &original {
                            Original::File(path) => image::ImageReader::open(path)
                                .and_then(|reader| reader.with_guessed_format())
                                .map_err(|error| error.to_string())?
                                .format()
                                .ok_or(language.text("画像形式が不明です"))?,
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
                if let Some(gallery) = s.media.gallery.as_mut().filter(|gallery| Some(gallery.id) == id) {
                    gallery.saving = false;
                    match result {
                        Ok(saved) => gallery.saved = saved,
                        Err(error) => {
                            tracing::error!(target: "agent", operation = "gallery.save", message = %error);
                            gallery.error = error;
                        }
                    }
                } else if let Err(error) = result {
                    tracing::error!(target: "agent", operation = "image.save", message = %error);
                    s.set_error(error);
                }
                cx.notify();
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

    fn resolved(path: &str, directory: bool) -> Outcome {
        Outcome::FileReferenceResolved {
            path: path.into(),
            directory,
        }
    }
    fn reference_request(view: &Desktop, source: &str, epoch: u64) -> files::ReferenceRequest {
        let (file, lookup_path) = agent_core::presentation::markdown::desktop_file_reference(
            source,
            &view.snapshot.navigation.cwd,
            view.snapshot.host_home_directory.as_deref(),
        )
        .unwrap();
        files::ReferenceRequest {
            epoch,
            target: view.reference_target(&file, lookup_path.as_deref()),
            action: files::ReferenceAction::Open(view.file_open_generation),
        }
    }
    fn start_reference(
        view: &mut Desktop,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> files::ReferenceRequest {
        Arc::make_mut(&mut view.snapshot).connected = true;
        let request = reference_request(view, source, view.snapshot.epoch);
        view.open_reference(request.target, window, cx);
        reference_request(view, source, view.snapshot.epoch)
    }

    #[gpui::test]
    fn text_panels_and_native_files_keep_their_draft_and_resolution_boundaries(
        cx: &mut TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let cwd = directory.path().to_string_lossy().into_owned();
        let source = path.to_string_lossy().into_owned();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let mut desktop = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
                        remote: None,
                        cwd: cwd.clone(),
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
                let snapshot = Arc::make_mut(&mut view.snapshot);
                Arc::make_mut(&mut snapshot.navigation).cwd = cwd.clone();
                view.composer.input.update(cx, |input, cx| {
                    input.set_value("Keep this draft", window, cx)
                });
                view.panels.open(view.panel_scope(), Panel::Files, None);
                view.sync_file_browser(true, window, cx);
                view.file_browser.as_mut().unwrap().model.tree =
                    Arc::new(agent_core::presentation::files::FileTreeView {
                        rows: vec![agent_core::presentation::files::FileTreeRow {
                            path: "large.txt".into(),
                            label: "large.txt".into(),
                            kind: agent_protocol::file_tree::EntryKind::File,
                            depth: 0,
                            expanded: false,
                            selected: false,
                            ignored: false,
                            loading: false,
                        }],
                        ready: true,
                        ..Default::default()
                    });
                view.panels.set_open(view.panel_scope(), true);
                cx.notify();
            });
        });
        window.run_until_parked();
        let entry = window
            .debug_bounds("workspace-file-0")
            .expect("image file entry");
        window.simulate_click(entry.center(), Modifiers::default());
        window.run_until_parked();
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                let key = view.file_browser.as_ref().unwrap().model.key.clone();
                view.open_file_from_tree(
                    &key,
                    &agent_core::presentation::files::FileTreeRequest {
                        id: 1,
                        path: "large.txt".into(),
                    },
                    cx,
                );
            })
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("download-file").is_some(),
            "unsupported files keep an explicit save action"
        );
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                assert!(
                    view.media.gallery.is_none(),
                    "text references must not open a media preview"
                );
                assert_eq!(view.panels.get(view.panel_scope()).kind(), Panel::File);
                assert_eq!(
                    view.panels
                        .get(view.panel_scope())
                        .selected()
                        .unwrap()
                        .path
                        .as_deref(),
                    Some(source.as_str())
                );
                assert_eq!(
                    view.composer.input.read(cx).value().as_ref(),
                    "Keep this draft"
                );
                let key = view.file_browser.as_ref().unwrap().model.key.clone();
                let duplicate = agent_core::presentation::files::FileTreeRequest {
                    id: 1,
                    path: "another.txt".into(),
                };
                view.open_file_from_tree(&key, &duplicate, cx);
                let mut closed_owner = key;
                closed_owner.owner.push_str("-previous-mount");
                view.open_file_from_tree(
                    &closed_owner,
                    &agent_core::presentation::files::FileTreeRequest { id: 2, ..duplicate },
                    cx,
                );
                assert_eq!(
                    view.panels
                        .get(view.panel_scope())
                        .selected()
                        .unwrap()
                        .path
                        .as_deref(),
                    Some(source.as_str()),
                    "duplicate projections and closed tree owners cannot reopen a file"
                );
                for name in [
                    "large.txt",
                    "installer.EXE",
                    "readme.txt",
                    "source.rs",
                    "index.html",
                    "notebook.ipynb",
                ] {
                    let target = directory.path().join(name).to_string_lossy().into_owned();
                    view.panels.set_open(view.panel_scope(), false);
                    let request = start_reference(view, &target, window, cx);
                    // The fixture is offline; deliver the Host's resolution
                    // through the same production completion used by links.
                    view.operation_completed(
                        OperationCompletion::FileReference(Box::new(request)),
                        Ok(resolved(&target, false)),
                        window,
                        cx,
                    );
                    let panel = view.panels.get(view.panel_scope());
                    assert!(panel.open);
                    assert_eq!(panel.kind(), Panel::File, "{name}");
                    assert_eq!(
                        panel.selected().unwrap().path.as_deref(),
                        Some(target.as_str())
                    );
                    assert!(view.media.gallery.is_none());
                    assert_eq!(
                        view.busy, 0,
                        "{name} must not start an implicit download or OS launch"
                    );
                    assert_eq!(
                        view.composer.input.read(cx).value().as_ref(),
                        "Keep this draft"
                    );
                }
                let before = view.panels.get(view.panel_scope());
                let stale = start_reference(view, "older.rs", window, cx);
                let latest = start_reference(view, "newer.rs", window, cx);
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(stale)),
                    Ok(resolved("/stale.rs", false)),
                    window,
                    cx,
                );
                assert_eq!(
                    view.panels.get(view.panel_scope()),
                    before,
                    "a slower lookup cannot replace a newer request"
                );
                let winner = format!("{cwd}/newer.rs");
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(latest)),
                    Ok(resolved(&winner, false)),
                    window,
                    cx,
                );
                assert_eq!(
                    view.panels
                        .get(view.panel_scope())
                        .selected()
                        .unwrap()
                        .path
                        .as_deref(),
                    Some(winner.as_str())
                );
                let stale = start_reference(view, "older.rs", window, cx);
                view.open_file(source.clone(), None, false, cx);
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(stale)),
                    Ok(resolved("/stale.rs", false)),
                    window,
                    cx,
                );
                assert_eq!(
                    view.panels
                        .get(view.panel_scope())
                        .selected()
                        .unwrap()
                        .path
                        .as_deref(),
                    Some(source.as_str()),
                    "tree and explicit editor opens supersede pending lookups"
                );
                let mut copy = reference_request(view, "missing.rs:12:3", view.snapshot.epoch);
                copy.action = files::ReferenceAction::Menu(
                    agent_core::presentation::files::FileAction::CopyRelativePath,
                );
                let found = format!("{cwd}/nested/missing.rs");
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(copy)),
                    Ok(resolved(&found, false)),
                    window,
                    cx,
                );
                assert_eq!(
                    cx.read_from_clipboard()
                        .and_then(|item| item.text())
                        .as_deref(),
                    Some("nested/missing.rs:12:3")
                );
                // The existing gallery remains available to its explicit image
                // entry points; only file-reference routing is being replaced.
                let image = directory
                    .path()
                    .join("explicit.png")
                    .to_string_lossy()
                    .into_owned();
                view.open_image_gallery(Arc::new(image.clone()), false, cx);
                assert_eq!(
                    view.media
                        .gallery
                        .as_ref()
                        .unwrap()
                        .current_image()
                        .0
                        .as_str(),
                    image
                );
                view.media.gallery = None;
                let request = start_reference(view, "~/Documents", window, cx);
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(request)),
                    Ok(resolved(&cwd, true)),
                    window,
                    cx,
                );
                assert_eq!(view.panels.get(view.panel_scope()).kind(), Panel::Files);
                let before = view.panels.get(view.panel_scope());
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(reference_request(
                        view,
                        &source,
                        view.snapshot.epoch + 1,
                    ))),
                    Ok(resolved(&source, false)),
                    window,
                    cx,
                );
                assert!(
                    view.media.gallery.is_none(),
                    "a stale Host response must not open a file"
                );
                assert_eq!(view.panels.get(view.panel_scope()), before);
            });
        });
        for (index, name) in [
            "picture.PNG",
            "clip.MP4",
            "song.mp3",
            "document.pdf",
            "notes.docx",
            "archive.zip",
            "font.ttf",
        ]
        .iter()
        .enumerate()
        {
            let target = directory.path().join(name);
            window.update(|window, cx| {
                desktop.update(cx, |view, cx| {
                    let before = view.panels.get(view.panel_scope());
                    let request = start_reference(view, target.to_str().unwrap(), window, cx);
                    view.operation_completed(
                        OperationCompletion::FileReference(Box::new(request)),
                        Ok(resolved(target.to_str().unwrap(), false)),
                        window,
                        cx,
                    );
                    assert_eq!(
                        view.panels.get(view.panel_scope()),
                        before,
                        "native files must not open a text panel"
                    );
                    assert_eq!(
                        view.composer.input.read(cx).value().as_ref(),
                        "Keep this draft"
                    );
                    assert_eq!(view.busy, 0, "local files do not require downloading");
                })
            });
            assert_eq!(
                window.opened_url(),
                Some(url::Url::from_file_path(&target).unwrap().into())
            );
            let name = format!("tree {name}");
            let target = directory.path().join(&name);
            window.update(|_, cx| {
                desktop.update(cx, |view, cx| {
                    let key = view.file_browser.as_ref().unwrap().model.key.clone();
                    view.open_file_from_tree(
                        &key,
                        &agent_core::presentation::files::FileTreeRequest {
                            id: index as u64 + 10,
                            path: name,
                        },
                        cx,
                    );
                })
            });
            assert_eq!(
                window.opened_url(),
                Some(url::Url::from_file_path(target).unwrap().into())
            );
        }
    }

    #[gpui::test]
    fn side_chat_links_and_changes_are_visible_and_return_to_the_same_draft(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
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
                view.composer.input.update(cx, |input, cx| {
                    input.set_value("Keep this side draft", window, cx)
                });
                let request = start_reference(view, "readme.txt:1", window, cx);
                view.operation_completed(
                    OperationCompletion::FileReference(Box::new(request)),
                    Ok(resolved("/fixture/readme.txt", false)),
                    window,
                    cx,
                );
                assert!(
                    view.panels.get(view.panel_scope()).open
                        && view.panels.get(view.panel_scope()).kind() == Panel::File
                );
                #[cfg(target_os = "windows")]
                {
                    view.click_conversation_link("https://example.com/first", true, window, cx);
                    assert!(
                        view.panels.get(view.panel_scope()).open
                            && view.panels.get(view.panel_scope()).kind() == Panel::Browser
                    );
                    assert_eq!(
                        view.panels.get(view.panel_scope()).active.and_then(|id| {
                            match view.surface_resources.get(&id) {
                                Some(SurfaceResource::Pending { url }) => url.as_deref(),
                                _ => None,
                            }
                        }),
                        Some("https://example.com/first")
                    );
                    view.click_conversation_link(
                        "http://localhost:3000/next?q=1#detail",
                        true,
                        window,
                        cx,
                    );
                    assert_eq!(
                        view.panels.get(view.panel_scope()).active.and_then(|id| {
                            match view.surface_resources.get(&id) {
                                Some(SurfaceResource::Pending { url }) => url.as_deref(),
                                _ => None,
                            }
                        }),
                        Some("http://localhost:3000/next?q=1#detail")
                    );
                    let request = start_reference(view, "readme.txt:1", window, cx);
                    view.operation_completed(
                        OperationCompletion::FileReference(Box::new(request)),
                        Ok(resolved("/fixture/readme.txt", false)),
                        window,
                        cx,
                    );
                    assert!(view.panels.get(view.panel_scope()).kind() == Panel::File);
                    assert_eq!(
                        view.composer.input.read(cx).value().as_ref(),
                        "Keep this side draft"
                    );
                }
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("conversation-file").is_some(),
            "file links must show the file panel"
        );
        let back = window
            .debug_bounds("back-side-chat")
            .expect("return to side chat");
        window.simulate_click(back.center(), Modifiers::default());
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert_eq!(
                    view.composer.input.read(cx).value().as_ref(),
                    "Keep this side draft"
                );
                assert!(!view.panels.get(view.panel_scope()).open);
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
                    view.composer.input.read(cx).value().as_ref(),
                    "Keep this side draft"
                );
                assert!(!view.panels.get(view.panel_scope()).open);
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
        cx.update(crate::appearance::init);
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
