//! Find in the loaded conversation, using the renderer's text documents and
//! Core's shared match ranges. The timeline and horizontal scrolls reveal the hit.
use crate::{
    composer::shortcut,
    conversation::Timeline,
    l10n::tr,
    ui::{self, Icon, colors},
};
use agent_core::presentation::conversation::{TextMatch, find_matches};
use nsh_core::{keymap::When, prelude::*};

/// A horizontal scroll and the direct child whose page position gives its offset.
#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    view: ElementRef,
    content: ElementRef,
}

/// Immutable enclosing scroll owners, outermost first, scoped to their views.
pub(crate) type Viewports = Option<Stored<Vec<Viewport>>>;

pub(crate) fn enclosing(parent: Viewports, view: ElementRef, content: ElementRef) -> Viewports {
    let mut owners = parent.map(|parent| parent.get()).unwrap_or_default();
    owners.push(Viewport { view, content });
    Some(stored(owners))
}

/// Enclosing spans are measured before scrolling, innermost first. Carrying
/// each inner movement into the hit keeps nested tables/code in the same space.
fn horizontal_offsets(
    viewports: impl IntoIterator<Item = (std::ops::Range<f32>, std::ops::Range<f32>)>,
    mut hit: std::ops::Range<f32>,
) -> Vec<Option<f32>> {
    viewports
        .into_iter()
        .map(|(viewport, content)| {
            let width = viewport.end - viewport.start;
            if width <= 0.0 {
                return None;
            }
            let offset = viewport.start - content.start;
            // A drawn zero-width anchor marks the first glyph: leave the viewport
            // after its leading edge for the match. Native text reports its width.
            let end = if hit.end > hit.start {
                hit.end
            } else {
                hit.start + width
            };
            let delta = nsh_core::scroll::reveal_delta(
                px(hit.start - viewport.start),
                px(end - viewport.start),
                px(width),
            )
            .0;
            let next = (offset + delta).clamp(0.0, (content.end - content.start - width).max(0.0));
            if next == offset {
                return None;
            }
            let moved = next - offset;
            hit.start -= moved;
            hit.end -= moved;
            Some(next)
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    row: String,
    document: usize,
    range: TextMatch,
}

#[derive(Clone, Copy)]
pub(crate) struct Find {
    open: Signal<bool>,
    query: Signal<String>,
    /// Index and navigation serial: Next must also re-reveal a sole match.
    current: Signal<(usize, u64)>,
    selected: Signal<Option<(u64, Match)>>,
    timeline: Stored<Option<Timeline>>,
    // Rendering the same hit again must not undo a reader's manual scroll.
    #[cfg(any(feature = "standard-text", all(target_os = "macos", debug_assertions)))]
    last_anchor: Stored<Option<String>>,
}

impl Find {
    pub(crate) fn new() -> Self {
        Self {
            open: signal(false),
            query: signal(String::new()),
            current: signal((0, 0)),
            selected: signal(None),
            timeline: stored(None),
            #[cfg(any(feature = "standard-text", all(target_os = "macos", debug_assertions)))]
            last_anchor: stored(None),
        }
    }
    pub(crate) fn highlighted(self) -> String {
        if self.open.get() {
            self.query.get()
        } else {
            String::new()
        }
    }
    pub(crate) fn reset_position(self) {
        self.current
            .update(|position| *position = (0, position.1.wrapping_add(1)));
        self.selected.set(None);
    }
    fn close(self) {
        self.open.set(false);
        self.query.set(String::new());
        self.reset_position();
    }
    pub(crate) fn selection(self, row: &str) -> Option<(usize, TextMatch)> {
        self.selected.with(|selected| {
            selected
                .as_ref()
                .filter(|(_, selected)| selected.row == row)
                .map(|(_, selected)| (selected.document, selected.range))
        })
    }
    pub(crate) fn active(self, row: &str, document: usize) -> Option<TextMatch> {
        self.selection(row)
            .filter(|(index, _)| *index == document)
            .map(|(_, range)| range)
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn selected_row(self, row: &str) -> bool {
        self.selected.with(|selected| {
            selected
                .as_ref()
                .is_some_and(|(_, selected)| selected.row == row)
        })
    }
    pub(crate) fn key(self) -> Option<String> {
        let (navigation, selected) = self.selected.get()?;
        Some(
            serde_json::to_string(&(
                navigation,
                selected.row,
                selected.document,
                selected.range.start,
                selected.range.end,
                self.highlighted(),
            ))
            .expect("search identity serializes"),
        )
    }
    /// Native glyphs or a drawn inline attachment report the selected text's
    /// actual position. Ignore measurements belonging to an earlier match.
    pub(crate) fn reveal(
        self,
        view: ElementRef,
        rect: Bounds<Points>,
        key: String,
        parents: Viewports,
    ) {
        let parents = parents.map(|parents| parents.get()).unwrap_or_default();
        spawn(async move {
            self.reveal_at(view, rect, key, parents).await;
        });
    }

    async fn reveal_at(
        self,
        view: ElementRef,
        rect: Bounds<Points>,
        key: String,
        parents: Vec<Viewport>,
    ) -> bool {
        if self.key().as_ref() != Some(&key) {
            return false;
        }
        let Some(timeline) = self.timeline.with(|timeline| *timeline) else {
            return false;
        };
        let Some(bounds) = view.measure().await else {
            return false;
        };
        let x = bounds.page.x.0 + rect.origin.x.0;
        let mut measured = Vec::new();
        for parent in parents.into_iter().rev() {
            let (Some(viewport), Some(content)) =
                (parent.view.measure().await, parent.content.measure().await)
            else {
                continue;
            };
            if self.key().as_ref() != Some(&key) {
                return false;
            }
            measured.push((
                parent.view,
                viewport.page.x.0..viewport.page.x.0 + viewport.frame.size.width.0,
                content.page.x.0..content.page.x.0 + content.frame.size.width.0,
            ));
        }
        let offsets = horizontal_offsets(
            measured
                .iter()
                .map(|(_, viewport, content)| (viewport.clone(), content.clone())),
            x..x + rect.size.width.0,
        );
        for ((view, _, _), offset) in measured.into_iter().zip(offsets) {
            if let Some(offset) = offset {
                view.scroll_to(point(px(offset), px(0.0)), false);
            }
        }
        if self.key().as_ref() != Some(&key) {
            return false;
        }
        timeline
            .reveal_text(bounds.page.y.0 + rect.origin.y.0)
            .await;
        self.key().as_ref() == Some(&key)
    }

    #[cfg(any(feature = "standard-text", all(target_os = "macos", debug_assertions)))]
    pub(crate) fn reset_anchor(self) {
        self.last_anchor.replace(None);
    }

    /// The standard TextView's inline match is revealed by NSH. Remember it
    /// only after measuring succeeds, so a culled attempt cannot suppress it.
    #[cfg(any(feature = "standard-text", all(target_os = "macos", debug_assertions)))]
    pub(crate) fn reveal_anchor(self, view: ElementRef, key: String) {
        if self.key().as_ref() != Some(&key)
            || self.last_anchor.with(|last| last.as_ref() == Some(&key))
        {
            return;
        }
        view.reveal(
            nsh_core::RevealOptions::default()
                .horizontal(nsh_core::ScrollAlignment::Start)
                .vertical(nsh_core::ScrollAlignment::Nearest)
                .animated(false),
        );
        spawn(async move {
            if self
                .reveal_at(
                    view,
                    Bounds::new(point(px(0.0), px(0.0)), size(px(0.0), px(1.0))),
                    key.clone(),
                    Vec::new(),
                )
                .await
            {
                self.last_anchor.replace(Some(key));
            }
        });
    }
}

fn occurrences(
    rows: Vec<(usize, String, String)>,
    query: &str,
    cwd: &str,
    home: Option<&str>,
) -> Vec<(usize, Match)> {
    let mut matches = Vec::new();
    if query.trim().is_empty() {
        return matches;
    }
    for (row, id, source) in rows {
        for (document, text) in
            crate::rich_text::search_documents(source, cwd.into(), home.map(str::to_owned))
                .into_iter()
                .enumerate()
        {
            for range in find_matches(&text, query) {
                matches.push((
                    row,
                    Match {
                        row: id.clone(),
                        document,
                        range,
                    },
                ));
            }
        }
    }
    matches
}

pub(crate) fn bar(find: Find, timeline: Timeline) -> impl IntoElement {
    find.timeline.replace(Some(timeline));
    let terminal = use_context::<crate::terminal::Focus>().unwrap();
    view()
        .absolute()
        .top(px(12.0))
        .right(px(16.0))
        .child(when(
            move || !terminal.get(),
            move || shortcut("mod+f", When::Always, move |_| find.open.set(true)),
        ))
        .child(when(
            move || find.open.get(),
            move || open_bar(find, timeline),
        ))
}

fn open_bar(find: Find, timeline: Timeline) -> View {
    let field = element_ref();
    let focused = signal(false);
    let connection = use_context::<crate::Connection>().unwrap();
    let matches = computed(move || {
        occurrences(
            timeline.message_texts(),
            &find.query.get(),
            &connection
                .composer
                .with(|p| p.controls.navigation.cwd.clone()),
            connection.context.with(|p| p.home()).as_deref(),
        )
    });
    let count = move || matches.with(Vec::len);
    let current = move || find.current.get().0.min(count().saturating_sub(1));
    let step = move |by: isize| {
        let total = count() as isize;
        if total > 0 {
            let at = (current() as isize + by).rem_euclid(total) as usize;
            find.current
                .update(|position| *position = (at, position.1.wrapping_add(1)));
        }
    };
    effect(move || {
        let at = current();
        if find.current.get_untracked().0 != at {
            find.current.update(|position| position.0 = at);
        }
        let navigation = find.current.get().1;
        let next = matches.with(|matches| matches.get(at).cloned());
        let selected = next
            .as_ref()
            .map(|(_, selected)| (navigation, selected.clone()));
        let previous = find.selected.get_untracked();
        if previous != selected {
            if let Some((row, _)) = &next {
                timeline.reveal(*row);
            }
            find.selected.set(selected);
        }
    });
    after_answer(move || field.focus());
    view()
        .w(px(280.0))
        .h(px(36.0))
        .flex_row()
        .items_center()
        .gap_1()
        .px_3()
        .rounded_full()
        .border_1()
        .border_color(colors::border())
        .bg(colors::SURFACE)
        .child(shortcut("escape", When::Always, move |_| find.close()))
        .child(when(
            move || focused.get(),
            move || shortcut("shift+enter", When::Always, move |_| step(-1)),
        ))
        .child(
            ui::field()
                .element_ref(&field)
                .flex_1()
                .min_w_0()
                .text_size(px(14.0))
                .value(move || find.query.get())
                .placeholder(tr("会話内を検索"))
                .submit_behavior(SubmitBehavior::Submit)
                .on_focus(move || focused.set(true))
                .on_blur(move || focused.set(false))
                .on_submit(move |_: &str| step(1))
                .on_change(move |text: &str| {
                    find.query.set(text.into());
                    find.current
                        .update(|position| *position = (0, position.1.wrapping_add(1)));
                }),
        )
        .child(
            text(move || match count() {
                0 => String::new(),
                total => format!("{}/{total}", current() + 1),
            })
            .text_size(px(12.0))
            .text_color(colors::muted()),
        )
        .child(ui::icon_button(
            Icon::CHEVRON_UP,
            tr("前の一致"),
            move || step(-1),
        ))
        .child(ui::icon_button(
            Icon::CHEVRON_DOWN,
            tr("次の一致"),
            move || step(1),
        ))
        .child(ui::icon_button(Icon::CLOSE, tr("閉じる"), move || {
            find.close()
        }))
}

#[cfg(test)]
mod tests {
    use super::horizontal_offsets;

    #[test]
    fn find_reveals_hidden_table_columns_and_unwrapped_code() {
        for (viewport, content, hit, expected) in [
            (100.0..400.0, 100.0..1600.0, 1100.0..1180.0, Some(796.0)),
            // A drawn code anchor has no width: leave room after its leading edge.
            (100.0..400.0, 100.0..1600.0, 1100.0..1100.0, Some(984.0)),
            // An enclosing table may already have moved this code viewport left of the window.
            (-200.0..100.0, -200.0..1300.0, -180.0..-180.0, Some(4.0)),
            // Previous also reveals a match to the left of an already scrolled view.
            (100.0..400.0, -400.0..1100.0, -100.0..-60.0, Some(284.0)),
            (100.0..400.0, -400.0..1100.0, 150.0..190.0, None),
            (100.0..400.0, 100.0..1600.0, 1590.0..1600.0, Some(1200.0)),
            (100.0..400.0, 100.0..300.0, 150.0..190.0, None),
            (100.0..100.0, 100.0..1600.0, 1100.0..1180.0, None),
            (100.0..99.0, 100.0..1600.0, 1100.0..1180.0, None),
        ] {
            assert_eq!(horizontal_offsets([(viewport, content)], hit), [expected]);
        }
    }

    #[test]
    fn a_code_match_inside_a_wide_table_is_revealed_in_both_viewports() {
        for (content, hit, expected) in [
            (600.0..1600.0, 1500.0..1540.0, [Some(756.0), Some(400.0)]),
            // The same target is reached from an existing inner scroll position.
            (400.0..1400.0, 1300.0..1340.0, [Some(756.0), Some(400.0)]),
            // Going back can move the inner content right while its table moves left.
            (100.0..1100.0, 400.0..440.0, [Some(284.0), Some(272.0)]),
        ] {
            assert_eq!(
                horizontal_offsets(
                    [(600.0..800.0, content), (100.0..400.0, 100.0..1100.0),],
                    hit
                ),
                expected
            );
        }
    }
}
