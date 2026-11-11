//! The PC browser surface in NSH: the image fitted
//! to the panel; clicks and wheel scrolls at its page coordinates and, on a
//! desktop, typing after a click into it; the page's dialog; and the Host's
//! browser data to import.
use crate::l10n::{localized, tr, tr_format};
use crate::{native, shared::Shared, ui};
#[cfg(not(target_os = "macos"))]
use agent_core::browser_view;
use agent_core::presentation::conversation::ActionTone;
use agent_protocol::browser::{BrowserAction, BrowserCredentials, BrowserFrame, BrowserKey};
use nsh_core::{AutoCapitalize, keymap::When, prelude::*};

/// The keys a desktop page takes beside the text typed into it, as NSH's
/// keymap names them. Backspace and Enter come with the typed text.
const KEYS: [(&str, BrowserKey); 12] = [
    ("escape", BrowserKey::Escape),
    ("tab", BrowserKey::Tab),
    ("up", BrowserKey::ArrowUp),
    ("down", BrowserKey::ArrowDown),
    ("left", BrowserKey::ArrowLeft),
    ("right", BrowserKey::ArrowRight),
    ("delete", BrowserKey::Delete),
    ("home", BrowserKey::Home),
    ("end", BrowserKey::End),
    ("pageup", BrowserKey::PageUp),
    ("pagedown", BrowserKey::PageDown),
    ("mod+a", BrowserKey::SelectAll),
];

pub(super) fn page(
    image: Signal<String>,
    frame: Signal<Shared<BrowserFrame>>,
    accepts_input: Memo<bool>,
    answering: Signal<bool>,
    credentials: Signal<Option<BrowserCredentials>>,
    viewport: Signal<(f64, f64)>,
    send: impl Fn(BrowserAction) -> bool + Copy + 'static,
) -> AnyElement {
    // The page's own size, which clicks and scrolls are reported in.
    let page = move || frame.with(|f| (f.width, f.height));
    let scale = move || {
        let ((width, height), (shown_width, shown_height)) = (page(), viewport.get());
        if width == 0 || height == 0 {
            return 1.0;
        }
        (shown_width / f64::from(width))
            .min(shown_height / f64::from(height))
            .min(1.0)
    };
    let dialog = computed(move || {
        if answering.get() {
            None
        } else {
            frame.with(|f| f.dialog.clone())
        }
    });
    let browsing = move || credentials.with(Option::is_none);
    let field = element_ref();
    view()
        .flex_1()
        .min_h_0()
        .bg(ui::colors::SURFACE)
        .label(tr("ブラウザ"))
        .on_layout(move |bounds: &Bounds<Points>| {
            viewport.set((
                f64::from(bounds.size.width.0),
                f64::from(bounds.size.height.0),
            ))
        })
        .child(when(
            move || browsing() && image.with(String::is_empty),
            || {
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_4()
                    .child(text(tr("プレビューはまだありません")).text_size(px(15.0)))
                    .child(
                        text(tr("URLを入力するか、AIにブラウザ操作を依頼してください。"))
                            .text_color(ui::colors::muted()),
                    )
            },
        ))
        .child(when(
            move || browsing() && !image.with(String::is_empty),
            move || {
                let width = move || px((f64::from(page().0) * scale()) as f32);
                let height = move || px((f64::from(page().1) * scale()) as f32);
                #[cfg(target_os = "macos")]
                return view()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .child(
                        native("bex.browser.canvas")
                            .prop("data", move || {
                                serde_json::json!({
                                    "image": image.get(), "enabled": accepts_input.get()
                                })
                            })
                            .on_native_event(move |event: &NativeEvent| {
                                if let Ok(action) =
                                    serde_json::from_value::<BrowserAction>(event.data.clone())
                                {
                                    if matches!(action, BrowserAction::Click { .. }) {
                                        field.focus();
                                    }
                                    send(action);
                                }
                            })
                            .w(width)
                            .h(height),
                    )
                    .into_element();
                #[cfg(not(target_os = "macos"))]
                {
                    view()
                        .w(width)
                        .h(height)
                        .child(
                            nsh_core::element::image(move || {
                                format!("data:image/jpeg;base64,{}", image.get())
                            })
                            .w(width)
                            .h(height),
                        )
                        .child(
                            native::wheel_area(move |wheel: native::Scroll| {
                                let (width, height) = page();
                                if let Some(action) = browser_view::scroll_action(
                                    (f64::from(wheel.x), f64::from(wheel.y)),
                                    (f64::from(wheel.dx), f64::from(wheel.dy)),
                                    scale(),
                                    width,
                                    height,
                                ) {
                                    send(action);
                                }
                            })
                            .absolute()
                            .inset_0()
                            .on_press(move |event: &PressEvent| {
                                field.focus();
                                let (width, height) = page();
                                if let Some(action) = browser_view::click_action(
                                    f64::from(event.position.x.0),
                                    f64::from(event.position.y.0),
                                    scale(),
                                    width,
                                    height,
                                ) {
                                    send(action);
                                }
                            }),
                        )
                        .into_element()
                }
            },
        ))
        .child(when(
            move || browsing() && is_desktop(),
            move || keyboard(field, accepts_input, send),
        ))
        .child(when(
            move || browsing() && dialog.with(Option::is_some),
            move || {
                nsh_base::dialog()
                    .close_on_escape(false)
                    .close_on_backdrop_press(false)
                    .on_cancel(move || {
                        send(BrowserAction::Dialog {
                            accept: false,
                            text: String::new(),
                        });
                    })
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(dialog_card(dialog, send))
            },
        ))
        .child(dynamic(move || {
            credentials
                .get()
                .map(|data| data_panel(data, send, move || credentials.set(None)))
        }))
        .into_element()
}

/// The keyboard's target once the page was clicked. Forward native insertions;
/// whole-value changes can include earlier keys while a selection is in flight.
fn keyboard(
    field: ElementRef,
    accepts_input: Memo<bool>,
    send: impl Fn(BrowserAction) -> bool + Copy + 'static,
) -> impl IntoElement {
    let focused = signal(false);
    view()
        .child(
            nsh_core::element::input()
                .element_ref(&field)
                .absolute()
                .top_0()
                .left_0()
                .w(px(1.0))
                .h(px(1.0))
                .opacity(0.0)
                .caret(false)
                .readonly(move || !accepts_input.get())
                .multiline(true)
                .auto_correct(false)
                .auto_capitalize(AutoCapitalize::None)
                .on_focus(move || focused.set(true))
                .on_blur(move || focused.set(false))
                .on_change(move |text: &str| {
                    if !text.is_empty() {
                        field.set_selection(0, text.encode_utf16().count() as u32);
                    }
                })
                .on_key_press(move |key: &str| {
                    let action = match key {
                        "Backspace" => BrowserAction::Key {
                            key: BrowserKey::Backspace,
                        },
                        "Enter" => BrowserAction::Key {
                            key: BrowserKey::Enter,
                        },
                        "Escape" | "" => return,
                        text => BrowserAction::Type { text: text.into() },
                    };
                    send(action);
                }),
        )
        // Keys that do not type text, bound only while the page has the keyboard.
        .child(when(
            move || focused.get(),
            move || {
                for (keystroke, key) in KEYS {
                    on_key_with(
                        keystroke,
                        KeyOptions {
                            when: When::Always,
                            repeat: true,
                        },
                        move |_| {
                            send(BrowserAction::Key { key });
                        },
                    );
                }
                view()
            },
        ))
}

/// The page's alert, confirmation or prompt, answered once.
fn dialog_card(
    dialog: Memo<Option<agent_protocol::browser::BrowserDialog>>,
    send: impl Fn(BrowserAction) -> bool + Copy + 'static,
) -> impl IntoElement {
    let reply = signal(String::new());
    let answer = move |accept: bool| {
        let text = if accept {
            reply.get_untracked()
        } else {
            String::new()
        };
        reply.set(String::new());
        send(BrowserAction::Dialog { accept, text });
    };
    view()
        .w_full()
        .max_w(px(480.0))
        .gap_3()
        .p_4()
        .rounded(px(12.0))
        .border_1()
        .border_color(ui::colors::border())
        .bg(ui::colors::BACKGROUND)
        .child(
            text(move || {
                dialog
                    .with(|d| d.as_ref().map(|d| d.message.clone()))
                    .unwrap_or_default()
            })
            .selectable(true),
        )
        .child(when(
            move || dialog.with(|d| d.as_ref().is_some_and(|d| d.prompt)),
            move || {
                ui::field()
                    .label(tr("入力"))
                    .value(move || reply.get())
                    .on_change(move |text: &str| reply.set(text.to_owned()))
                    .on_submit(move |_: &str| answer(true))
            },
        ))
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(ui::action_button(
                    tr("キャンセル"),
                    ActionTone::Secondary,
                    move || answer(false),
                ))
                .child(ui::action_button("OK", ActionTone::Primary, move || {
                    answer(true)
                })),
        )
}

/// The Host's browser data, as the Apple apps' sheet lists it: its browser
/// profiles to import sign-ins or saved passwords from, the browsers it
/// could not read, and the shown site's saved passwords.
fn data_panel(
    data: BrowserCredentials,
    send: impl Fn(BrowserAction) -> bool + Copy + 'static,
    close: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    use ui::{Enabled, colors};
    on_key("escape", move |_| close());
    let BrowserCredentials {
        profiles,
        import_errors,
        origin,
        accounts,
        password_error,
    } = data;
    let note = |copy: String| view().p_3().child(text(copy).text_color(colors::muted()));
    let button = move |title: String, tone, action: BrowserAction| {
        ui::action_button(title, tone, move || {
            send(action.clone());
        })
    };
    let mut sections = Vec::new();
    let mut imports: Vec<View> = profiles
        .into_iter()
        .map(|profile| {
            view()
                .gap_2()
                .p_3()
                .child(text(profile.label).font_weight(FontWeight::MEDIUM))
                .child(
                    view()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .child(button(
                            tr("ログイン状態（Cookie）").into(),
                            ActionTone::Secondary,
                            BrowserAction::ImportCookies {
                                profile: profile.id.clone(),
                            },
                        ))
                        .child(button(
                            tr("保存パスワード → Keychain").into(),
                            ActionTone::Secondary,
                            BrowserAction::ImportPasswords {
                                profile: profile.id,
                            },
                        )),
                )
        })
        .collect();
    if imports.is_empty() {
        imports.push(note(
            tr("取り込めるブラウザプロファイルがありません。").into(),
        ));
    }
    sections.push((tr("接続先のブラウザから取り込む").to_owned(), imports));
    if !import_errors.is_empty() {
        let errors = import_errors
            .into_iter()
            .map(|error| {
                view().gap_1().p_3().child(text(error.browser)).child(
                    text(localized(error.message))
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                        .selectable(true),
                )
            })
            .collect();
        sections.push((tr("読み取れなかったブラウザ").to_owned(), errors));
    }
    if let Some(origin) = origin {
        let mut passwords: Vec<View> = accounts
            .iter()
            .map(|username| {
                view().p_3().items_start().child(button(
                    if username.is_empty() {
                        tr("ユーザー名なし").into()
                    } else {
                        username.clone()
                    },
                    ActionTone::Secondary,
                    BrowserAction::FillPassword {
                        origin: origin.clone(),
                        username: username.clone(),
                    },
                ))
            })
            .collect();
        if accounts.is_empty() {
            passwords.push(note(password_error.map_or_else(
                || tr("このサイトの保存パスワードはありません。").into(),
                localized,
            )));
        }
        let saved = !accounts.is_empty();
        passwords.push(
            view().p_3().items_start().child(
                button(
                    tr("このサイトの保存パスワードを削除").into(),
                    ActionTone::Danger,
                    BrowserAction::DeletePasswords {
                        origin: origin.clone(),
                    },
                )
                .enabled(move || saved),
            ),
        );
        sections.push((tr_format("保存パスワード · {0}", [&origin]), passwords));
    }
    view()
        .absolute()
        .inset_0()
        .bg(colors::BACKGROUND)
        .label(tr("ブラウザデータ"))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(40.0))
                .flex_shrink_0()
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(tr("ブラウザデータ"))
                        .font_weight(FontWeight::SEMIBOLD)
                        .flex_1(),
                )
                .child(ui::text_button(tr("閉じる"), close)),
        )
        .child(
            scroll().flex_1().min_h_0().child(sections.into_iter().fold(
                view().gap_4().p_4(),
                |content, (title, rows)| {
                    content.child(
                        view()
                            .gap_2()
                            .child(text(title).text_size(px(12.0)).text_color(colors::muted()))
                            .child(ui::card(rows)),
                    )
                },
            )),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_page_keys_are_ones_windows_lets_the_app_bind() {
        use nsh_core::{keymap::Keystroke, platform::Os};
        for (keystroke, _) in super::KEYS {
            let parsed = Keystroke::parse(keystroke, Os::Windows).unwrap();
            assert_eq!(parsed.reserved(Os::Windows), None, "{keystroke}");
        }
    }
}
