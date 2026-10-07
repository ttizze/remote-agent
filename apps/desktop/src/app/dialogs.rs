//! Confirmation dialogs shared by every screen.
use super::Desktop;
use agent_core::{state::Intent, view::thread_menu::ThreadMenuConfirmation};
use gpui_kit::{
    component::{
        WindowExt,
        button::ButtonVariant,
        dialog::{AlertDialog, DialogButtonProps},
    },
    *,
};
use std::rc::Rc;

/// What a confirmation dialog asks.
pub(crate) struct Confirm {
    pub(crate) title: Option<String>,
    pub(crate) message: String,
    /// The confirming button's label.
    pub(crate) action: String,
    /// Draws the confirming button red.
    pub(crate) destructive: bool,
}

impl Desktop {
    /// Asks before running `confirmed`.
    pub(crate) fn confirm(
        &self,
        dialog: Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
        confirmed: impl Fn(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
    ) {
        let owner = cx.entity().downgrade();
        let confirmed = Rc::new(confirmed);
        let Confirm {
            title,
            message,
            action,
            destructive,
        } = dialog;
        let confirm_label: SharedString = action.into();
        window.open_alert_dialog(cx, move |alert: AlertDialog, _, _| {
            let owner = owner.clone();
            let confirmed = confirmed.clone();
            let alert = match &title {
                Some(title) => alert.title(title.clone()),
                None => alert,
            };
            alert
                .description(message.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(confirm_label.clone())
                        .ok_variant(if destructive {
                            ButtonVariant::Danger
                        } else {
                            ButtonVariant::Primary
                        })
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let confirmed = confirmed.clone();
                    let _ = owner.update(cx, |view, cx| confirmed(view, window, cx));
                    true
                })
        });
    }

    /// Runs `intent`, first asking when the view model attached a confirmation.
    pub(crate) fn perform_confirmed(
        &self,
        intent: Intent,
        confirmation: Option<&ThreadMenuConfirmation>,
        confirm_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match confirmation {
            None => self.perform(intent),
            Some(confirmation) => self.confirm(
                Confirm {
                    title: confirmation.title.clone(),
                    message: confirmation.message.clone(),
                    action: confirm_label.into(),
                    destructive: confirmation.destructive,
                },
                window,
                cx,
                move |view, _, _| view.perform(intent.clone()),
            ),
        }
    }
}
