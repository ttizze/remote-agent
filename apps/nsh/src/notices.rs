use crate::Connection;
use crate::l10n::tr;
use crate::ui::{self, Icon, colors};
use agent_core::{
    presentation::error::{ErrorNotice, ErrorScope},
    state::{Intent, operations::ErrorId},
};
use nsh_core::{element::rgba, prelude::*};

/// T3 Code's thread error banner (`ThreadErrorBanner`): the latest turn's
/// failure, floated at the top of the timeline, until it is dismissed. A dismissal holds for that conversation and message while
/// the app runs, as T3's session-scoped one.
#[cfg(desktop)]
pub(crate) fn thread_error() -> impl IntoElement {
    use agent_protocol::session::SessionRef;
    // T3's `--error` (red-500 with a tenth of white) and `--error-foreground`.
    const ERROR: nsh_core::element::Rgba = nsh_core::element::rgb(0xfb414a);
    let connection = use_context::<Connection>().unwrap();
    let dismissed = signal(Vec::<(Option<SessionRef>, String)>::new());
    let shown = computed(move || {
        let (session, error) = connection
            .context
            .with(|p| (p.navigation.thread_id.clone(), p.thread_error()));
        let error = error?;
        let key = (session, error.clone());
        (!dismissed.with(|dismissed| dismissed.contains(&key))).then_some(key)
    });
    view()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .flex_row()
        .justify_center()
        .px_4()
        .pt_3()
        .pointer_events(PointerEvents::BoxNone)
        .child(when(
            move || shown.with(Option::is_some),
            move || {
                view()
                    .max_w(px(768.0))
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .px(px(14.0))
                    .py_3()
                    .rounded(px(14.0))
                    .border_1()
                    .border_color(rgba(0xfb414a52))
                    // T3's glass: the page at 80% under 4% of the error colour
                    // (unblurred: AppKit's blur lightens it).
                    .bg(rgba(0x140c0dcc))
                    .child(
                        view()
                            .h(px(20.0))
                            .pt(px(2.0))
                            .items_center()
                            .text_color(ERROR)
                            .child(ui::icon(Icon::ALERT, 16.0)),
                    )
                    .child(
                        text(move || {
                            shown.with(|key| {
                                key.as_ref()
                                    .map(|(_, error)| error.clone())
                                    .unwrap_or_default()
                            })
                        })
                        .flex_1()
                        .min_w_0()
                        .pt(px(2.0))
                        .selectable(true)
                        .number_of_lines(3)
                        .text_size(px(14.0))
                        .line_height(px(20.0))
                        .text_color(rgba(0xff6467cc)),
                    )
                    .child(
                        ui::bare_button(tr("エラーを閉じる"), move || {
                            if let Some(key) = shown.get_untracked() {
                                dismissed.update(|dismissed| dismissed.push(key));
                            }
                        })
                        .w(px(24.0))
                        .h(px(24.0))
                        .rounded(px(8.0))
                        .text_color(colors::t3_muted())
                        .hover(|s| s.bg(colors::T3_ACCENT))
                        .child(ui::icon(Icon::CLOSE, 14.0)),
                    )
            },
        ))
}

/// Errors as T3 Code's error banners: the title and its dismissal, the reason,
/// and the recovery Core offers.
pub(crate) fn notices(
    workspace: impl Fn() -> bool + 'static,
    settings: impl Fn() -> bool + 'static,
) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    each(
        move || {
            connection.publication.with(|p| {
                let mut notices = p.context.notices();
                let pane = &p.pane_id;
                if connection.pane.is_none() {
                    for scope in [ErrorScope::Application, ErrorScope::Projects] {
                        notices.extend(p.snapshot.error_notices(pane, scope));
                    }
                } else {
                    // A side chat shows its own failures; the Host's stays with
                    // the main conversation.
                    let host = p.snapshot.connection.connection_failure.clone();
                    notices.extend(
                        p.snapshot
                            .error_notices(pane, ErrorScope::Application)
                            .into_iter()
                            .filter(|notice| {
                                !matches!(&notice.id, ErrorId::Application { message }
                                    if Some(message) == host.as_ref())
                            }),
                    );
                }
                if workspace() {
                    notices.extend(p.snapshot.error_notices(pane, ErrorScope::Workspace));
                }
                if settings() {
                    notices.extend(p.settings.notices());
                }
                notices
            })
        },
        |notice: &ErrorNotice| format!("{:?}", notice.id),
        move |notice, _| {
            view()
                .m_2()
                .px_4()
                .py_3()
                .gap_2()
                .rounded(px(12.0))
                .border_1()
                .border_color(rgba(0xff8e8652))
                .bg(rgba(0xff8e8614))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            text(move || notice.with(|notice| notice.title.clone()))
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(ui::small_icon_button(
                            Icon::CLOSE,
                            tr("閉じる"),
                            move || {
                                connection.dispatch(Intent::DismissError {
                                    id: notice.with_untracked(|notice| notice.id.clone()),
                                });
                            },
                        )),
                )
                .child(
                    text(move || notice.with(|notice| notice.text.to_string()))
                        .selectable(true)
                        .text_size(px(13.0))
                        .text_color(colors::muted()),
                )
                .child(when(
                    move || notice.with(|notice| notice.recovery_label.is_some()),
                    move || {
                        view().flex_row().child(ui::text_button(
                            move || {
                                SharedString::from(notice.with(|notice| {
                                    notice.recovery_label.clone().unwrap_or_default()
                                }))
                            },
                            move || {
                                connection.dispatch(Intent::RecoverError {
                                    id: notice.with_untracked(|notice| notice.id.clone()),
                                });
                            },
                        ))
                    },
                ))
        },
    )
}
