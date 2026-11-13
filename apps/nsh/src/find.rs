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
    async fn reveal_at(self, view: ElementRef, key: &str) -> bool {
        if self.key().as_deref() != Some(key) {
            return false;
        }
        let Some(timeline) = self.timeline.with(|timeline| *timeline) else {
            return false;
        };
        let Some(bounds) = view.measure().await else {
            return false;
        };
        if self.key().as_deref() != Some(key) {
            return false;
        }
        timeline.reveal_text(bounds.page.y.0).await;
        self.key().as_deref() == Some(key)
    }

    pub(crate) fn reset_anchor(self) {
        self.last_anchor.replace(None);
    }

    /// The standard TextView's inline match is revealed by NSH. Remember it
    /// only after measuring succeeds, so a culled attempt cannot suppress it.
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
            if self.reveal_at(view, &key).await {
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
