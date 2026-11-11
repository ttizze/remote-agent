use crate::l10n::tr;
use crate::ui::{self, colors};
use crate::{Connection, shared::Shared};
use agent_core::presentation::view::Presentation;
use nsh_core::prelude::*;

pub(crate) fn screen(id: u64, host: String) -> impl IntoElement {
    let parent = use_context::<Connection>().unwrap();
    let host = stored(host);
    let empty = stored(Shared(Presentation::empty(
        agent_core::localization::UiLanguage::default(),
    )));
    let publication = computed(move || {
        parent.state.with(|state| {
            state
                .side_chats
                .get(&id)
                .and_then(|pane| pane.publication.clone())
                .unwrap_or_else(|| empty.get())
        })
    });
    let pending = computed(move || {
        parent.state.with(|state| {
            state.side_chats.get(&id).is_none_or(|pane| {
                pane.pending || pane.publication.is_none() && pane.failure.is_none()
            })
        })
    });
    let failure = computed(move || {
        parent.state.with(|state| {
            state
                .side_chats
                .get(&id)
                .and_then(|pane| pane.failure.clone())
        })
    });
    provide_context(Connection::new(
        parent.handles(),
        computed(move || host.get()),
        publication,
        Some(id),
    ));
    view()
        .flex_1()
        .min_w_0()
        .child(when(
            move || pending.get(),
            move || {
                view()
                    .flex_1()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(colors::muted())
                    .child(ui::spinner())
                    .child(text(tr("サイドチャットを準備中…")).text_size(px(13.0)))
            },
        ))
        .child(when(
            move || failure.get().is_some(),
            move || {
                view()
                    .p_4()
                    .gap_3()
                    .child(
                        text(move || {
                            failure
                                .get()
                                .map(|error| error.to_string())
                                .unwrap_or_default()
                        })
                        .selectable(true)
                        .text_color(move || nsh_base::theme().destructive),
                    )
                    .child(
                        view()
                            .flex_row()
                            .child(ui::text_button(tr("再試行"), move || {
                                parent.application.with(|app| app.prepare_side_chat(id))
                            })),
                    )
            },
        ))
        .child(when(
            move || !pending.get() && failure.get().is_none(),
            move || {
                let following = signal(true);
                view()
                    .flex_1()
                    .min_w_0()
                    .child(crate::notices::notices(|| false, || false))
                    .child(crate::conversation::Timeline::new(following).list())
                    .child(crate::composer::composer(following))
            },
        ))
}
