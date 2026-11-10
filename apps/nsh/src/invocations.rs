use crate::Connection;
use crate::ui::{self, Icon, colors};
use agent_core::{
    composer::{CompletionKey, candidate_label, completion_key, insert_invocation},
    state::Intent,
};
use agent_protocol::composer::{ComposerCandidate, InvocationKind};
use nsh_core::prelude::*;

/// Catalog filtering, insertion and what keys do remain core decisions; the list
/// is T3 Code's command menu above the composer, whose keys work it while the
/// field keeps the focus, as the desktop client's.
pub(crate) fn suggestions(focus: impl Fn() + Copy + 'static) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let draft = computed(move || connection.composer.with(|p| p.draft.text.clone()));
    // Each edit of the draft lists from the top again and reopens a closed list.
    let highlighted = signal(0_usize);
    let dismissed = signal(false);
    effect(move || {
        draft.with(|_| ());
        highlighted.set(0);
        dismissed.set(false);
    });
    let suggestions = computed(move || {
        if dismissed.get() {
            return None;
        }
        connection.composer.with(|p| {
            let text = p.draft.text.clone();
            let cursor = text.len() as u32;
            p.composer_suggestions(text, cursor)
        })
    });
    let candidates =
        move || suggestions.with(|s| s.as_ref().map(|s| s.candidates.clone()).unwrap_or_default());
    let insert = move |invocation: agent_protocol::composer::Invocation| {
        let (thread_id, text) = connection.publication.with_untracked(|p| {
            (
                p.composer.controls.navigation.draft_key.clone(),
                p.composer.draft.text.clone(),
            )
        });
        if let Some(insertion) = insert_invocation(
            text.clone(),
            text.len() as u32,
            invocation.kind,
            invocation.name.clone(),
        ) {
            connection.dispatch(Intent::InsertInvocation {
                thread_id,
                text: insertion.text,
                invocation,
            });
            focus();
        }
    };
    when(
        move || suggestions.with(Option::is_some),
        move || {
            view()
                .gap_1()
                .child(
                    text(move || {
                        suggestions.with(|s| {
                            s.as_ref()
                                .and_then(|s| s.status.clone())
                                .unwrap_or_default()
                        })
                    })
                    .text_size(px(12.0))
                    .text_color(colors::MUTED),
                )
                .child(when(
                    move || {
                        suggestions.with(|s| s.as_ref().is_some_and(|s| !s.candidates.is_empty()))
                    },
                    move || {
                        if is_desktop() {
                            for (key, repeat) in [
                                ("up", true),
                                ("down", true),
                                ("return", false),
                                ("tab", false),
                                ("escape", false),
                            ] {
                                on_key_with(
                                    key,
                                    KeyOptions {
                                        when: nsh_core::keymap::When::Always,
                                        repeat,
                                        ..Default::default()
                                    },
                                    move |_| {
                                        let listed = candidates();
                                        match completion_key(
                                            key,
                                            highlighted.get_untracked(),
                                            listed.len(),
                                        ) {
                                            Some(CompletionKey::Highlight(index)) => {
                                                highlighted.set(index)
                                            }
                                            Some(CompletionKey::Insert(index)) => {
                                                insert(listed[index].invocation.clone())
                                            }
                                            Some(CompletionKey::Dismiss) => dismissed.set(true),
                                            None => {}
                                        }
                                    },
                                );
                            }
                        }
                        ui::card(vec![
                            view().child(scroll().max_h(px(4.0 * 40.0 + 8.0)).child(
                                view().p_1().child(each(
                                    candidates,
                                    |candidate: &ComposerCandidate| {
                                        candidate.invocation.path.clone()
                                    },
                                    move |candidate, index| {
                                        row(candidate.get_untracked(), insert).style_when(
                                            move || index.get() == highlighted.get(),
                                            |s| s.bg(colors::SELECTED),
                                        )
                                    },
                                )),
                            )),
                        ])
                    },
                ))
        },
    )
}

/// A skill or plugin: its icon, name and description.
fn row(
    candidate: ComposerCandidate,
    insert: impl Fn(agent_protocol::composer::Invocation) + 'static,
) -> nsh_base::ButtonElement {
    let ComposerCandidate {
        invocation,
        description,
    } = candidate;
    let name = candidate_label(&invocation.name);
    let glyph = if invocation.kind == InvocationKind::Skill {
        Icon::SPARKLES
    } else {
        Icon::PUZZLE
    };
    nsh_base::button()
        .accessibility_label(Some(SharedString::from(name.clone())))
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(40.0))
        .px_2()
        .rounded(px(ui::RADIUS))
        .text_color(colors::FOREGROUND)
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .on_press(move || insert(invocation.clone()))
        .child(
            view()
                .text_color(colors::MUTED)
                .child(ui::icon(glyph, 16.0)),
        )
        .child(
            text(name)
                .text_size(px(14.0))
                .number_of_lines(1)
                .flex_shrink_0(),
        )
        .child(
            text(candidate_label(&description))
                .text_size(px(13.0))
                .text_color(colors::MUTED)
                .number_of_lines(1)
                .flex_1()
                .min_w_0(),
        )
}
