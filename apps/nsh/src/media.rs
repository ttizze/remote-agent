//! Host paths use core's authenticated transfer; native views receive local copies.
use crate::l10n::{localized, tr, tr_format};
use crate::{
    Connection,
    client::{Client, LocalFile},
    native,
    shared::Shared,
};
use agent_core::presentation::markdown::{MarkdownFileSurface, markdown_file_target};
use nsh_core::{ImageSource, prelude::*};
use std::{rc::Rc, sync::Arc};

#[derive(Clone, Copy)]
pub(crate) struct Preview {
    state: Signal<PreviewState>,
    /// The generated images of the conversation whose image the desktop
    /// viewer shows, beside it.
    #[cfg(desktop)]
    gallery: Signal<Option<Gallery>>,
    owner: Owner,
}

#[derive(Clone, PartialEq)]
enum PreviewState {
    Closed,
    Loading(uuid::Uuid),
    Ready(Shared<LocalFile>),
}

#[cfg(desktop)]
#[derive(Clone, PartialEq)]
struct Gallery {
    request: uuid::Uuid,
    host: String,
    cwd: String,
    /// The source of the image the viewer shows.
    shown: String,
    /// The conversation's generated images, `None` while they load.
    sources: Option<Result<Vec<String>, String>>,
}

impl Preview {
    pub fn new() -> Self {
        Self {
            state: signal(PreviewState::Closed),
            #[cfg(desktop)]
            gallery: signal(None),
            owner: Owner::current(),
        }
    }

    fn show(self, file: Shared<LocalFile>) {
        self.state.set(PreviewState::Ready(file));
    }

    pub fn begin(self) -> uuid::Uuid {
        let request = uuid::Uuid::new_v4();
        self.state.set(PreviewState::Loading(request));
        #[cfg(desktop)]
        self.gallery.set(None);
        request
    }

    pub fn is_pending(self, request: uuid::Uuid) -> bool {
        self.state.get_untracked() == PreviewState::Loading(request)
    }

    pub fn load(self, transfer: impl Future<Output = Result<Shared<LocalFile>, String>> + 'static) {
        let request = self.begin();
        // Preview ownership is independent of recycled conversation rows.
        self.owner.run(move || {
            spawn(async move {
                let result = transfer.await;
                if !self.is_pending(request) {
                    return;
                }
                match result {
                    Ok(file) => self.show(file),
                    Err(reason) => {
                        self.state.set(PreviewState::Closed);
                        native::message(tr("ファイル"), reason);
                    }
                }
            })
        });
    }

    pub fn close(self) {
        self.state.set(PreviewState::Closed);
        #[cfg(desktop)]
        self.gallery.set(None);
    }

    /// The image the desktop's own viewer shows: an image file the preview
    /// holds, which Quick Look shows elsewhere.
    #[cfg(desktop)]
    fn shown_image(self) -> Option<Shared<LocalFile>> {
        self.state.with(|state| match state {
            PreviewState::Ready(file) if native::image_file(&file.path) => Some(file.clone()),
            _ => None,
        })
    }

    /// Shows a conversation's image, `file` loaded from `source`, beside the
    /// conversation's generated images, as the desktop client's gallery did.
    #[cfg(desktop)]
    fn open_image(self, connection: Connection, source: String, file: Shared<LocalFile>) {
        use agent_core::{
            presentation::media::gallery_sources,
            state::{Intent, operations as op},
            store::Outcome,
        };
        self.show(file);
        let host = connection.host.get_untracked();
        let thread = connection
            .context
            .with_untracked(|c| c.navigation().thread_id.clone());
        let request = uuid::Uuid::new_v4();
        self.gallery.set(Some(Gallery {
            request,
            host: host.clone(),
            cwd: connection
                .composer
                .with_untracked(|c| c.controls.navigation.cwd.clone()),
            shown: source,
            sources: thread.is_none().then(|| Ok(Vec::new())),
        }));
        let Some(thread_id) = thread else {
            return;
        };
        let response = connection.client.with(|client| {
            client.request(
                &host,
                connection.pane,
                Intent::LoadSessionImages(op::LoadSessionImages { thread_id }),
            )
        });
        self.owner.run(move || {
            spawn(async move {
                let result = match response.recv().await {
                    Ok(Ok(Outcome::SessionImages { images })) => Ok(gallery_sources(images)),
                    Ok(Ok(_)) => Err(tr("画像一覧を読み込めませんでした。").to_owned()),
                    Ok(Err(reason)) => Err(reason.to_string()),
                    Err(reason) => Err(reason.to_string()),
                };
                self.gallery.update(|gallery| {
                    if let Some(gallery) = gallery.as_mut().filter(|g| g.request == request) {
                        gallery.sources = Some(result);
                    }
                });
            })
        });
    }

    /// Shows the gallery's image at `source`, `file` loaded from it.
    #[cfg(desktop)]
    fn select(self, source: String, file: Shared<LocalFile>) {
        self.show(file);
        self.gallery.update(|gallery| {
            if let Some(gallery) = gallery {
                gallery.shown = source;
            }
        });
    }
}

pub(crate) fn preview() -> impl Element {
    let preview = use_context::<Preview>().unwrap();
    let root = view();
    #[cfg(desktop)]
    let root = root.child(when(
        move || preview.shown_image().is_some(),
        move || viewer(preview),
    ));
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let root = root.child(opened_file(preview));
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let root = root.child(quick_look(preview));
    root.child(when(
        move || matches!(preview.state.get(), PreviewState::Loading(_)),
        || text(tr("ファイルを読み込み中…")),
    ))
}

/// Quick Look shows what the desktop's viewer does not: every file on the
/// iPhone, a file other than an image on the Mac.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn quick_look(preview: Preview) -> impl Element {
    native("bex.preview")
        .h(px(0.0))
        .prop("path", move || {
            serde_json::json!(preview.state.with(|file| match file {
                PreviewState::Ready(file) if !cfg!(desktop) || !native::image_file(&file.path) =>
                    file.path.to_string_lossy().into_owned(),
                _ => String::new(),
            }))
        })
        .on_native_event(move |event: &NativeEvent| {
            if &*event.name == "closed"
                && preview.state.with_untracked(|file| {
                    matches!(file, PreviewState::Ready(file) if event.data.as_str() == file.path.to_str())
                })
            {
                preview.state.set(PreviewState::Closed);
            } else if &*event.name == "error" {
                preview.state.set(PreviewState::Closed);
                native::message(
                    tr("プレビュー"),
                    tr("このファイルをプレビューできませんでした。"),
                );
            }
        })
}

/// Where Quick Look is not, a file other than an image opens in the app the
/// system opens it with, while a bar keeps its local copy until it is
/// dismissed.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn opened_file(preview: Preview) -> View {
    use crate::ui::{self, colors};
    let file = computed(move || match preview.state.get() {
        PreviewState::Ready(file) if !native::image_file(&file.path) => Some(file),
        _ => None,
    });
    effect(move || {
        let Some(file) = file.get() else {
            return;
        };
        let opened = open_path(file.path.clone());
        spawn(async move {
            if let Err(reason) = opened.await {
                preview.state.set(PreviewState::Closed);
                native::message(tr("プレビュー"), reason);
            }
        });
    });
    view().child(when(
        move || file.with(Option::is_some),
        move || {
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(move || {
                        file.with(|file| {
                            file.as_ref()
                                .and_then(|file| file.path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        })
                    })
                    .flex_1()
                    .min_w_0()
                    .number_of_lines(1),
                )
                .child(ui::text_button(tr("閉じる"), move || preview.close()))
        },
    ))
}

/// The desktop's image viewer over the window, as the desktop client's: the
/// image, magnified by steps and scrolled, saved where the user chooses, and
/// beside it the conversation's generated images, each shown when pressed.
#[cfg(desktop)]
fn viewer(preview: Preview) -> impl Element {
    use crate::ui::{self, Enabled, Icon, colors};
    use agent_core::presentation::media::{ImageZoom, gallery_title};
    use nsh_core::element::{overlay, rgba};
    let file = computed(move || preview.shown_image());
    let zoom = signal(ImageZoom::FIT);
    let saved = signal(false);
    effect(move || {
        file.get();
        zoom.set(ImageZoom::FIT);
        saved.set(false);
    });
    let sources = computed(move || {
        preview.gallery.with(|gallery| {
            gallery
                .as_ref()
                .and_then(|gallery| gallery.sources.clone()?.ok())
                .unwrap_or_default()
        })
    });
    let title = move || {
        preview.gallery.with(|gallery| match gallery {
            Some(gallery) => {
                sources.with(|sources| localized(gallery_title(sources, &gallery.shown)))
            }
            None => file.with(|file| {
                file.as_ref()
                    .and_then(|file| file.path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }),
        })
    };
    let loading = move || {
        preview
            .gallery
            .with(|gallery| gallery.as_ref().is_some_and(|g| g.sources.is_none()))
    };
    let failure = move || {
        preview.gallery.with(|gallery| {
            gallery
                .as_ref()
                .and_then(|g| g.sources.clone()?.err())
                .map(localized)
        })
    };
    let thumbnails = move || !sources.with(Vec::is_empty);
    // The Mac's window buttons sit over the top of the window.
    let top = if cfg!(target_os = "macos") {
        44.0
    } else {
        16.0
    };
    // The image fits the window but for the viewer's bars and thumbnails.
    let fit = move || {
        let window = window();
        let side = if thumbnails() { 88.0 + 16.0 } else { 0.0 };
        (
            (window.width.0 - 32.0 - side).max(120.0),
            (window.height.0 - top - 124.0).max(120.0),
        )
    };
    let path = move || {
        file.with(|file| {
            file.as_ref()
                .map(|file| file.path.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    };
    overlay()
        .absolute()
        .inset_0()
        .p_4()
        .pt(px(top))
        .gap_3()
        .bg(rgba(0x000000e6))
        .text_color(colors::foreground())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .h(px(28.0))
                .child(text(title).flex_1().min_w_0().number_of_lines(1))
                .child(when(loading, || {
                    text(tr("画像一覧を読み込み中…"))
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                }))
                .child(
                    ui::icon_button(
                        Icon::DOWNLOAD,
                        move || {
                            SharedString::from(if saved.get() {
                                tr("保存済み")
                            } else {
                                tr("保存")
                            })
                        },
                        move || {
                            if let Some(file) = file.get_untracked() {
                                save_image(file, Some(saved));
                            }
                        },
                    )
                    .enabled(move || !saved.get()),
                )
                .child(ui::icon_button(Icon::CLOSE, tr("閉じる"), move || {
                    preview.close()
                })),
        )
        .child(when(
            move || failure().is_some(),
            move || {
                text(move || failure().unwrap_or_default())
                    .text_size(px(12.0))
                    .text_color(nsh_core::element::rgb(0xff8e86))
            },
        ))
        .child(
            view()
                .flex_1()
                .min_h_0()
                .flex_row()
                .gap_4()
                .child(when(thumbnails, move || {
                    gallery_thumbnails(preview, sources)
                }))
                .child(
                    scroll().flex_1().min_w_0().h_full().child(
                        scroll().horizontal().child(
                            view()
                                .child(
                                    nsh_core::element::image(path)
                                        .w(move || px(fit().0 * zoom.get().scale()))
                                        .h(move || px(fit().1 * zoom.get().scale()))
                                        .object_fit(ResizeMode::Contain)
                                        .label(tr("プレビュー"))
                                        .on_error(move |_: &str| {
                                            preview.close();
                                            native::message(
                                                tr("プレビュー"),
                                                tr("このファイルをプレビューできませんでした。"),
                                            );
                                        }),
                                )
                                // A pinch or a Ctrl (⌘) wheel over the image
                                // zooms it as the buttons below do.
                                .child(
                                    native::zoom_area(move |gesture| {
                                        zoom.update(|zoom| *zoom = zoom.gestured(gesture))
                                    })
                                    .absolute()
                                    .inset_0(),
                                ),
                        ),
                    ),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    ui::icon_button(Icon::ZOOM_OUT, tr("縮小"), move || {
                        if let Some(smaller) = zoom.get_untracked().zoomed_out() {
                            zoom.set(smaller);
                        }
                    })
                    .enabled(move || zoom.get().zoomed_out().is_some()),
                )
                .child(
                    nsh_base::button()
                        .accessibility_label(Some(SharedString::from(tr("倍率をリセット"))))
                        .h(px(24.0))
                        .px_2()
                        .rounded(px(ui::RADIUS))
                        .justify_center()
                        .hover(|s| s.bg(colors::GHOST_HOVER))
                        .child(text(move || zoom.get().label()).text_size(px(14.0)))
                        .on_press(move || zoom.set(ImageZoom::FIT)),
                )
                .child(
                    ui::icon_button(Icon::ZOOM_IN, tr("拡大"), move || {
                        if let Some(larger) = zoom.get_untracked().zoomed_in() {
                            zoom.set(larger);
                        }
                    })
                    .enabled(move || zoom.get().zoomed_in().is_some()),
                ),
        )
}

/// The gallery's column of thumbnails, the shown image's marked.
#[cfg(desktop)]
fn gallery_thumbnails(preview: Preview, sources: Memo<Vec<String>>) -> impl Element {
    use crate::ui::colors;
    let connection = use_context::<Connection>().unwrap();
    let place = move || {
        preview.gallery.with_untracked(|gallery| {
            gallery
                .as_ref()
                .map(|g| (g.host.clone(), g.cwd.clone()))
                .unwrap_or_default()
        })
    };
    scroll()
        .w(px(88.0))
        .h_full()
        .child(view().gap_2().child(each(
            move || sources.get().into_iter().enumerate().collect::<Vec<_>>(),
            |(_, source): &(usize, String)| source.clone(),
            move |entry, _| {
                let (index, source) = entry.get_untracked();
                let (host, cwd) = place();
                let shown = {
                    let source = source.clone();
                    move || {
                        preview
                            .gallery
                            .with(|g| g.as_ref().is_some_and(|g| g.shown == source))
                    }
                };
                let selected = source.clone();
                view()
                    .w(px(80.0))
                    .h(px(80.0))
                    .p_2()
                    .rounded(px(crate::ui::RADIUS))
                    .style_when(shown, |s| s.bg(colors::SELECTED))
                    .child(loaded_image(
                        connection.client,
                        computed(move || (host.clone(), cwd.clone(), source.clone())),
                        ImageSize::Box(64.0, 64.0),
                        tr_format("生成画像 {0}", [&(index + 1)]),
                        move |file| preview.select(selected.clone(), file),
                        false,
                    ))
            },
        )))
}

/// Saves the image's local copy where the user chooses, as the desktop
/// client's did; `saved` marks it once it is.
#[cfg(desktop)]
fn save_image(file: Shared<LocalFile>, saved: Option<Signal<bool>>) {
    let mut dialog = nsh_core::file_dialog::SaveDialog::new();
    if let Some(name) = file.path.file_name() {
        dialog = dialog.file_name(name.to_string_lossy());
    }
    let chosen = pick_save_path(dialog);
    spawn(async move {
        let target = match chosen.await {
            Ok(Some(target)) => target,
            Ok(None) => return,
            Err(reason) => return native::message(tr("画像を保存"), reason),
        };
        let source = file.path.clone();
        let copied = blocking(move || {
            if target != source {
                std::fs::copy(&source, &target)?;
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        match copied {
            Ok(Ok(())) => {
                if let Some(saved) = saved {
                    saved.set(true);
                }
            }
            _ => native::message(tr("画像を保存"), tr("画像を保存できませんでした。")),
        }
    });
}

pub(crate) async fn receive(
    transfer: async_channel::Receiver<Result<Arc<LocalFile>, agent_core::client::AgentError>>,
) -> Result<Shared<LocalFile>, String> {
    transfer
        .recv()
        .await
        .map_err(|reason| reason.to_string())?
        .map(Shared)
        .map_err(|reason| reason.to_string())
}

pub(crate) async fn load_image(
    client: Arc<Client>,
    host: String,
    cwd: String,
    source: String,
) -> Result<Shared<LocalFile>, String> {
    let transfer = if source.starts_with("data:image/") {
        let mime = source
            .strip_prefix("data:")
            .and_then(|value| value.split_once(';'))
            .map(|(mime, _)| mime)
            .ok_or(tr("画像を読み取れませんでした。"))?;
        let extension =
            native::image_extension(mime).ok_or(tr("この画像形式を利用できません。"))?;
        let data = ImageSource {
            uri: source.into(),
            ..Default::default()
        }
        .inline()
        .data
        .ok_or(tr("画像を読み取れませんでした。"))?;
        client.cache_image(data.to_vec(), extension)
    } else if source.starts_with("https://") || source.starts_with("http://") {
        let response = fetch(source).await?;
        if !response.ok() {
            return Err(tr_format(
                "画像を取得できませんでした (HTTP {0})。",
                [&response.status()],
            ));
        }
        let extension = response
            .header("content-type")
            .and_then(|mime| native::image_extension(mime.split(';').next().unwrap_or(mime)))
            .unwrap_or_else(|| "png".into());
        client.cache_image(response.body, extension)
    } else {
        let file = markdown_file_target(source, cwd, MarkdownFileSurface::Mobile)
            .ok_or(tr("画像の場所を読み取れませんでした。"))?;
        client.download(host, file.path)
    };
    receive(transfer).await
}

/// How large an image shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ImageSize {
    /// Fitted in a box of these points.
    Box(f32, f32),
    /// As T3 Code's answer images: a point per pixel, at most 30rem tall and
    /// as wide as there is room, its corners rounded.
    Natural,
    /// As T3 Code's lone sent image: 12rem tall and as wide as its picture,
    /// from 6rem, cropped where there is less room.
    Sent,
    /// Filling its cell of a sent message's collage, cropped.
    Cell,
}

/// T3 Code's ring around sent media and its collage's hairlines: white at 12%.
const MEDIA_EDGE: u32 = 0xffffff1f;

/// The images a sent message carries, above its bubble as T3 Code's: one
/// keeps its own shape, several share one rounded shape split by hairlines.
pub(crate) fn sent_images(
    connection: Connection,
    preview: Preview,
    sources: impl Fn() -> Vec<String> + 'static,
    above_bubble: impl Fn() -> bool + 'static,
    max_width: impl Fn() -> f32 + 'static,
) -> impl Element {
    let sources = computed(sources);
    let count = move || sources.with(Vec::len);
    let collage = move || count() > 1;
    view()
        .max_w(move || px(max_width()))
        .rounded(px(16.0))
        // The corner by the bubble tightens, as the bubble's does.
        .style_when(above_bubble, |s| s.mb(px(4.0)).rounded_br(px(6.0)))
        .overflow_hidden()
        // An inset shadow sits below native image children. The outline
        // draws above them, inside the frame, without changing its size.
        .outline(px(1.0))
        .outline_offset(px(-1.0))
        .outline_color(nsh_core::element::rgba(MEDIA_EDGE))
        .style_when(collage, move |s| {
            // T3 Code's collage: two side by side 12rem tall, three with the
            // first beside the other two, more in pairs 6rem tall with an odd
            // last one across.
            s.w(px(320.0))
                .grid()
                .gap(px(1.0))
                .bg(nsh_core::element::rgba(MEDIA_EDGE))
                .grid_template_columns(move || {
                    SharedString::from(if count() == 3 { "3fr 2fr" } else { "1fr 1fr" })
                })
                .grid_template_rows(move || {
                    SharedString::from(match count() {
                        2 => "192px".to_owned(),
                        3 => "95.5px 95.5px".to_owned(),
                        count => vec!["96px"; count.div_ceil(2)].join(" "),
                    })
                })
        })
        .child(each(
            move || {
                sources
                    .get()
                    .into_iter()
                    .enumerate()
                    .map(|(index, source)| (index, source, collage()))
                    .collect::<Vec<_>>()
            },
            // A single image becoming a collage must recreate its image frame.
            |item: &(usize, String, bool)| item.clone(),
            move |item, _| {
                let index = item.get_untracked().0;
                let source = move || item.get().1;
                view()
                    .overflow_hidden()
                    .style_when(collage, move |s| {
                        s.bg(crate::ui::colors::BACKGROUND)
                            .row_span(move || -> u16 {
                                if count() == 3 && index == 0 { 2 } else { 1 }
                            })
                            .col_span(move || -> u16 {
                                let count = count();
                                if count > 3 && count % 2 == 1 && index + 1 == count {
                                    2
                                } else {
                                    1
                                }
                            })
                    })
                    .child(image_view(
                        connection,
                        preview,
                        source,
                        if item.get_untracked().2 {
                            ImageSize::Cell
                        } else {
                            ImageSize::Sent
                        },
                    ))
            },
        ))
}

/// A conversation's or the composer's image, which opens in the viewer and,
/// on the desktop, offers to save it from its menu.
pub(crate) fn image_view(
    connection: Connection,
    preview: Preview,
    source: impl Fn() -> String + 'static,
    size: ImageSize,
) -> impl Element {
    let source = computed(source);
    let key = computed(move || {
        (
            connection.host.get(),
            connection
                .composer
                .with(|p| p.controls.navigation.cwd.clone()),
            source.get(),
        )
    });
    loaded_image(
        connection.client,
        key,
        size,
        tr("画像を拡大"),
        move |file| {
            #[cfg(desktop)]
            preview.open_image(connection, source.get_untracked(), file);
            #[cfg(not(desktop))]
            preview.show(file);
        },
        true,
    )
}

/// The image `key` (Host, folder, source) names, loaded to a local copy and
/// `press`ed with it; `menu` offers to save it on the desktop.
fn loaded_image(
    client: Stored<Arc<Client>>,
    key: Memo<(String, String, String)>,
    size: ImageSize,
    label: impl Into<SharedString>,
    press: impl Fn(Shared<LocalFile>) + 'static,
    menu: bool,
) -> View {
    let label = label.into();
    let decode_error = signal(None::<String>);
    // Its pixels once it loaded, and the points it shows in.
    let pixels = signal(None::<(f32, f32)>);
    let natural = size == ImageSize::Natural;
    let cell = size == ImageSize::Cell;
    let shown = move || match size {
        ImageSize::Box(width, height) => (width, height),
        // `max-height: 30rem`; until it loads, T3 Code's 16:9 slot.
        ImageSize::Natural => pixels.get().map_or((480.0, 270.0), |(width, height)| {
            let scale = (480.0 / height).min(1.0);
            (width * scale, height * scale)
        }),
        // `h-48 w-auto min-w-24`; until it loads, its `w-64` placeholder.
        ImageSize::Sent => pixels.get().map_or((256.0, 192.0), |(width, height)| {
            ((width * 192.0 / height.max(1.0)).max(96.0), 192.0)
        }),
        ImageSize::Cell => (0.0, 0.0),
    };
    let fit = match size {
        ImageSize::Box(..) | ImageSize::Natural => ResizeMode::Contain,
        ImageSize::Sent | ImageSize::Cell => ResizeMode::Cover,
    };
    let loaded = resource(
        move || Some(key.get()),
        move |(host, cwd, source), _| load_image(client.get(), host, cwd, source),
    );
    effect(move || {
        key.get();
        decode_error.set(None);
    });
    let failed = move || loaded.error().is_some() || decode_error.get().is_some();
    let press = Rc::new(press);
    // TextView's image wrapper has intrinsic width; a percentage alone collapses it.
    // A failed image keeps only its message's height, not the image's.
    let frame = if cell {
        view().w_full().h_full()
    } else {
        view()
            .w(move || px(shown().0))
            .style_when(move || !failed(), move |s| s.min_h(move || px(shown().1)))
    };
    frame
        .max_w(relative(1.0))
        .gap_2()
        .child(when(
            move || loaded.get().is_some(),
            move || {
                let press = press.clone();
                let image = image(move || {
                    loaded.with(|file| {
                        file.map(|file| file.path.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
                })
                .w_full()
                .style_when(move || !cell, move |s| s.h(move || px(shown().1)))
                .style_when(move || cell, |s| s.h_full())
                .object_fit(fit)
                .style_when(move || natural, |s| s.rounded(px(10.0)).overflow_hidden())
                .on_load(move |loaded: &Size<DevicePixels>| {
                    pixels.set(Some((loaded.width.0 as f32, loaded.height.0 as f32)))
                })
                .label(label.clone())
                .role(AccessibilityRole::ImageButton)
                .on_error(move |reason: &str| decode_error.set(Some(reason.into())))
                .on_press(move || {
                    if let Some(file) = loaded.get() {
                        press(file);
                    }
                });
                #[cfg(desktop)]
                let image = image.on_context_menu(move |_: &ContextMenuEvent| {
                    if let Some(file) = loaded.get().filter(|_| menu) {
                        crate::ui::popup(
                            &serde_json::json!([{"id": "save", "title": tr("画像を名前を付けて保存…")}]),
                            move |_| save_image(file.clone(), None),
                        );
                    }
                });
                #[cfg(not(desktop))]
                let _ = menu;
                image
            },
        ))
        .child(when(
            move || loaded.loading(),
            || text(tr("画像を読み込み中…")),
        ))
        .child(when(failed, move || {
            use crate::conversation::look;
            view()
                .gap_2()
                .items_start()
                // In its slot, as T3 Code's media frame.
                .style_when(move || natural, move |s| {
                    s.w_full()
                        .h(move || px(shown().1))
                        .rounded(px(10.0))
                        .bg(look::CODE)
                        .items_center()
                        .justify_center()
                })
                // As T3 Code's unavailable image: its warning and why, in a
                // small muted chip.
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(look::rule())
                        .bg(look::CODE)
                        .text_color(look::muted())
                        .child(crate::ui::icon(crate::ui::Icon::WARNING, 14.0))
                        // T3 Code's title, then the Host's reason in place
                        // of its alt text.
                        .child(
                            text(move || {
                                let reason = loaded
                                    .error()
                                    .or_else(|| decode_error.get())
                                    .map(localized)
                                    .unwrap_or_default();
                                format!("{} · {reason}", tr("画像を表示できません"))
                            })
                            .text_size(px(12.0))
                            .line_height(px(16.0)),
                        ),
                )
                .child(view().flex_row().child(crate::ui::text_button(
                    tr("再試行"),
                    move || {
                        decode_error.set(None);
                        loaded.refetch()
                    },
                )))
        }))
}
