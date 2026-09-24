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
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.shift || modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
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
        match event.keystroke.key.as_str() {
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
                thread_id: self.draft_key().into(),
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
                    .rounded(px(8.))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(0x444444)))
                    .child(div().text_sm().child(candidate.invocation.name.clone()))
                    .child(
                        div()
                            .text_xs()
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
    use gpui_kit::{EntityInputHandler, Focusable, TestAppContext};

    #[gpui::test]
    fn invocation_completion_preserves_suffix_and_does_not_accept_ime(cx: &mut TestAppContext) {
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
