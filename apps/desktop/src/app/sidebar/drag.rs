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
pub(super) struct ThreadDrag {
    pub(super) id: String,
    pub(super) section: SidebarSection,
    pub(super) title: String,
}

pub(super) struct DragState {
    pub(super) active: String,
    pub(super) from: SidebarSection,
    /// The section the drop would land in.
    pub(super) target: Option<SidebarSection>,
    over: Option<String>,
    preview: Entity<DragPreview>,
}

/// The lifted row under the pointer.
pub(super) struct DragPreview {
    title: String,
    verb: Option<SidebarDropVerb>,
}
impl DragPreview {
    pub(super) fn new(title: String) -> Self {
        Self { title, verb: None }
    }
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

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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
            .when_some(self.verb, |row, verb| {
                let (glyph, label) = verb_badge(verb);
                row.child(
                    ui::text_2xs(h_flex())
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
                        .child(label),
                )
            })
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
            over: None,
            preview,
        });
        cx.notify();
    }

    /// The pointer moved over the list item `over_id` while dragging.
    pub(super) fn drag_over(&mut self, over_id: &str, cx: &mut Context<Self>) {
        let items = self.views.sidebar.list_items();
        let Some(drag) = self.sidebar.drag.as_mut() else {
            return;
        };
        if drag.over.as_deref() == Some(over_id) {
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
