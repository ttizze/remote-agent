//! The `/`, `$` and `@` menu over the composer for the trigger at the caret.
use super::{Desktop, banners::Piece};
use crate::app::ui::{color, icon};
use agent_core::{
    state::Intent,
    view::{
        composer::{
            commands::{ComposerCommandTarget, ComposerTriggerKind, SkillSourceKind},
            menu::ComposerMenuView,
        },
        timeline::rows::TimelineLayout,
    },
};
use gpui_kit::{component::h_flex, prelude::FluentBuilder, *};

#[derive(Default)]
pub(super) struct MenuState {
    highlight: usize,
    /// Escape closed the menu until the text changes.
    dismissed: bool,
    /// The text and caret the menu's sources were last asked for.
    requested: Option<(String, u32)>,
}
impl MenuState {
    pub(super) fn text_changed(&mut self) {
        self.highlight = 0;
        self.dismissed = false;
    }
}

impl Desktop {
    /// Asks for what the menu lists whenever the composer's text or caret
    /// moved, and when the composer appears.
    pub(super) fn sync_composer_menu(&mut self, cx: &App) {
        if self.shown_composer().is_none() {
            self.composer.menu.requested = None;
            return;
        }
        let (text, cursor) = self.editor_text_and_cursor(cx);
        if self.composer.menu.requested.as_ref() == Some(&(text.clone(), cursor)) {
            return;
        }
        self.composer.menu.requested = Some((text.clone(), cursor));
        self.perform(Intent::UpdateComposerMenu {
            text,
            cursor,
            layout: TimelineLayout::Desktop,
        });
    }

    /// The menu for the editor's text and caret, while a trigger is open.
    fn composer_menu_state(&self, cx: &App) -> Option<(String, u32, ComposerMenuView)> {
        if self.composer.menu.dismissed
            || self
                .shown_composer()
                .is_none_or(|composer| composer.editor.disabled)
        {
            return None;
        }
        let (text, cursor) = self.editor_text_and_cursor(cx);
        let menu = self.snapshot.composer_menu(text.clone(), cursor);
        menu.trigger.is_some().then_some((text, cursor, menu))
    }

    /// Chooses the highlighted item; false when no menu item is offered.
    pub(super) fn select_composer_menu_item(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((text, cursor, menu)) = self.composer_menu_state(cx) else {
            return false;
        };
        let Some(item) = menu.items.get(
            self.composer
                .menu
                .highlight
                .min(menu.items.len().saturating_sub(1)),
        ) else {
            return false;
        };
        self.choose_composer_menu_item(text, cursor, item.id.clone());
        true
    }

    fn choose_composer_menu_item(&mut self, text: String, cursor: u32, item_id: String) {
        self.composer.menu.text_changed();
        self.perform(Intent::SelectComposerItem {
            text,
            cursor,
            item_id,
        });
    }

    /// Moves the highlight; false when no menu is open.
    pub(super) fn step_composer_menu(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let Some((_, _, menu)) = self.composer_menu_state(cx) else {
            return false;
        };
        let count = menu.items.len();
        if count > 0 {
            let current = self.composer.menu.highlight.min(count - 1);
            self.composer.menu.highlight = if down {
                (current + 1) % count
            } else {
                (current + count - 1) % count
            };
            cx.notify();
        }
        true
    }

    /// Escape closes the menu, then the stash; false when neither is open.
    pub(super) fn dismiss_composer_layer(&mut self, cx: &mut Context<Self>) -> bool {
        if self.composer_menu_state(cx).is_some() {
            self.composer.menu.dismissed = true;
        } else if self.composer.banners.stash_open {
            self.composer.banners.stash_open = false;
        } else {
            return false;
        }
        cx.notify();
        true
    }

    pub(super) fn composer_command_menu(&mut self, cx: &mut Context<Self>) -> Option<Piece> {
        let (text, cursor, menu) = self.composer_menu_state(cx)?;
        let highlight = self
            .composer
            .menu
            .highlight
            .min(menu.items.len().saturating_sub(1));
        let skill_trigger = menu
            .trigger
            .as_ref()
            .is_some_and(|trigger| trigger.kind == ComposerTriggerKind::Skill);
        let rows = menu.items.into_iter().enumerate().map(|(index, item)| {
            let source = item.skill_source.map(|source| {
                let glyph = match source {
                    SkillSourceKind::App => "blocks",
                    SkillSourceKind::Repo | SkillSourceKind::Project => "folder",
                    SkillSourceKind::Personal => "user-round",
                    SkillSourceKind::System => "settings",
                    SkillSourceKind::Other => "package",
                };
                let label = if skill_trigger {
                    format!("{} Skill", source.label())
                } else {
                    source.label().to_owned()
                };
                h_flex()
                    .flex_none()
                    .ml_auto()
                    .gap_1()
                    .px_1p5()
                    .rounded(px(4.))
                    .bg(color("secondary"))
                    .text_color(color("secondaryForeground"))
                    .text_size(px(11.))
                    .child(icon(glyph).size(px(12.)))
                    .child(label)
            });
            let leading = match &item.target {
                ComposerCommandTarget::Path { directory, .. } => {
                    Some(if *directory { "folder" } else { "file" })
                }
                ComposerCommandTarget::Thread { .. } => Some("message-square"),
                ComposerCommandTarget::PullRequest { .. } => Some("git-pull-request"),
                _ => None,
            };
            let (text, item_id) = (text.clone(), item.id.clone());
            h_flex()
                .id(SharedString::from(format!("composer-item-{}", item.id)))
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .rounded_sm()
                .px_2()
                .py(px(6.))
                .cursor_pointer()
                .when(index == highlight, |row| {
                    row.bg(color("accentSurface"))
                        .text_color(color("accentSurfaceForeground"))
                })
                .on_mouse_move(cx.listener(move |view, _: &MouseMoveEvent, _, cx| {
                    if view.composer.menu.highlight != index {
                        view.composer.menu.highlight = index;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.choose_composer_menu_item(text.clone(), cursor, item_id.clone())
                }))
                .when_some(leading, |row, name| {
                    row.child(icon(name).size(px(16.)).text_color(color("textMuted")))
                })
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.45))
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(item.label),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(item.description),
                )
                .children(source)
        });
        let content = match menu.empty_label {
            Some(label) => div()
                .px(px(20.))
                .pt(px(14.))
                .pb(px(28.))
                .text_xs()
                .text_color(color("textMuted"))
                .child(label)
                .into_any_element(),
            None => div()
                .id("composer-menu-items")
                .max_h(px(288.))
                .overflow_y_scroll()
                .p_1()
                .children(rows)
                .into_any_element(),
        };
        Some(Piece::new(content))
    }
}
