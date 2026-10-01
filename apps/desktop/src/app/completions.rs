use super::*;
use agent_core::composer::{ComposerSuggestions, insert_invocation};
use agent_protocol::composer::Invocation;

impl Desktop {
    fn completion_suggestions(&self, cx: &App) -> Option<ComposerSuggestions> {
        let input = self.composer.read(cx);
        if self.completion_dismissed
            || !input.selected_range().is_empty()
            || !self.snapshot.connected
        {
            return None;
        }
        self.snapshot
            .composer_suggestions(input.value().to_string(), input.cursor() as u32)
    }
    pub(super) fn completion_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let candidates = self
            .completion_suggestions(cx)
            .map_or_else(Vec::new, |s| s.candidates);
        if candidates.is_empty() {
            return false;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return false;
        }
        match key {
            "up" => self.completion_index = self.completion_index.saturating_sub(1),
            "down" => self.completion_index = (self.completion_index + 1).min(candidates.len() - 1),
            "escape" => self.completion_dismissed = true,
            "tab" => {
                self.accept_completion(window, cx);
            }
            _ => return false,
        }
        cx.stop_propagation();
        cx.notify();
        true
    }
    pub(super) fn accept_completion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return false;
        }
        let candidates = self
            .completion_suggestions(cx)
            .map_or_else(Vec::new, |s| s.candidates);
        let Some(candidate) = candidates.get(
            self.completion_index
                .min(candidates.len().saturating_sub(1)),
        ) else {
            return false;
        };
        self.insert_completion(candidate.invocation.clone(), window, cx);
        true
    }
    fn insert_completion(
        &mut self,
        invocation: Invocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.composer.read(cx);
        let Some(inserted) = insert_invocation(
            input.value().to_string(),
            input.cursor() as u32,
            invocation.kind,
            invocation.name.clone(),
        ) else {
            return;
        };
        self.composer_value = inserted.text.clone().into();
        self.composer_revision += 1;
        let revision = self.composer_revision;
        self.composer_pending = Some(revision);
        self.composer.update(cx, |input, cx| {
            input.set_value(inserted.text.clone(), window, cx);
            input.set_selected_range(inserted.cursor as usize..inserted.cursor as usize, cx);
            input.focus(window, cx);
        });
        self.perform(
            Intent::InsertInvocation {
                thread_id: self.draft_key().clone(),
                text: inserted.text,
                invocation,
            },
            OperationCompletion::Composer(revision),
        );
        self.completion_dismissed = false;
        cx.notify();
    }
    pub(super) fn completion_menu(&self, cx: &Context<Self>) -> AnyElement {
        let Some(suggestions) = self.completion_suggestions(cx) else {
            return div().into_any_element();
        };
        let candidates = suggestions.candidates;
        let mut menu = v_flex()
            .id("composer-invocations")
            .max_h(px(230.))
            .overflow_y_scroll()
            .gap_1()
            .p_2();
        for (index, candidate) in candidates.iter().enumerate() {
            let selected = index
                == self
                    .completion_index
                    .min(candidates.len().saturating_sub(1));
            let candidate = candidate.clone();
            menu = menu.child(
                div()
                    .id(("invocation", index))
                    .debug_selector(move || format!("composer-candidate-{index}"))
                    .flex()
                    .items_center()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .rounded(px(8.))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(0x444444)))
                    .when(
                        candidate.invocation.kind
                            == agent_protocol::composer::InvocationKind::Skill,
                        |row| row.child(Icon::new(IconName::BookOpen).size(px(14.))),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("composer-candidate-name-{index}"))
                            .max_w(relative(0.5))
                            .flex_shrink_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(candidate.invocation.name.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("composer-candidate-description-{index}")
                            })
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .text_color(rgb(0xaaaaaa))
                            .child(candidate.description.clone()),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.insert_completion(candidate.invocation.clone(), window, cx)
                    })),
            );
        }
        if let Some(status) = suggestions.status {
            menu = menu.child(div().text_xs().child(status));
        }
        menu.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Arc, Desktop, Mode};
    use agent_protocol::composer::ComposerCandidate;
    use agent_protocol::composer::ComposerCatalog;
    use agent_protocol::composer::Invocation;
    use agent_protocol::composer::InvocationKind;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, EntityInputHandler, Focusable, IntoElement, ParentElement,
        Render, Styled, TestAppContext, Window, div, px,
    };

    struct CompletionView(Entity<Desktop>, f32);
    impl Render for CompletionView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(self.1))
                .child(self.0.update(cx, |view, cx| view.completion_menu(cx)))
        }
    }

    #[gpui::test]
    fn completion_candidates_keep_names_and_descriptions_on_one_line(cx: &mut TestAppContext) {
        let _runtime = runtime(cx);
        for width in [360., 850.] {
            let (_, window) = cx.add_window_view(|window, cx| {
                let desktop = cx.new(|cx| Desktop::new(Mode::SideChat {
                    remote: Some(super::RemoteHost { id: "fixture".into(), name: "fixture".into(), ticket: "invalid-fixture-ticket".into() }),
                    cwd: "/fixture".into(),
                }, window, cx));
                desktop.update(cx, |view, cx| {
                    let snapshot = Arc::make_mut(&mut view.snapshot);
                    snapshot.connected = true;
                    snapshot.composer_catalog = Some(Arc::new(ComposerCatalog {
                        cwd: snapshot.navigation.cwd.clone(), loading: true,
                        candidates: ["Review", "Analyze Data Quality With A Very Long Skill Name"].into_iter().map(|name| ComposerCandidate {
                            invocation: Invocation { kind: InvocationKind::Skill, name: name.into(), path: format!("/fixture/{name}/SKILL.md") },
                            description: "Investigate whether structured datasets and query results are trustworthy enough to use, including freshness, duplicates and missing values".into(),
                        }).collect(), ..Default::default()
                    }));
                    view.composer.update(cx, |input, cx| {
                        input.set_value("/", window, cx);
                        input.set_selected_range(1..1, cx);
                    });
                });
                CompletionView(desktop, width)
            });
            window.run_until_parked();
            for (row_selector, name_selector, description_selector) in [
                (
                    "composer-candidate-0",
                    "composer-candidate-name-0",
                    "composer-candidate-description-0",
                ),
                (
                    "composer-candidate-1",
                    "composer-candidate-name-1",
                    "composer-candidate-description-1",
                ),
            ] {
                let row = window.debug_bounds(row_selector).unwrap();
                let name = window.debug_bounds(name_selector).unwrap();
                let description = window.debug_bounds(description_selector).unwrap();
                assert_eq!(name.top(), description.top());
                assert_eq!(name.size.height, description.size.height);
                assert!(
                    name.left() >= row.left() + px(22.),
                    "every skill reserves the same leading icon"
                );
                assert!(name.right() <= description.left());
                assert!(description.right() <= row.right());
                assert!(
                    row.size.height <= px(32.),
                    "long descriptions must not increase row height"
                );
            }
        }
    }

    fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Runtime {
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
            });
        });
        runtime
    }

    #[gpui::test]
    fn composer_arrow_actions_preserve_vertical_movement_and_reach_text_edges(
        cx: &mut TestAppContext,
    ) {
        let _runtime = runtime(cx);
        let mut composer = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                let mut desktop = Desktop::new(
                    Mode::SideChat {
                        remote: None,
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                );
                Arc::make_mut(&mut desktop.snapshot).connected = true;
                desktop.composer.update(cx, |input, cx| {
                    input.set_readonly(false, cx);
                    input.set_value("ABCDE", window, cx);
                    input.set_selected_range(3..3, cx);
                    input.focus_handle(cx).focus(window, cx);
                });
                composer = Some(desktop.composer.clone());
                desktop
            });
            gpui_kit::component::Root::new(desktop, window, cx)
        });
        let composer = composer.unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes("up");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), 0);
        cx.simulate_keystrokes("down");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), 5);

        cx.update(|window, cx| {
            composer.update(cx, |input, cx| {
                input.set_value("alpha\nbravo\ncharlie", window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| composer.update(cx, |input, cx| input.set_selected_range(8..8, cx)));
        cx.simulate_keystrokes("up");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), 2);
        cx.simulate_keystrokes("up");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), 0);
        cx.update(|_, cx| composer.update(cx, |input, cx| input.set_selected_range(14..14, cx)));
        cx.simulate_keystrokes("down");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), 19);

        let wrapped = "abcdef ".repeat(100);
        let end = wrapped.len();
        cx.update(|window, cx| {
            composer.update(cx, |input, cx| {
                input.set_value(wrapped, window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            composer.update(cx, |input, cx| {
                input.set_selected_range(end - 2..end - 2, cx)
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("up");
        let cursor = composer.read_with(cx, |input, _| input.cursor());
        assert!(
            cursor > 0 && cursor < end - 2,
            "soft wrap must move one row: {cursor}"
        );
        cx.update(|_, cx| {
            composer.update(cx, |input, cx| {
                input.set_selected_range(end - 2..end - 2, cx)
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("down");
        assert_eq!(composer.read_with(cx, |input, _| input.cursor()), end);
    }

    #[gpui::test]
    fn invocation_completion_preserves_suffix_and_does_not_accept_ime(cx: &mut TestAppContext) {
        let _runtime = runtime(cx);
        let view = cx.add_window(|window, cx| {
            Desktop::new(
                Mode::SideChat {
                    remote: None,
                    cwd: "/fixture".into(),
                },
                window,
                cx,
            )
        });
        view.update(cx, |view, window, cx| {
            let snapshot = Arc::make_mut(&mut view.snapshot);
            snapshot.connected = true;
            snapshot.composer_catalog = Some(Arc::new(ComposerCatalog {
                cwd: snapshot.navigation.cwd.clone(),
                candidates: vec![ComposerCandidate {
                    invocation: Invocation {
                        kind: InvocationKind::Skill,
                        name: "review".into(),
                        path: "/fixture/review/SKILL.md".into(),
                    },
                    description: "Review".into(),
                }],
                ..Default::default()
            }));
            view.composer.update(cx, |input, cx| {
                input.set_readonly(false, cx);
                input.set_value("日本語 /rev 後半", window, cx);
                let cursor = "日本語 /rev".len();
                input.set_selected_range(cursor..cursor, cx);
                input.replace_and_mark_text_in_range(None, "iew", Some(3..3), window, cx);
                assert!(input.marked_text_range(window, cx).is_some());
            });
            assert!(!view.accept_completion(window, cx));
            view.composer
                .update(cx, |input, cx| input.unmark_text(window, cx));
            assert!(view.accept_completion(window, cx));
            assert_eq!(
                view.composer.read(cx).value().as_ref(),
                "日本語 $review  後半"
            );
            assert_eq!(view.composer.read(cx).cursor(), "日本語 $review ".len());
            assert!(view.composer.read(cx).focus_handle(cx).is_focused(window));
        })
        .unwrap();
    }
}
