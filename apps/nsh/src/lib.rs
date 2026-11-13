//! Native UI over the same ClientSession and presentation as the existing apps.
#[cfg(any(all(target_os = "macos", debug_assertions), test))]
mod acceptance;
mod accounts;
mod activity;
mod application;
mod background_work;
mod browser;
mod client;
mod composer;
mod connections;
mod conversation;
#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod desktop_workspace;
mod details;
#[cfg(desktop)]
mod devices;
#[cfg(dictation)]
mod dictation;
mod editor;
mod environment;
mod file_actions;
mod files;
mod find;
mod invocations;
mod l10n;
mod media;
mod model_controls;
mod native;
mod notices;
#[cfg(desktop)]
mod onboarding;
#[cfg(desktop)]
mod permissions;
mod plans;
mod projects;
mod queue;
mod requests;
mod rich_text;
#[cfg(any(desktop, target_os = "ios"))]
mod root;
#[cfg(desktop)]
mod settings;
mod shared;
mod side_chat;
#[cfg(desktop)]
mod sidebar;
mod storage;
mod terminal;
#[cfg(any(not(any(target_os = "macos", target_os = "ios")), test))]
mod terminal_screen;
mod threads;
mod ui;
mod workbench;
mod worktrees;

#[cfg(desktop)]
pub use desktop::run as run_desktop;
#[cfg(target_os = "ios")]
pub use native::ios::register as register_ios;

use agent_core::{
    presentation::{
        composer::ComposerPresentation, context::ConversationContext,
        conversation::RenderedConversation, settings::SettingsPresentation, view::Presentation,
    },
    state::Intent,
};
use application::Navigation;
use l10n::tr;
use nsh_core::{HeaderSystemItem, prelude::*};
use serde_json::json;
use shared::Shared;
use std::sync::Arc;

/// A view's connection: its Host's publication and, separately, the parts views read.
/// Core keeps a part's Arc while it is unchanged, so a part's readers run again only
/// when that part changes, not on every streamed delta of the conversation.
#[derive(Clone, Copy)]
pub(crate) struct Connection {
    application: Stored<Arc<application::Application>>,
    state: ReadSignal<Shared<application::State>>,
    client: Stored<Arc<client::Client>>,
    host: Memo<String>,
    publication: Memo<Shared<Presentation>>,
    composer: Memo<Shared<ComposerPresentation>>,
    context: Memo<Shared<ConversationContext>>,
    settings: Memo<Shared<SettingsPresentation>>,
    conversation: Memo<Option<Shared<RenderedConversation>>>,
    scope: Memo<agent_core::panels::PanelScope>,
    pairing: Memo<bool>,
    pane: Option<u64>,
    composer_ref: ElementRef,
    /// The desktop composer's box, which rows dragged from a file tree drop on.
    composer_box: ElementRef,
    /// The composer's open model form, which the settings can open too.
    model_picker: Signal<Option<model_controls::Picker>>,
}

/// What every connection of one application shares.
#[derive(Clone, Copy)]
pub(crate) struct AppHandles {
    application: Stored<Arc<application::Application>>,
    state: ReadSignal<Shared<application::State>>,
    client: Stored<Arc<client::Client>>,
    pairing: Memo<bool>,
}

impl Connection {
    /// A connection to `publication`, sharing `handles`.
    fn new(
        handles: AppHandles,
        host: Memo<String>,
        publication: Memo<Shared<Presentation>>,
        pane: Option<u64>,
    ) -> Self {
        Self {
            application: handles.application,
            state: handles.state,
            client: handles.client,
            pairing: handles.pairing,
            host,
            publication,
            composer: computed(move || publication.with(|p| Shared(p.composer.clone()))),
            context: computed(move || publication.with(|p| Shared(p.context.clone()))),
            settings: computed(move || publication.with(|p| Shared(p.settings.clone()))),
            conversation: computed(move || {
                publication.with(|p| p.conversation.clone().map(Shared))
            }),
            scope: computed(move || {
                publication.with(|p| {
                    p.snapshot
                        .panel_scope(host.get(), p.context.navigation().draft_key)
                })
            }),
            pane,
            composer_ref: element_ref(),
            composer_box: element_ref(),
            model_picker: signal(None),
        }
    }

    fn handles(self) -> AppHandles {
        AppHandles {
            application: self.application,
            state: self.state,
            client: self.client,
            pairing: self.pairing,
        }
    }

    fn uploading(self) -> bool {
        // Read the owner as well as tracking its publication, so a second input
        // event cannot submit while a newly staged batch is waiting to render.
        self.state.with(|_| ());
        let host = self.host.get();
        let draft = self
            .composer
            .with(|c| c.controls.navigation.draft_key.clone());
        self.application
            .with(|application| application.snapshot().uploading(&host, &draft))
    }

    fn dispatch(self, intent: Intent) {
        self.client
            .with(|client| client.dispatch(&self.host.get_untracked(), self.pane, intent));
    }
}

#[component]
pub fn App() -> impl IntoElement {
    let service = storage::directory().and_then(|root| application::Application::new(root, None));
    let (runtime, service) = match service {
        Ok(service) => service,
        Err(reason) => {
            return view()
                .p_4()
                .child(text(l10n::localized(reason.to_string())))
                .into_element();
        }
    };
    let stored_service = service.clone();
    cleanup(move || {
        let _ = runtime.block_on(service.shutdown());
    });
    app_view(stored_service, Area::Mobile)
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Area {
    Mobile,
    #[cfg(desktop)]
    Shell,
}

/// The application's views, made again in each language and contrast it selects.
fn app_view(service: Arc<application::Application>, area: Area) -> AnyElement {
    let mut updates = service.subscribe();
    let state = signal(Shared(updates.borrow_and_update().clone()));
    spawn(async move {
        while updates.changed().await.is_ok() {
            let next = updates.borrow_and_update().clone();
            state.set(Shared(next));
        }
    });
    let language = computed(move || state.with(|state| state.language));
    #[cfg(desktop)]
    effect(move || {
        language.get();
        nsh_core::element::desktop::set_menu_labels(desktop::menu_labels());
    });
    let contrast = computed(move || state.with(|state| state.contrast));
    let content = each(
        move || vec![(language.get(), contrast.get())],
        |(language, contrast)| (language.code(), *contrast),
        move |_, _| app_content(service.clone(), state.read_only(), area),
    );
    view()
        .w_full()
        .h_full()
        .text_color(move || nsh_base::theme().foreground)
        .child(content)
        .into_element()
}

fn app_content(
    service: Arc<application::Application>,
    state: ReadSignal<Shared<application::State>>,
    area: Area,
) -> AnyElement {
    let application = stored(service.clone());
    let client = stored(service.client.clone());
    let hosts = computed(move || state.with(|state| state.hosts.clone()));
    let host = computed(move || state.with(|state| state.host.clone()));
    let empty = stored(Shared(Presentation::empty(
        agent_core::localization::UiLanguage::default(),
    )));
    let publication = computed(move || {
        state
            .with(|state| state.publications.get(&host.get()).cloned())
            .unwrap_or_else(|| empty.get())
    });
    let handles = AppHandles {
        application,
        state,
        client,
        pairing: computed(move || state.with(|state| state.pairing)),
    };
    let pane = None;
    let connection = Connection::new(handles, host, publication, pane);
    provide_context(connection);
    let preview = media::Preview::new();
    provide_context(preview);
    let workspace = workbench::Workbench::new();
    provide_context(workspace);
    let files = files::Browser::new(preview);
    provide_context(files);
    provide_context(terminal::Focus::new());
    provide_context(find::Find::new());
    let failure = computed(move || state.with(|state| state.failure.clone().map(l10n::localized)));
    use nsh_base::text_view::{TextViewPart, TextViewParts};
    // Native composers use 17pt on iOS (scaled once by its NSH environment)
    // and 14pt on desktop. Use the system font's natural line metrics in both;
    // small headings must not become smaller than the body.
    let body_size = if cfg!(target_os = "ios") { 17.0 } else { 14.0 };
    let typography = [
        TextViewPart::Root,
        TextViewPart::Heading4,
        TextViewPart::Heading5,
        TextViewPart::Heading6,
    ]
    .into_iter()
    .fold(TextViewParts::new(), |parts, part| {
        parts.part(part, move |style| {
            style
                .text_size(px(body_size))
                .line_height(nsh_core::LineHeight::Normal)
        })
    });
    provide_context(typography);
    let routes = signal(vec![0_u8]);
    let settings = computed(move || state.with(|state| state.settings));
    let provider = computed(move || state.with(|state| state.provider));
    let navigate = move |action| application.with(|application| application.navigate(action));
    let account_locked = computed(move || state.with(|state| state.account_locked()));
    let close_settings = move || navigate(Navigation::Settings(false));
    // T3 Code's sidebar: a column beside the content, a sheet over it in a
    // narrow window.
    #[cfg(desktop)]
    let sidebar_shown = computed(move || {
        state
            .with(|state| state.sidebar)
            .presentation(window().width.0)
    });
    #[cfg(desktop)]
    let sidebar = computed(move || {
        sidebar_shown.get() == agent_core::presentation::sidebar::SidebarPresentation::Beside
    });
    #[cfg(desktop)]
    let sidebar_width = signal(sidebar::WIDTH);
    let searching = signal(false);
    let query = signal(String::new());
    let primary = area == Area::Mobile;
    #[cfg(desktop)]
    let primary = primary || area == Area::Shell;
    if primary {
        effect(move || client.with(|client| client.scene(native::in_use())));
    }
    let title = computed(move || {
        connection
            .context
            .with(|p| p.title())
            .unwrap_or_else(|| tr("新しいチャット").into())
    });
    #[cfg(desktop)]
    let project = computed(move || connection.context.with(|p| p.workspace_project()));
    let list = move || {
        threads::List::new(query).view(move || {
            dismiss_keyboard();
            if !is_desktop() {
                routes.set(vec![0, 1]);
            }
        })
    };
    let toolbar_action = move |action: &str| {
        if application.with(|app| app.snapshot().account_locked()) {
            return;
        }
        match action {
            "sidebar" => navigate(Navigation::ToggleSidebar(window().width.0)),
            "search" => searching.update(|open| *open = !*open),
            "worktrees" => worktrees::open(),
            "browser" | "files" | "terminal" | "diff" | "agents" | "sideChat" => {
                workbench::open_tool(action)
            }
            "compose" => {
                // A new chat stays in the selected project; a chat without one
                // works in the Host's chat folder, which is no project.
                connection.dispatch(Intent::NewChat {
                    cwd: if is_desktop() {
                        publication.with(|p| p.context.selected_directory())
                    } else {
                        String::new()
                    },
                });
                if !is_desktop() {
                    routes.set(vec![0, 1]);
                }
            }
            _ => navigate(Navigation::Settings(true)),
        }
    };
    #[cfg(desktop)]
    let hero = desktop_workspace::draft_hero;
    #[cfg(not(desktop))]
    let hero = view;
    // T3 Code's chat canvas, the conversation beside the sidebar: its details
    // card floats over it, as Bex's side panel stands beside the timeline.
    #[cfg(desktop)]
    let canvas = move || {
        window().width.0
            - if sidebar.get() {
                sidebar_width.get()
            } else {
                0.0
            }
    };
    #[cfg(not(desktop))]
    let canvas = move || window().width.0;
    let content = move || {
        let following = signal(true);
        let timeline = conversation::Timeline::new(following);
        // A selected conversation still opening is not a new one.
        let welcome = computed(move || {
            is_desktop()
                && connection.conversation.with(Option::is_none)
                && connection.context.with(|p| p.opening().is_none())
        });
        // As T3 Code's draft hero, a new conversation centers its composer;
        // a conversation's composer lies over the timeline's end, which
        // scrolls under it.
        view()
            .flex_1()
            .min_w_0()
            .pb_keyboard()
            .style_when(move || welcome.get(), |s| s.justify_center())
            .child(when(move || !welcome.get(), move || timeline.list()))
            .child(when(move || welcome.get(), hero))
            // Under the composer, as T3 Code's overlay lies over the timeline.
            .child(when(
                move || !welcome.get(),
                move || timeline.minimap(canvas),
            ))
            .child(when(
                move || is_desktop() && !welcome.get(),
                move || find::bar(use_context::<find::Find>().unwrap(), timeline),
            ))
            .child(
                view()
                    .style_when(
                        move || !welcome.get(),
                        |s| s.absolute().left(px(0.0)).right(px(0.0)).bottom(px(0.0)),
                    )
                    .on_layout(move |bounds: &nsh_core::Bounds<nsh_core::Points>| {
                        timeline.cover(bounds.size.height.0)
                    })
                    .child(composer::composer(following)),
            )
    };
    let notices = move || {
        view()
            .child(notices::notices(
                move || workspace.state.with(|state| state.open),
                move || settings.get(),
            ))
            .child(when(
                move || failure.get().is_some(),
                move || {
                    view()
                        .flex_row()
                        .gap_2()
                        .p_2()
                        .child(text(move || failure.get().unwrap_or_default()))
                        .child(ui::text_button(tr("閉じる"), move || {
                            application.with(|application| application.dismiss_failure())
                        }))
                },
            ))
    };
    match area {
        #[cfg(desktop)]
        Area::Shell => {
            let list = threads::List::new(query);
            use nsh_core::desktop::{TitleBarOptions, TitleBarStyle, WindowAttributes};
            let mut bar = TitleBarOptions::default();
            bar.style = TitleBarStyle::Hidden;
            bar.height = Some(px(ui::HEADER_HEIGHT));
            let mut attributes = WindowAttributes::default();
            attributes.title_bar = Some(bar);
            nsh_core::element::desktop::current_window().set(attributes);
            // An update's worker replaces the app once its event loop ends.
            effect(move || {
                if state.with(|state| state.quitting) {
                    desktop::quit();
                }
            });
            // The first-run setup and the project import take the window's
            // content, the sidebar included.
            let setup = computed(move || state.with(|state| state.onboarding.page().is_some()));
            let tools = move || {
                !settings.get()
                    && !setup.get()
                    && !hosts.with(Vec::is_empty)
                    && workspace.state.with(|s| s.open)
            };
            let maximized = move || tools() && workspace.state.with(|s| s.maximized);
            let tool_width = signal(540.0_f32);
            let details = signal(true);
            let details_inline = move || {
                agent_core::presentation::conversation::details_card_inline(
                    canvas() - if tools() { tool_width.get() } else { 0.0 },
                    window().height.0 - ui::HEADER_HEIGHT,
                )
            };
            let main = move || {
                nsh_base::h_resizable()
                    .handle_class("bg-border group-hover:bg-primary group-active:bg-primary")
                    .handle_accessibility_label(Some(SharedString::from(tr(
                        "ツールパネルのサイズを変更",
                    ))))
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        nsh_base::resizable_panel()
                            .visible(move || !maximized())
                            .min_size(px(360.0))
                            .child(
                                view()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .accessibility_elements_hidden(maximized)
                                    .child(sidebar::conversation_header(
                                        sidebar,
                                        move || {
                                            navigate(Navigation::ToggleSidebar(window().width.0))
                                        },
                                        sidebar::breadcrumb(title, project, move || {
                                            toolbar_action("compose")
                                        }),
                                        sidebar::panel_toggles(
                                            details,
                                            details_inline,
                                            tools,
                                            move || {
                                                application.with(|application| {
                                                    application.set_panel_open(
                                                        workspace.scope.get_untracked(),
                                                        true,
                                                    )
                                                })
                                            },
                                        ),
                                    ))
                                    .child(
                                        view()
                                            .flex_1()
                                            .min_h_0()
                                            .flex_row()
                                            .child(
                                                view()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(content())
                                                    .child(notices::thread_error()),
                                            )
                                            .child(when(
                                                move || details.get() && details_inline(),
                                                move || {
                                                    use agent_core::presentation::conversation::DETAILS_CARD_GAP;
                                                    view()
                                                        .p(px(DETAILS_CARD_GAP))
                                                        .pl_0()
                                                        .child(desktop_workspace::card())
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        nsh_base::resizable_panel()
                            .visible(tools)
                            .size(move || (!maximized()).then(|| px(tool_width.get())))
                            .min_size(move || px(if maximized() { 0.0 } else { 360.0 }))
                            .max_size(move || {
                                (!maximized()).then(|| {
                                    px((window().width.0 * 0.7).min(canvas() - 360.0).max(360.0))
                                })
                            })
                            .on_resize(move |size: &Points| tool_width.set(size.0))
                            .child(workbench::screen(Some(
                                when(
                                    move || maximized() && !sidebar.get(),
                                    move || {
                                        sidebar::header(false, move || {
                                            navigate(Navigation::ToggleSidebar(window().width.0))
                                        })
                                    },
                                )
                                .into_element(),
                            ))),
                    )
            };
            return view()
                .flex_1()
                .flex_row()
                .bg(ui::colors::BACKGROUND)
                .text_color(ui::colors::foreground())
                .child(media::preview())
                .child(browser::host())
                .child(
                    // The settings overlay is modal: what it covers is out of
                    // reach of assistive technologies while it shows.
                    view()
                        .flex_1()
                        .min_w_0()
                        .accessibility_elements_hidden(move || settings.get())
                        .child(list.projects.view())
                        .child(
                            nsh_base::h_resizable()
                                .handle_class("bg-border group-hover:bg-primary group-active:bg-primary")
                                .handle_accessibility_label(Some(SharedString::from(tr("サイドバーの幅を変更"))))
                                .child(
                                    nsh_base::resizable_panel()
                                        .visible(move || sidebar.get() && !setup.get())
                                        .fixed(true)
                                        .size(move || Some(px(sidebar_width.get())))
                                        .min_size(px(256.0))
                                        .max_size(move || Some(px((window().width.0 - 360.0).min(window().width.0 / 2.0).max(256.0))))
                                        .on_resize(move |size: &Points| sidebar_width.set(size.0))
                                        .child(
                                            view()
                                                .size_full()
                                                .accessibility_elements_hidden(move || !sidebar.get() || setup.get())
                                                .child(sidebar::sidebar(hosts, list, move || toolbar_action("compose"))),
                                        ),
                                )
                                .child(
                                    nsh_base::resizable_panel()
                                        .min_size(px(360.0))
                                        .child(
                                            view()
                                                .flex_1()
                                                .min_w_0()
                                                .min_h_0()
                                                .child(notices())
                                                .child(when(move || hosts.with(Vec::is_empty), connections::hero))
                                                .child(when(
                                                    move || !hosts.with(Vec::is_empty) && setup.get(),
                                                    move || onboarding::page(hosts),
                                                ))
                                                .child(when(
                                                    move || !hosts.with(Vec::is_empty) && !setup.get(),
                                                    main,
                                                ))
                                                .child(when(
                                                    move || {
                                                        !setup.get()
                                                            && sidebar_shown.get()
                                                                == agent_core::presentation::sidebar::SidebarPresentation::Sheet
                                                    },
                                                    move || {
                                                        sidebar::sheet(
                                                            hosts,
                                                            list,
                                                            move || toolbar_action("compose"),
                                                            move || navigate(Navigation::DismissSidebarSheet),
                                                        )
                                                    },
                                                )),
                                        ),
                                ),
                        ),
                )
                .child(when(
                    move || settings.get(),
                    move || settings::overlay(hosts),
                ))
                .into_element();
        }
        Area::Mobile => {}
    }
    let navigation = nsh_base::nav_stack().flex_1().child(each(
        move || {
            let mut stack = routes.get();
            if workspace.state.with(|state| state.open) {
                stack.push(3);
            }
            if settings.get() {
                stack.push(2);
                if provider.get().is_some() {
                    stack.push(5);
                }
            }
            stack
        },
        |route: &u8| *route,
        move |route, position| {
            let page = match route.get_untracked() {
                1 => nsh_base::nav_page()
                    .title(move || title.get())
                    .child(notices())
                    .child(content())
                    .header_right(
                        native::menu(
                            "ellipsis",
                            tr("その他"),
                            move || {
                                let mut entries = vec![json!({"id": "refresh", "title": tr("更新"), "symbol": "arrow.clockwise"})];
                                entries.extend(workbench::tool_items());
                                entries.push(json!({"id": "settings", "title": tr("環境"), "symbol": "gearshape"}));
                                entries.push(json!({"id": "worktrees", "title": tr("ワークツリー設定"), "symbol": "arrow.triangle.branch", "enabled": connection.settings.with(|p| p.connected())}));
                                json!(entries)
                            },
                            move |item| {
                                if item == "refresh" {
                                    connection.application.with(|application| { application.request_list(application::ListRequest::Refresh); });
                                } else {
                                    toolbar_action(item);
                                }
                            },
                        )
                        .w(px(44.0))
                        .h(px(44.0)),
                    ),
                2 => nsh_base::nav_page()
                    .title(tr("環境"))
                    .child(notices())
                    .child(environment::overview(hosts)),
                5 => nsh_base::nav_page()
                    .title(move || {
                        connection.settings.with(|p| {
                            p
                                .connection_setup()
                                .agents
                                .into_iter()
                                .find(|a| Some(a.provider) == provider.get())
                                .map(|a| a.name)
                                .unwrap_or_else(|| tr("アカウント").into())
                        })
                    })
                    .header_back_visible(move || !account_locked.get())
                    .pop_on_back(move || !account_locked.get())
                    .pop_on_swipe(move || !account_locked.get())
                    .child(notices())
                    .child(
                        scroll().flex_1().child(
                            view()
                                .p_4()
                                .pb_keyboard()
                                .child(accounts::screen(provider.get_untracked().unwrap())),
                        ),
                    ),
                3 => nsh_base::nav_page()
                    .title(workbench::title)
                    .child(notices())
                    .child(workbench::screen(None)),
                _ => nsh_base::nav_page()
                    .title("Bex")
                    .header_back_title(tr("戻る"))
                    .header_right_system_item(HeaderSystemItem::Compose)
                    .header_right(view().on_press(move || toolbar_action("compose")))
                    .child(notices())
                    .child(when(
                        move || hosts.with(Vec::is_empty),
                        move || connections::screen(hosts),
                    ))
                    .child(when(
                        move || !hosts.with(Vec::is_empty),
                        move || {
                            view()
                                .flex_1()
                                .child(connections::selector(hosts, host))
                                .child(when(
                                    move || searching.get(),
                                    move || {
                                        ui::search_field(
                                            move || query.get(),
                                            move |text| query.set(text.to_owned()),
                                            None,
                                        )
                                        .w_full()
                                    },
                                ))
                                .child(list())
                                .child(native::toolbar(title, toolbar_action).w_full())
                        },
                    )),
            };
            page.on_dismiss(move || {
                if route.get_untracked() == 2 {
                    close_settings();
                }
                if route.get_untracked() == 5 {
                    navigate(Navigation::Provider(None));
                }
                if route.get_untracked() == 3 {
                    files.close();
                }
                routes.update(|routes| routes.truncate(position.get_untracked()));
            })
        },
    ));
    view()
        .flex_1()
        .child(media::preview())
        .child(worktrees::presenter(move || routes.set(vec![0, 1])))
        .child(navigation)
        .into_element()
}
