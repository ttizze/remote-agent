//! Approval settings in the model picker's footer. Core owns the provider's
//! settings, their loading and the choices.
use crate::Connection;
use crate::l10n::tr;
use crate::ui::{self, Icon, colors};
use agent_core::{
    presentation::{permissions::PERMISSION_CHOICES, view::Presentation},
    state::{Intent, operations as op},
};
use agent_protocol::{permissions::PermissionMode, session::ProviderKind};
use nsh_core::{
    element::{Rgba, rgb},
    overlay::{Align, Placement},
    prelude::*,
};

/// The selected full-access choice warns in the permissions menu.
const FULL_ACCESS: Rgba = rgb(0xf58b42);
/// What the control and its menu show of the current draft's provider.
#[derive(Clone, PartialEq)]
struct Permissions {
    provider: Option<ProviderKind>,
    title: &'static str,
    label: &'static str,
    mode: Option<PermissionMode>,
    version: Option<String>,
    loading: bool,
    error: Option<String>,
    connected: bool,
}

impl Permissions {
    fn of(p: &Presentation) -> Self {
        let control = p.snapshot.permission_control(&p.settings.draft_key());
        Self {
            provider: control.provider,
            title: control.title,
            label: control.label,
            mode: control.mode,
            version: control.version,
            loading: control.loading,
            error: control.error,
            connected: p.snapshot.connection.connected,
        }
    }

    fn enabled(&self) -> bool {
        self.connected && self.provider.is_some()
    }

    /// The change to `mode`, once the settings it replaces are known.
    fn update(&self, mode: PermissionMode) -> Option<Intent> {
        if self.loading || !self.connected {
            return None;
        }
        Some(Intent::UpdatePermissionSettings(
            op::UpdatePermissionSettings {
                provider: self.provider?,
                mode,
                version: self.version.clone()?,
            },
        ))
    }

    fn read(&self) -> Option<Intent> {
        self.provider
            .map(|provider| Intent::ReadPermissionSettings(op::ReadPermissionSettings { provider }))
    }
}

pub(crate) fn picker_control() -> View {
    let connection = use_context::<Connection>().unwrap();
    let permissions = computed(move || connection.publication.with(|p| Permissions::of(p)));
    let label = move || tr(permissions.with(|p| p.label)).to_owned();
    let open = signal(false);
    let dispatch = move |intent: Option<Intent>| {
        if let Some(intent) = intent {
            connection.dispatch(intent);
        }
    };
    let trigger = nsh_base::button()
        .accessibility_label(nsh_core::element::Prop::from(
            (move || Some(SharedString::from(tr("承認方法を変更")))).into_reactive(),
        ))
        .disabled(move || !permissions.with(Permissions::enabled))
        .h(px(28.0))
        .px_2()
        .gap(px(6.0))
        .flex_row()
        .items_center()
        .rounded(px(ui::RADIUS))
        .text_color(colors::muted())
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .disabled_style(|s| s.opacity(0.5))
        .child(ui::icon(Icon::SHIELD, 14.0))
        .child(text(label).text_size(px(12.0)).number_of_lines(1))
        .child(ui::icon(Icon::CHEVRON_UP, 12.0));
    view().flex_row().items_center().child(
        nsh_base::popover()
            .open(move || Some(open.get()))
            .on_open_change(move |event: &nsh_base::PopoverEvent| match event {
                nsh_base::PopoverEvent::OpenChange(true, _) => {
                    if permissions.with_untracked(Permissions::enabled) {
                        open.set(true);
                        dispatch(permissions.with_untracked(Permissions::read));
                    }
                }
                nsh_base::PopoverEvent::OpenChange(false, _) => open.set(false),
                _ => {}
            })
            .placement(Placement::Top)
            .align(Align::End)
            .offset(px(8.0))
            .popup_class("bg-[#0f0f0f] rounded-[16px] border border-[#303030]")
            .trigger(trigger)
            .child(menu(permissions, dispatch, move || open.set(false))),
    )
}

/// The provider's three modes, each with what it allows, the current one checked.
fn menu(
    permissions: Memo<Permissions>,
    dispatch: impl Fn(Option<Intent>) + Copy + 'static,
    close: impl Fn() + Copy + 'static,
) -> View {
    let heading = view()
        .w(px(430.0))
        .p_3()
        .gap_1()
        .text_color(colors::foreground())
        .child(
            text(move || tr(permissions.with(|p| p.title)).to_owned())
                .pb_2()
                .text_size(px(12.0))
                .text_color(colors::muted()),
        );
    PERMISSION_CHOICES
        .into_iter()
        .fold(heading, |menu, (mode, label, description)| {
            menu.child(choice(
                permissions,
                dispatch,
                close,
                mode,
                label,
                description,
            ))
        })
        .child(when(
            move || permissions.with(|p| p.loading),
            || {
                text(tr("読み込み中…"))
                    .text_size(px(12.0))
                    .text_color(colors::muted())
            },
        ))
        .child(when(
            move || permissions.with(|p| p.error.is_some()),
            move || {
                view()
                    .gap_2()
                    .items_start()
                    .child(
                        text(move || permissions.with(|p| p.error.clone().unwrap_or_default()))
                            .selectable(true)
                            .text_size(px(14.0)),
                    )
                    .child(ui::text_button(tr("再読み込み"), move || {
                        dispatch(permissions.with_untracked(Permissions::read))
                    }))
            },
        ))
}

fn choice(
    permissions: Memo<Permissions>,
    dispatch: impl Fn(Option<Intent>) + Copy + 'static,
    close: impl Fn() + Copy + 'static,
    mode: PermissionMode,
    label: &'static str,
    description: &'static str,
) -> nsh_base::ButtonElement {
    let current = move || permissions.with(|p| p.mode == Some(mode));
    nsh_base::button()
        .accessibility_label(Some(SharedString::from(tr(label))))
        .disabled(move || permissions.with(|p| p.update(mode).is_none()))
        .w_full()
        .py_2()
        .px_2()
        .gap_3()
        .flex_row()
        .items_center()
        .rounded(px(ui::RADIUS))
        .text_color(move || {
            if mode == PermissionMode::FullAccess && current() {
                FULL_ACCESS
            } else {
                colors::foreground()
            }
        })
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .disabled_style(|s| s.opacity(0.5))
        // As T3 Code's menu, choosing a mode closes it.
        .on_press(move || {
            dispatch(permissions.with_untracked(|p| p.update(mode)));
            close();
        })
        .child(ui::icon(Icon::SHIELD, 16.0))
        .child(
            view()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(text(tr(label)).text_size(px(14.0)))
                .child(
                    text(tr(description))
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                ),
        )
        .child(when(current, || ui::icon(Icon::CHECK, 14.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::state::{Draft, Snapshot};
    use agent_protocol::permissions::PermissionSettings;
    use std::sync::Arc;

    fn presentation(result: Option<Result<PermissionSettings, String>>) -> Arc<Presentation> {
        let mut state = Snapshot::default();
        state.connection.connected = true;
        let draft = state.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut state.input.drafts).insert(
            draft,
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: ProviderKind::Codex,
                    id: "native".into(),
                }),
                ..Default::default()
            }),
        );
        state.catalog.permission_settings = Arc::new(
            [(
                ProviderKind::Codex,
                Arc::new(op::PermissionSettingsState { result }),
            )]
            .into(),
        );
        Presentation::project(Arc::new(state), "", None).unwrap()
    }

    #[test]
    fn a_mode_changes_only_from_the_loaded_version_of_the_drafts_provider() {
        let loaded = Permissions::of(&presentation(Some(Ok(PermissionSettings {
            mode: Some(PermissionMode::FullAccess),
            version: "revision-2".into(),
        }))));
        assert_eq!(loaded.title, "Codex の承認方法");
        assert_eq!(loaded.label, "フルアクセス");
        assert!(loaded.enabled());
        assert!(
            matches!(loaded.update(PermissionMode::Ask), Some(Intent::UpdatePermissionSettings(request)) if request.provider == ProviderKind::Codex && request.version == "revision-2" && request.mode == PermissionMode::Ask)
        );
        assert!(
            matches!(loaded.read(), Some(Intent::ReadPermissionSettings(request)) if request.provider == ProviderKind::Codex)
        );

        let loading = Permissions::of(&presentation(None));
        assert!(loading.loading);
        assert!(loading.update(PermissionMode::Auto).is_none());

        let failed = Permissions::of(&presentation(Some(Err("offline".into()))));
        assert_eq!(failed.error.as_deref(), Some("offline"));
        assert!(failed.update(PermissionMode::Auto).is_none());
        assert!(failed.read().is_some(), "a failure can be retried");

        let disconnected = Permissions {
            connected: false,
            ..loaded
        };
        assert!(!disconnected.enabled());
        assert!(disconnected.update(PermissionMode::Ask).is_none());
    }
}
