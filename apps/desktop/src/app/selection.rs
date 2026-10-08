use gpui_kit::{
    App, Context, Entity, EventEmitter, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, Render, SharedString, Styled, Window, anchored,
    base::TextSelection,
    canvas,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::TextareaState,
        menu::{ContextMenuExt, PopupMenuItem},
    },
    deferred, div, point,
    prelude::FluentBuilder,
    px,
};

pub(super) enum SelectionAction {
    AddToChat(String),
    AskSideChat(String),
    Explain(String),
}

pub(super) struct ConversationSelection {
    selected: Option<(Point<Pixels>, String)>,
    pending: Option<Point<Pixels>>,
    allow_side_chat: bool,
}

impl EventEmitter<SelectionAction> for ConversationSelection {}

impl ConversationSelection {
    pub(super) fn new(allow_side_chat: bool) -> Self {
        Self {
            selected: None,
            pending: None,
            allow_side_chat,
        }
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        if self.selected.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn wrap(entity: &Entity<Self>, content: impl IntoElement) -> impl IntoElement {
        let owner = entity.clone();
        let context_owner = entity.clone();
        let close_owner = entity.clone();
        div()
            .id("conversation-selection")
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("conversation-selection-content")
                    .size_full()
                    .flex()
                    .flex_col()
                    .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                        owner.update(cx, |view, cx| {
                            view.pending = Some(event.position);
                            cx.notify();
                        });
                    })
                    .on_scroll_wheel(move |_, _, cx| {
                        close_owner.update(cx, |view, cx| view.clear(cx));
                    })
                    .child(content)
                    .context_menu(move |menu, window, cx| {
                        context_owner.update(cx, |view, cx| view.clear(cx));
                        let text = TextSelection::selected_text(window, cx);
                        let query = text.clone();
                        menu.item(
                            PopupMenuItem::new("コピー")
                                .disabled(text.is_empty())
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                        text.clone(),
                                    ));
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Googleで検索")
                                .disabled(query.is_empty())
                                .on_click(move |_, _, cx| cx.open_url(search_url(&query).as_str())),
                        )
                    }),
            )
            .child(entity.clone())
    }
}

impl Render for ConversationSelection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity();
        let capture = self.pending.take().map(|position| {
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    // Selected substrings are projected during TextView paint, not mouse-up.
                    window.defer(cx, move |window, cx| {
                        let text = TextSelection::selected_text(window, cx);
                        owner.update(cx, |view, cx| {
                            view.selected = (!text.is_empty()).then_some((position, text));
                            cx.notify();
                        });
                    });
                },
            )
            .size(px(0.))
        });
        let Some((position, text)) = &self.selected else {
            return div().children(capture).into_any_element();
        };
        let add = text.clone();
        let details = text.clone();
        let side = text.clone();
        let menu = h_flex()
            .id("selection-actions")
            .debug_selector(|| "selection-actions".into())
            .bg(gpui_kit::rgb(0x303030))
            .border_1()
            .border_color(gpui_kit::rgb(0x454545))
            .rounded_lg()
            .shadow_md()
            .on_mouse_down_out(cx.listener(|view, _, _, cx| view.clear(cx)))
            .child(
                Button::new("selection-add")
                    .debug_selector(|| "selection-add".into())
                    .label("チャットに追加")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.emit(SelectionAction::AddToChat(add.clone()));
                        view.clear(cx);
                    })),
            )
            .child(
                Button::new("selection-details")
                    .debug_selector(|| "selection-details".into())
                    .label("詳細を表示")
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.emit(SelectionAction::Explain(details.clone()));
                        view.clear(cx);
                    })),
            )
            .when(self.allow_side_chat, |menu| {
                menu.child(
                    Button::new("selection-side-chat")
                        .debug_selector(|| "selection-side-chat".into())
                        .label("サイドチャットで質問")
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            cx.emit(SelectionAction::AskSideChat(side.clone()));
                            view.clear(cx);
                        })),
                )
            });
        div()
            .children(capture)
            .child(
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui_kit::Anchor::BottomLeft)
                        .offset(point(px(0.), px(-12.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(menu),
                )
                .with_priority(1),
            )
            .into_any_element()
    }
}

fn search_url(text: &str) -> url::Url {
    let mut url = url::Url::parse("https://www.google.com/search").expect("static Google URL");
    url.query_pairs_mut().append_pair("q", text);
    url
}

pub(super) fn append_to_composer(
    composer: &Entity<TextareaState>,
    text: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let value = append_quote(composer.read(cx).value().as_ref(), text);
    set_composer_text(composer, value, window, cx);
}

pub(super) fn set_composer_text(
    composer: &Entity<TextareaState>,
    value: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    composer.update(cx, |input, cx| {
        let end = value.len();
        input.replace_all(value, window, cx);
        input.set_selected_range(end..end, cx);
        input.focus(window, cx);
    });
}

pub(super) fn append_quote(draft: &str, text: &str) -> SharedString {
    let quote = text
        .split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    if draft.is_empty() {
        format!("{quote}\n\n").into()
    } else {
        format!("{draft}\n\n{quote}\n\n").into()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConversationSelection, SelectionAction, append_to_composer, search_url};
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, InputEvent as _, IntoElement, Modifiers, MouseButton,
        MouseMoveEvent, MouseUpEvent, ParentElement, Render, Styled, TestAppContext,
        VisualTestContext, Window,
        component::{
            Root,
            input::{InputEvent, TextareaState},
            text::TextView,
        },
        div, point, px,
    };

    struct Chat(Entity<ConversationSelection>, gpui::ListState);
    impl Render for Chat {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(ConversationSelection::wrap(
                    &self.0,
                    gpui::list(self.1.clone(), |_, _, _| {
                        div()
                            .pt(px(80.))
                            .child(
                                TextView::markdown("message", "Alpha Bravo\n\nCharlie Delta")
                                    .selectable(true),
                            )
                            .into_any_element()
                    })
                    .flex_1()
                    .min_h_0(),
                ))
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<ConversationSelection>, &mut VisualTestContext) {
        cx.update(crate::appearance::init);
        let selection = cx.new(|_| ConversationSelection::new(true));
        let entity = selection.clone();
        let (_, window) = cx.add_window_view(|window, cx| {
            let chat = cx.new(|_| {
                Chat(
                    entity,
                    gpui::ListState::new(1, gpui::ListAlignment::Top, px(600.)),
                )
            });
            Root::new(chat, window, cx)
        });
        window.run_until_parked();
        window.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (selection, window)
    }

    fn select_first_paragraph(cx: &mut VisualTestContext) {
        let start = point(px(1.), px(90.));
        let end = point(px(250.), px(90.));
        cx.simulate_mouse_move(start, None, Modifiers::default());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        // Native input can deliver the final move and release before the next paint.
        cx.update(|window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position: end,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseUpEvent {
                    position: end,
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn selecting_text_shows_actions_and_right_click_copies_only_the_selection(
        cx: &mut TestAppContext,
    ) {
        let (selection, cx) = setup(cx);
        select_first_paragraph(cx);
        let selected = selection.read_with(cx, |view, _| view.selected.as_ref().unwrap().1.clone());
        assert_eq!(selected, "Alpha Bravo\n");
        assert!(cx.debug_bounds("selection-actions").is_some());
        let at = point(px(50.), px(90.));
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes("down enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Alpha Bravo\n"
            )
        });
    }

    #[gpui::test]
    fn side_chat_keeps_selection_actions_without_offering_an_invisible_nested_chat(
        cx: &mut TestAppContext,
    ) {
        let (selection, cx) = setup(cx);
        cx.update(|_, cx| selection.update(cx, |view, _| view.allow_side_chat = false));
        select_first_paragraph(cx);
        assert!(cx.debug_bounds("selection-add").is_some());
        assert!(cx.debug_bounds("selection-details").is_some());
        assert!(cx.debug_bounds("selection-side-chat").is_none());
    }

    #[gpui::test]
    fn right_click_search_uses_only_selected_text(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        select_first_paragraph(cx);
        let at = point(px(50.), px(90.));
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes("down down enter");
        let url = url::Url::parse(&cx.opened_url().unwrap()).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("www.google.com"));
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [("q".into(), "Alpha Bravo\n".into())]
        );
    }

    #[gpui::test]
    fn selection_actions_keep_the_text_and_details_request_an_ai_explanation(
        cx: &mut TestAppContext,
    ) {
        let (selection, cx) = setup(cx);
        let actions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&selection, move |_, action, _| match action {
                SelectionAction::AddToChat(text) => {
                    received.borrow_mut().push(("main", text.clone()))
                }
                SelectionAction::AskSideChat(text) => {
                    received.borrow_mut().push(("side", text.clone()))
                }
                SelectionAction::Explain(text) => {
                    received.borrow_mut().push(("explain", text.clone()))
                }
            })
        });
        for (id, destination) in [
            ("selection-add", "main"),
            ("selection-side-chat", "side"),
            ("selection-details", "explain"),
        ] {
            select_first_paragraph(cx);
            let at = cx.debug_bounds(id).unwrap().center();
            cx.simulate_click(at, Modifiers::default());
            cx.run_until_parked();
            assert_eq!(
                actions.borrow().last(),
                Some(&(destination, "Alpha Bravo\n".to_owned()))
            );
            assert!(selection.read_with(cx, |view, _| view.selected.is_none()));
        }
    }

    #[gpui::test]
    fn adding_a_quote_emits_the_draft_change_and_keeps_the_existing_text(cx: &mut TestAppContext) {
        cx.update(crate::appearance::init);
        let (composer, cx) = cx.add_window_view(TextareaState::new);
        cx.update(|window, cx| {
            composer.update(cx, |input, cx| input.set_value("unsent draft", window, cx))
        });
        let changed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = changed.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&composer, move |input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    received
                        .borrow_mut()
                        .push(input.read(cx).value().to_string());
                }
            })
        });
        cx.update(|window, cx| append_to_composer(&composer, "日本語\nsecond", window, cx));
        cx.run_until_parked();
        let expected = "unsent draft\n\n> 日本語\n> second\n\n";
        assert_eq!(changed.borrow().last().map(String::as_str), Some(expected));
        composer.read_with(cx, |input, _| {
            assert_eq!(input.value().as_ref(), expected);
            assert_eq!(input.selected_range(), expected.len()..expected.len());
        });
        cx.update(|window, cx| composer.update(cx, |input, cx| input.set_value("", window, cx)));
        cx.update(|window, cx| append_to_composer(&composer, "first", window, cx));
        cx.run_until_parked();
        assert_eq!(
            changed.borrow().last().map(String::as_str),
            Some("> first\n\n")
        );
    }

    #[test]
    fn search_encodes_unicode_and_reserved_characters() {
        let text = "日本語 & x=1\n次の行";
        let url = search_url(text);
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("www.google.com"));
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [("q".into(), text.into())]
        );
    }
}
