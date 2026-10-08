//! The small GitHub repository publish form used by the Git toolbar.
use crate::app::{Desktop, ui::color};
use agent_core::state::Intent;
use gpui_kit::{
    component::{
        Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        notification::Notification,
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// Opens the GitHub-only publish form for the selected checkout.
pub(super) fn open(cwd: String, window: &mut Window, cx: &mut Context<Desktop>) {
    let desktop = cx.entity().downgrade();
    let form = cx.new(|cx| PublishForm::new(desktop, cwd, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Publish repository")
            .w(px(432.))
            .child(form.clone())
    });
}

struct PublishForm {
    desktop: WeakEntity<Desktop>,
    cwd: String,
    repository: Entity<InputState>,
    private: bool,
    error: Option<String>,
    _subscription: Subscription,
}

impl PublishForm {
    fn new(
        desktop: WeakEntity<Desktop>,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let repository = cx.new(|cx| InputState::new(window, cx).placeholder("owner/repository"));
        repository.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe_in(&repository, window, |form, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                form.submit(window, cx);
            }
        });
        Self {
            desktop,
            cwd,
            repository,
            private: true,
            error: None,
            _subscription: subscription,
        }
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let repository = self.repository.read(cx).value().trim().to_owned();
        if !valid_repository(&repository) {
            self.error = Some("Enter a GitHub repository as owner/name.".into());
            cx.notify();
            return;
        }
        let intent = Intent::PublishRepository {
            cwd: self.cwd.clone(),
            repository,
            visibility: if self.private { "private" } else { "public" }.into(),
            remote_name: Some("origin".into()),
            protocol: Some("ssh".into()),
        };
        let _ = self.desktop.update(cx, |view, _| {
            view.perform_then(intent, |_, result, window, cx| match result {
                Ok(_) => {
                    window.close_dialog(cx);
                    window.push_notification(
                        Notification::success("Repository published")
                            .title("GitHub repository published"),
                        cx,
                    );
                }
                Err(error) => {
                    window.push_notification(
                        Notification::error(agent_core::presentation::error::error_message(error))
                            .title("Repository publish failed"),
                        cx,
                    );
                }
            });
        });
    }
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    !owner.trim().is_empty() && !name.trim().is_empty() && parts.next().is_none()
}

impl Render for PublishForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let private = self.private;
        let error = self.error.clone();
        v_flex()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("Create a GitHub repository and push this checkout to origin."),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Repository"))
                    .child(Input::new(&self.repository).aria_label("GitHub repository")),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_sm().child("Private repository"))
                    .child(
                        Switch::new("publish-private")
                            .checked(private)
                            .on_click(cx.listener(|form, checked: &bool, _, cx| {
                                form.private = *checked;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(error, |view, error| {
                view.child(div().text_sm().text_color(color("danger")).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("publish-cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, window, cx| window.close_dialog(cx))),
                    )
                    .child(
                        Button::new("publish-submit")
                            .primary()
                            .xsmall()
                            .label("Publish")
                            .on_click(cx.listener(|form, _, window, cx| form.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::valid_repository;

    #[test]
    fn repository_requires_one_owner_and_name() {
        assert!(valid_repository("acme/project"));
        assert!(!valid_repository("project"));
        assert!(!valid_repository("acme/"));
        assert!(!valid_repository("acme/project/extra"));
    }
}
