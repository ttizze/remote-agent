//! The new-thread hero: "What should we build in {project}?" over a composer
//! centred in the chat column.
use super::{Desktop, on_click};
use crate::app::{
    sidebar::project_mark,
    ui::{color, icon},
};
use agent_core::{
    state::Intent,
    view::composer::hero::{DraftHeroHeadline, DraftHeroHeadlineKind},
};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

impl Desktop {
    pub(super) fn render_draft_hero(
        &mut self,
        hero: &DraftHeroHeadline,
        stack: Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut picker = || self.hero_project_picker(hero, cx);
        let heading = h_flex()
            .w_full()
            .flex_wrap()
            .justify_center()
            .items_center()
            .gap_x(px(8.))
            .text_size(px(30.))
            .line_height(px(36.))
            .text_color(color("text"))
            .map(|heading| match hero.kind {
                DraftHeroHeadlineKind::WorkOn => heading.child("What should we work on?"),
                DraftHeroHeadlineKind::BuildIn => heading
                    .child("What should we build in")
                    .child(h_flex().child(picker()).child("?")),
                DraftHeroHeadlineKind::ChooseProject => heading.child(picker()).child("to start"),
                DraftHeroHeadlineKind::AddProject => heading.child("Add a project to start"),
            });
        let second_line = hero.reserve_second_line.then(|| {
            h_flex()
                .mt_2()
                .h(px(24.))
                .justify_center()
                .items_center()
                .text_sm()
                .map(|line| {
                    if hero.picker_below_heading {
                        line.child(self.hero_project_picker(hero, cx))
                    } else if hero.start_without_project {
                        line.child(
                            div()
                                .id("start-without-project")
                                .cursor_pointer()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(color("textMuted"))
                                .hover(|link| link.text_color(color("text")).underline())
                                .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                    view.perform(Intent::SetNewThreadProject { project_id: None });
                                }))
                                .child("or start without a project"),
                        )
                    } else {
                        line
                    }
                })
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .justify_center()
            .px(px(20.))
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(
                        v_flex()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(relative(1.))
                            .pb_4()
                            .items_center()
                            .child(heading)
                            .children(second_line),
                    )
                    .child(stack),
            )
            .into_any_element()
    }

    /// The project the draft starts in, chosen inside the headline.
    fn hero_project_picker(&self, hero: &DraftHeroHeadline, cx: &mut Context<Self>) -> AnyElement {
        let base = Button::new("hero-project")
            .ghost()
            .h_auto()
            .p_0()
            .text_size(px(30.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color("text"))
            .border_b_1()
            .border_dashed()
            .border_color(color("text").opacity(0.3))
            .label(hero.project_label.clone())
            .when_some(hero.project_tooltip.clone(), |button, tooltip| {
                button.tooltip(tooltip)
            });
        if !hero.project_menu {
            return base
                .text_color(color("textMuted").opacity(0.6))
                .on_click(
                    cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.open_add_project(window, cx)
                    }),
                )
                .into_any_element();
        }
        let view = cx.entity().downgrade();
        let images: std::collections::HashMap<String, Option<std::sync::Arc<Image>>> = hero
            .project_choices
            .iter()
            .map(|choice| {
                (
                    choice.project_id.clone(),
                    self.project_icon_image(&choice.project_id),
                )
            })
            .collect();
        let hero = hero.clone();
        base.dropdown_menu_with_anchor(Anchor::TopCenter, move |mut menu, _, _| {
            if hero.no_project_choice {
                menu = menu.item(
                    PopupMenuItem::new("No project")
                        .icon(icon("message-square-dashed"))
                        .checked(hero.no_project_selected)
                        .on_click(on_click(&view, |view, _, _| {
                            view.perform(Intent::SetNewThreadProject { project_id: None });
                        })),
                );
            }
            for choice in &hero.project_choices {
                let project = choice.project_id.clone();
                let (name, image) = (
                    choice.name.clone(),
                    images.get(&choice.project_id).cloned().flatten(),
                );
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .gap_2()
                            .child(project_mark(image.clone(), &name, 16.))
                            .child(name.clone())
                    })
                    .checked(choice.selected)
                    .on_click(on_click(&view, move |view, _, _| {
                        view.perform(Intent::SetNewThreadProject {
                            project_id: Some(project.clone()),
                        });
                    })),
                );
            }
            menu.separator().item(
                PopupMenuItem::new("Add project")
                    .icon(icon("folder-plus"))
                    .on_click(on_click(&view, |view, window, cx| {
                        view.open_add_project(window, cx)
                    })),
            )
        })
        .into_any_element()
    }
}
