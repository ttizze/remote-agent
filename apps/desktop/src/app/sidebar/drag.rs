//! Dragging a row onto another row or a section boundary: the lifted card,
//! the verb it would perform, and the drop the view model plans.
use super::super::{
    Desktop,
    ui::{self, color, icon, tint},
};
use agent_core::{
    state::Intent,
    view::sidebar::{
        SidebarDropVerb, SidebarSection, SidebarThreadDropPlan, resolve_sidebar_drop_target,
        resolve_sidebar_drop_verb,
    },
};
use gpui_kit::{component::h_flex, prelude::FluentBuilder, *};

/// The dragged row.
#[derive(Clone)]
pub(crate) struct ThreadDrag {
    pub(super) id: String,
    pub(super) section: SidebarSection,
    pub(super) title: String,
}

pub(super) struct DragState {
    pub(super) active: String,
    pub(super) from: SidebarSection,
    /// The section the drop would land in.
    pub(super) target: Option<SidebarSection>,
    /// The pointer left the list: the threads go to a composer as context.
    pub(super) context: bool,
    over: Option<String>,
    preview: Entity<DragPreview>,
}

/// The lifted row under the pointer, or the threads' chip once it left the list.
pub(super) struct DragPreview {
    title: String,
    /// Where the row was grabbed, from its top left corner.
    grab: Point<Pixels>,
    verb: Option<SidebarDropVerb>,
    /// How many threads go along as context, while outside the list.
    context: Option<usize>,
}
impl DragPreview {
    pub(super) fn new(title: String, grab: Point<Pixels>) -> Self {
        Self {
            title,
            grab,
            verb: None,
            context: None,
        }
    }
}

/// The chip that follows the pointer while threads are dragged to a composer.
fn context_ghost(title: &str, count: usize) -> Div {
    let title = if title.trim().is_empty() {
        "Thread".to_owned()
    } else {
        title.trim().to_owned()
    };
    h_flex()
        .max_w(px(288.))
        .gap_2()
        .rounded(px(8.))
        .border_1()
        .border_color(color("border"))
        .bg(color("surfaceOverlay"))
        .px_3()
        .py_2()
        .text_sm()
        .text_color(color("text"))
        .shadow_lg()
        .child(
            icon("messages-square")
                .size_4()
                .flex_shrink_0()
                .text_color(color("secondaryLabel")),
        )
        .child(div().min_w_0().truncate().child(title))
        .when(count > 1, |ghost| {
            ghost.child(
                div()
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(color("muted"))
                    .px_1p5()
                    .text_xs()
                    .text_color(color("secondaryLabel"))
                    .child(format!("+{}", count - 1)),
            )
        })
}

fn verb_badge(verb: SidebarDropVerb) -> (&'static str, &'static str) {
    match verb {
        SidebarDropVerb::Pin => ("pin", "Pin"),
        SidebarDropVerb::Unpin => ("pin-off", "Unpin"),
        SidebarDropVerb::Settle => ("circle-check", "Settle"),
        SidebarDropVerb::Unsettle => ("undo-2", "Un-settle"),
        SidebarDropVerb::Wake => ("alarm-clock-off", "Wake"),
    }
}

/// The action a drop or sweep performs, as a badge at the row's end.
pub(super) fn verb_badge_element(verb: SidebarDropVerb) -> Div {
    let (glyph, label) = verb_badge(verb);
    ui::text_2xs(h_flex())
        .ml_auto()
        .flex_shrink_0()
        .h_5()
        .gap_1()
        .px_1p5()
        .rounded(px(4.))
        .border_1()
        .border_color(tint("accent", 0.4))
        .bg(tint("accent", 0.1))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color("accent"))
        .child(icon(glyph).size_3())
        .child(label)
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(count) = self.context {
            // The chip sits just below and right of the pointer.
            return div()
                .pl(self.grab.x + px(12.))
                .pt(self.grab.y + px(12.))
                .child(context_ghost(&self.title, count))
                .into_any_element();
        }
        h_flex()
            .w(px(ui::metrics().sidebar_width - 16.))
            .h_9()
            .px(px(10.))
            .gap_2p5()
            .rounded(px(8.))
            .bg(color("sidebarRowActive"))
            .border_1()
            .border_color(color("sidebarBorder"))
            .shadow_lg()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color("sidebarForeground"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.title.clone()),
            )
            .when_some(self.verb, |row, verb| row.child(verb_badge_element(verb)))
            .into_any_element()
    }
}

impl Desktop {
    pub(super) fn start_thread_drag(
        &mut self,
        drag: &ThreadDrag,
        preview: Entity<DragPreview>,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.drag = Some(DragState {
            active: drag.id.clone(),
            from: drag.section,
            target: None,
            context: false,
            over: None,
            preview,
        });
        cx.notify();
    }

    /// The dragged threads: the selection when the lifted row is part of it.
    pub(super) fn dragged_threads(&self, drag: &ThreadDrag) -> Vec<String> {
        let selected = &self.sidebar.selection.selected;
        if !selected.contains(&drag.id) {
            return vec![drag.id.clone()];
        }
        selected.clone()
    }

    /// Past the list's left or right edge the drag carries the threads to a
    /// composer instead of moving the row.
    pub(super) fn drag_across_list_edge(&mut self, outside: bool, cx: &mut Context<Self>) {
        let count = self
            .sidebar
            .drag
            .as_ref()
            .filter(|drag| drag.context != outside)
            .map(|drag| {
                let selected = &self.sidebar.selection.selected;
                if selected.contains(&drag.active) {
                    selected.len()
                } else {
                    1
                }
            });
        let (Some(count), Some(drag)) = (count, self.sidebar.drag.as_mut()) else {
            return;
        };
        drag.context = outside;
        drag.over = None;
        drag.target = None;
        drag.preview.update(cx, |preview, cx| {
            preview.verb = None;
            preview.context = outside.then_some(count);
            cx.notify();
        });
        cx.notify();
    }

    /// Threads dropped on the composer join its draft at the caret.
    pub(crate) fn drop_threads_on_composer(&mut self, drag: &ThreadDrag, cx: &mut Context<Self>) {
        self.sidebar.drag = None;
        let thread_ids = self.dragged_threads(drag);
        if let Some((text, cursor)) = self.composer_caret(cx) {
            self.perform(Intent::AddThreadContexts {
                text,
                cursor,
                thread_ids,
            });
        }
        cx.notify();
    }

    /// The pointer moved over the list item `over_id` while dragging.
    pub(super) fn drag_over(&mut self, over_id: &str, cx: &mut Context<Self>) {
        let items = self.views.sidebar.list_items();
        let Some(drag) = self.sidebar.drag.as_mut() else {
            return;
        };
        if drag.context || drag.over.as_deref() == Some(over_id) {
            return;
        }
        drag.over = Some(over_id.to_owned());
        drag.target = resolve_sidebar_drop_target(&items, &drag.active, over_id)
            .map(|target| SidebarSection::from(target.section));
        let verb = resolve_sidebar_drop_verb(drag.from, drag.target);
        drag.preview.update(cx, |preview, cx| {
            if preview.verb != verb {
                preview.verb = verb;
                cx.notify();
            }
        });
        cx.notify();
    }

    /// Drops the dragged row on `over_id` as the view model plans it.
    pub(super) fn drop_thread(&mut self, drag: &ThreadDrag, over_id: &str, cx: &mut Context<Self>) {
        self.sidebar.drag = None;
        let plan = self.snapshot.sidebar_drop(
            ui::now_ms(),
            self.sidebar.options(),
            drag.id.clone(),
            over_id.to_owned(),
        );
        if plan != SidebarThreadDropPlan::None {
            self.perform(Intent::DropThread {
                thread_id: drag.id.clone(),
                plan,
            });
        }
        cx.notify();
    }

    /// Makes `element` a drop slot named `over_id`.
    pub(super) fn drop_slot<E: InteractiveElement + 'static>(
        &self,
        element: E,
        over_id: String,
        cx: &mut Context<Self>,
    ) -> E {
        let hover_id = over_id.clone();
        element
            .on_drag_move::<ThreadDrag>(cx.listener(
                move |view, event: &DragMoveEvent<ThreadDrag>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        view.drag_over(&hover_id, cx);
                    }
                },
            ))
            .on_drop::<ThreadDrag>(cx.listener(move |view, drag: &ThreadDrag, _, cx| {
                view.drop_thread(drag, &over_id, cx)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{SidebarDropVerb, verb_badge};

    #[test]
    fn each_drop_verb_reads_as_its_row_action() {
        assert_eq!(
            verb_badge(SidebarDropVerb::Unsettle),
            ("undo-2", "Un-settle")
        );
        assert_eq!(
            verb_badge(SidebarDropVerb::Wake),
            ("alarm-clock-off", "Wake")
        );
        assert_eq!(verb_badge(SidebarDropVerb::Pin), ("pin", "Pin"));
    }
}
