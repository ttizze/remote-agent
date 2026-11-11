use crate::Connection;
use crate::l10n::{localized, tr};
#[cfg(not(desktop))]
use crate::native;
use crate::ui::Enabled;
use agent_core::{
    presentation::{
        model_settings::OPENROUTER_MODEL_QUERY, permissions::PERMISSION_CHOICES, view::Presentation,
    },
    state::{DraftKey, Intent, operations as op},
};
use agent_protocol::session::ProviderKind;
#[cfg(desktop)]
use nsh_core::element::{ImageSrc, input};
use nsh_core::prelude::*;
use serde_json::{Value, json};

/// The open model form: which draft of which Host and account it applies to,
/// and the models it shows.
#[derive(Clone, PartialEq)]
pub(crate) struct Picker {
    id: String,
    host: String,
    storage: String,
    draft: DraftKey,
    provider: Option<ProviderKind>,
    query: String,
}

impl Picker {
    fn owns(&self, host: &str, storage: &str, draft: &DraftKey) -> bool {
        self.host == host && self.storage == storage && &self.draft == draft
    }
}

/// Opens the model form for `connection`'s draft on `provider`'s models (the
/// draft's provider when `None`) matching `query`.
pub(crate) fn open(connection: Connection, provider: Option<ProviderKind>, query: &str) {
    let host = connection.host.get_untracked();
    connection
        .model_picker
        .set(Some(connection.publication.with_untracked(|p| Picker {
            id: uuid::Uuid::new_v4().to_string(),
            host,
            storage: p.snapshot.connection.storage_scope.clone(),
            draft: p.settings.draft_key(),
            provider: provider.or(p.settings.provider()),
            query: query.into(),
        })));
    connection.dispatch(Intent::ReadProviderSettings(op::ReadProviderSettings {}));
    connection.dispatch(Intent::ListAccounts(op::ListAccounts {}));
}

/// The model control's padding at each side of its name, and the name's font.
#[cfg(desktop)]
const PADDING: f32 = 8.0;
#[cfg(desktop)]
const LABEL_FONT: &str = "14px system-ui";

const FAST_WIDTH: f32 = 40.0;
const EFFORT_WIDTH: f32 = 44.0;

pub(crate) fn quick_width(has_fast: bool, effort_count: usize) -> f32 {
    (if has_fast { FAST_WIDTH } else { 0.0 }) + if effort_count > 0 { EFFORT_WIDTH } else { 0.0 }
}

/// The model control and its picker. On the desktop the control sets `natural`
/// to its width with the whole name and is `width` wide, its name shortened to
/// fit; elsewhere it takes its own width.
pub(crate) fn controls(
    #[cfg_attr(not(desktop), allow(unused_variables))] natural: Signal<f32>,
    #[cfg_attr(not(desktop), allow(unused_variables))] width: impl Fn() -> f32 + 'static,
) -> View {
    let connection = use_context::<Connection>().unwrap();
    let publication = connection.publication;
    let picker = connection.model_picker;
    effect(move || {
        let host = connection.host.get();
        publication.with(|p| {
            let draft = p.settings.draft_key();
            if picker.with_untracked(|picker| {
                picker.as_ref().is_some_and(|picker| {
                    !picker.owns(&host, &p.snapshot.connection.storage_scope, &draft)
                })
            }) {
                picker.set(None);
            }
            if let Some(provider) = p.snapshot.permission_control(&draft).load_provider {
                connection.dispatch(Intent::ReadPermissionSettings(op::ReadPermissionSettings {
                    provider,
                }));
            }
        });
    });
    let label = computed(move || {
        connection.settings.with(|p| {
            p.selected_model_name()
                .unwrap_or_else(|| tr("モデル").into())
        })
    });
    let open_picker = move || open(connection, None, "");
    let enabled = move || {
        connection.settings.with(|p| p.connected())
            && !connection.composer.with(|c| c.controls.sending)
    };
    let quick = computed(move || connection.settings.with(|p| p.quick_controls()));
    let fast_button = move || {
        use crate::ui::{self, Icon};
        ui::ghost(
            move || {
                SharedString::from(tr(if quick.with(|q| q.fast) {
                    "Fast：オン"
                } else {
                    "Fast：オフ"
                }))
            },
            view().child(dynamic(move || {
                ui::icon(
                    if quick.with(|q| q.fast) {
                        Icon::FAST_ON
                    } else {
                        Icon::FAST
                    },
                    18.0,
                )
            })),
        )
        .w(px(FAST_WIDTH))
        .h(px(44.0))
        .text_color(move || {
            if quick.with(|q| q.fast) {
                crate::ui::colors::foreground()
            } else {
                crate::ui::colors::muted()
            }
        })
        .enabled(enabled)
        .on_press(move || {
            if let Some(service_tier) = quick.with_untracked(|q| q.toggle_fast_to.clone()) {
                connection.dispatch(Intent::SelectServiceTier {
                    thread_id: connection.settings.with_untracked(|p| p.draft_key()),
                    service_tier,
                });
            }
        })
    };
    let effort_button = move || {
        use crate::ui::{self, colors};
        ui::ghost(
            move || {
                SharedString::from(crate::l10n::tr_format(
                    "推論の強度：{0}",
                    [&quick.with(|q| q.effort.clone())],
                ))
            },
            view().child(dynamic(move || {
                let count = quick.with(|q| q.efforts.len());
                (0..count).fold(view().flex_row().gap(px(2.0)).items_end(), |bars, index| {
                    bars.child(
                        view()
                            .w(px(3.0))
                            .h(px(6.0 + 10.0 * (index + 1) as f32 / count as f32))
                            .rounded(px(1.0))
                            .bg(move || {
                                if index < quick.with(|q| q.effort_level) as usize {
                                    colors::foreground()
                                } else {
                                    nsh_core::element::rgb(0x555555)
                                }
                            }),
                    )
                })
            })),
        )
        .w(px(EFFORT_WIDTH))
        .h(px(44.0))
        .enabled(enabled)
        .on_press(open_picker)
    };
    // Drawn by NSH, so that its name shortens with an ellipsis as T3 Code's does.
    #[cfg(desktop)]
    let trigger = {
        use crate::ui::{self, colors};
        effect(move || natural.set(2.0 * PADDING + measure_text(LABEL_FONT, &label.get()).0));
        nsh_base::button()
            .accessibility_label(nsh_core::element::Prop::from(
                (move || Some(SharedString::from(label.get()))).into_reactive(),
            ))
            .enabled(enabled)
            .on_press(open_picker)
            .h(px(32.0))
            .w(move || px(width()))
            .px(px(PADDING))
            .flex_row()
            .items_center()
            .rounded(px(ui::RADIUS))
            .text_color(colors::foreground())
            .hover(|s| s.bg(colors::GHOST_HOVER))
            .active(|s| s.bg(colors::SELECTED))
            .disabled_style(|s| s.opacity(0.5))
            .child(
                text(move || label.get())
                    .text_size(px(14.0))
                    .truncate()
                    .flex_1()
                    .min_w_0(),
            )
    };
    #[cfg(not(desktop))]
    let trigger = crate::ui::text_button(move || SharedString::from(label.get()), open_picker)
        .enabled(enabled);
    let data = computed(move || {
        picker.with(|picker| {
            picker
                .as_ref()
                .map(|picker| publication.with(|p| picker_data(p, picker)))
        })
    });
    let act = move |action: &str, value: &str| act(connection, picker, action, value);
    #[cfg(desktop)]
    {
        use nsh_core::overlay::{Align, Placement};
        view()
            .flex_row()
            .items_center()
            .child(when(
                move || quick.with(|q| q.toggle_fast_to.is_some()),
                fast_button,
            ))
            .child(
                nsh_base::popover()
                    .open(move || Some(picker.with(Option::is_some)))
                    .on_open_change(move |event: &nsh_base::PopoverEvent| {
                        if matches!(event, nsh_base::PopoverEvent::OpenChange(false, _)) {
                            picker.set(None);
                        }
                    })
                    .placement(Placement::Top)
                    .align(Align::End)
                    .offset(px(8.0))
                    .popup_class("bg-[#0f0f0f] rounded-[10px] border border-[#303030]")
                    .trigger(trigger)
                    .child(panel(data, act)),
            )
            .child(when(
                move || quick.with(|q| !q.efforts.is_empty()),
                effort_button,
            ))
    }
    #[cfg(not(desktop))]
    {
        view()
            .flex_row()
            .items_center()
            .child(when(
                move || quick.with(|q| q.toggle_fast_to.is_some()),
                fast_button,
            ))
            .child(trigger)
            .child(when(
                move || quick.with(|q| !q.efforts.is_empty()),
                effort_button,
            ))
            .child(
                native("bex.details")
                    .prop("data", move || json!({"modelPicker": {"data": data.get()}}))
                    .on_native_event(move |event: &NativeEvent| {
                        if picker.with_untracked(|picker| {
                            picker.as_ref().is_some_and(|picker| {
                                event.data["token"].as_str() == Some(&picker.id)
                            })
                        }) {
                            act(
                                event.data["action"].as_str().unwrap_or_default(),
                                event.data["value"].as_str().unwrap_or_default(),
                            );
                        }
                    })
                    .w(px(0.0))
                    .h(px(0.0)),
            )
    }
}

/// Applies a model form action while its picker still owns the current draft.
fn act(connection: Connection, picker: Signal<Option<Picker>>, action: &str, value: &str) {
    let publication = connection.publication;
    let Some(current) = picker.get_untracked().filter(|picker| {
        publication.with_untracked(|p| {
            picker.owns(
                &connection.host.get_untracked(),
                &p.snapshot.connection.storage_scope,
                &p.settings.draft_key(),
            )
        })
    }) else {
        return;
    };
    match action {
        "close" => picker.set(None),
        "search" => picker.update(|picker| {
            if let Some(picker) = picker {
                picker.query = value.into();
            }
        }),
        "accounts" => {
            if let Some(provider) = current.provider {
                picker.set(None);
                connection.application.with(|app| {
                    app.navigate(crate::Navigation::Settings(true));
                    app.navigate(crate::Navigation::Provider(Some(provider)));
                });
            }
        }
        "provider"
            if publication
                .with_untracked(|p| p.settings.connected() && !p.composer.controls.sending) =>
        {
            if let Ok(provider) = serde_json::from_value::<ProviderKind>(json!(value)) {
                picker.update(|picker| {
                    if let Some(picker) = picker {
                        picker.provider = Some(provider);
                        picker.query.clear();
                    }
                });
                // The phone's segmented form selects an agent immediately;
                // desktop tabs browse its models until one is chosen.
                #[cfg(not(desktop))]
                if let Some(model) = connection
                    .settings
                    .with_untracked(|p| p.model_for_provider(provider))
                {
                    connection.dispatch(Intent::SelectModel {
                        thread_id: current.draft,
                        model,
                    });
                }
            }
        }
        _ => {
            if let Some(intent) =
                publication.with_untracked(|p| picker_intent(p, &current, action, value))
            {
                connection.dispatch(intent);
                // Keep the picker open to adjust the selected model's effort.
                if action == "openrouter-flash" {
                    picker.update(|picker| {
                        if let Some(picker) = picker {
                            picker.query = OPENROUTER_MODEL_QUERY.into();
                        }
                    });
                }
            }
        }
    }
}

#[cfg(desktop)]
pub(crate) fn provider_mark(id: &str) -> Option<(&'static str, nsh_core::element::Rgba)> {
    use crate::ui::colors::foreground;
    use nsh_core::element::rgb;
    Some(match id {
        "codex" => (
            include_str!("../../mobile/iosApp/Bex/Assets.xcassets/openai.imageset/openai.svg"),
            foreground(),
        ),
        "claude" => (
            include_str!("../../mobile/iosApp/Bex/Assets.xcassets/claude.imageset/claude.svg"),
            rgb(0xd97757),
        ),
        "deepseek" => (
            include_str!("../../mobile/iosApp/Bex/Assets.xcassets/deepseek.imageset/deepseek.svg"),
            rgb(0x4d6bfe),
        ),
        _ => return None,
    })
}

/// The desktop model form, as the desktop client's popover: the agents beside the
/// account, the model search and list, and the response settings.
#[cfg(desktop)]
fn panel(data: Memo<Option<Value>>, act: impl Fn(&str, &str) + Copy + 'static) -> View {
    use crate::{
        l10n::tr_format,
        ui::{self, Icon, colors},
    };
    let field = move |key: &'static str| {
        move || {
            data.with(|data| {
                data.as_ref()
                    .map(|data| data[key].clone())
                    .unwrap_or_default()
            })
        }
    };
    let disabled = move || field("disabled")().as_bool().unwrap_or(true);
    let agents = view()
        .w(px(44.0))
        .flex_shrink_0()
        .p_1()
        .gap_1()
        .bg(colors::RAISED)
        .child(each(
            move || field("providers")().as_array().cloned().unwrap_or_default(),
            |choice: &Value| choice["id"].as_str().unwrap_or_default().to_owned(),
            move |choice, _| {
                let choice = choice.get_untracked();
                let id = choice["id"].as_str().unwrap_or_default().to_owned();
                let title = choice["title"].as_str().unwrap_or_default().to_owned();
                let selected = {
                    let id = id.clone();
                    let selected =
                        computed(move || field("provider")().as_str() == Some(id.as_str()));
                    move || selected.get()
                };
                let mark = provider_mark(&id);
                nsh_base::button()
                    .accessibility_label(Some(SharedString::from(title.clone())))
                    .disabled(disabled)
                    .w(px(36.0))
                    .h(px(36.0))
                    .rounded(px(6.0))
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(colors::GHOST_HOVER))
                    .selected(selected)
                    .style_when(selected, |s| s.bg(colors::SELECTED))
                    .on_press(move || act("provider", &id))
                    .child(match mark {
                        Some((markup, color)) => svg(ImageSrc::svg_markup(&markup.replacen(
                            "<svg ",
                            "<svg fill=\"currentColor\" ",
                            1,
                        )))
                        .w(px(20.0))
                        .h(px(20.0))
                        .text_color(color)
                        .into_element(),
                        None => text(title).text_size(px(12.0)).into_element(),
                    })
            },
        ));
    let account = nsh_base::button()
        .accessibility_label(Some(SharedString::from(tr("アカウントと週間残量"))))
        .px_3()
        .py_2()
        .gap_2()
        .rounded(px(8.0))
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .on_press(move || act("accounts", ""))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    text(move || {
                        field("account")()["title"]
                            .as_str()
                            .unwrap_or(tr("未選択"))
                            .to_owned()
                    })
                    .flex_1()
                    .min_w_0()
                    .number_of_lines(1)
                    .text_size(px(12.0)),
                )
                .child(ui::icon(Icon::CHEVRON_RIGHT, 12.0)),
        )
        .child(each(
            move || {
                field("account")()["usage"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            },
            |usage: &Value| usage["label"].as_str().unwrap_or_default().to_owned(),
            |usage, _| {
                let percent = move || {
                    usage
                        .with(|usage| usage["remainingPercent"].as_f64().unwrap_or_default() as f32)
                };
                view()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        text(move || {
                            usage.with(|usage| {
                                usage["label"].as_str().unwrap_or_default().to_owned()
                            })
                        })
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                    )
                    .child(
                        view()
                            .flex_1()
                            .h(px(4.0))
                            .rounded_full()
                            .bg(colors::RAISED)
                            .child(
                                view()
                                    .h(px(4.0))
                                    .rounded_full()
                                    .w(move || relative(percent() / 100.0))
                                    .bg(move || {
                                        if percent() <= 20.0 {
                                            nsh_core::element::rgb(0xfb923c)
                                        } else {
                                            colors::foreground()
                                        }
                                    }),
                            ),
                    )
                    .child(text(move || format!("{}%", percent())).text_size(px(12.0)))
            },
        ));
    let models = scroll().flex_1().min_h_0().child(
        view()
            .px_2()
            .pt(px(10.0))
            .child(when(
                move || field("models")().as_array().is_none_or(Vec::is_empty),
                move || {
                    text(move || {
                        if field("loading")().as_bool().unwrap_or(false) {
                            tr("モデルを読み込み中…")
                        } else {
                            tr("利用可能なモデルがありません")
                        }
                    })
                    .p_2()
                    .text_color(colors::muted())
                },
            ))
            .child(each(
                move || field("models")().as_array().cloned().unwrap_or_default(),
                |model: &Value| model["id"].as_str().unwrap_or_default().to_owned(),
                move |model, _| {
                    let model = model.get_untracked();
                    let id = model["id"].as_str().unwrap_or_default().to_owned();
                    let title = model["title"].as_str().unwrap_or_default().to_owned();
                    let selected = {
                        let id = id.clone();
                        let selected = computed(move || {
                            field("selectedModel")().as_str() == Some(id.as_str())
                        });
                        move || selected.get()
                    };
                    nsh_base::button()
                        .accessibility_label(Some(SharedString::from(title.clone())))
                        .disabled(disabled)
                        .h(px(32.0))
                        .px_2()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .rounded(px(ui::RADIUS))
                        .hover(|s| s.bg(colors::GHOST_HOVER))
                        .selected(selected)
                        .selected_style(|s| s.bg(colors::SELECTED))
                        .on_press(move || act("model", &id))
                        .child(dynamic(move || {
                            let mark = field("provider")().as_str().and_then(provider_mark);
                            view().child(mark.map(|(markup, color)| {
                                svg(ImageSrc::svg_markup(&markup.replacen(
                                    "<svg ",
                                    "<svg fill=\"currentColor\" ",
                                    1,
                                )))
                                .w(px(16.0))
                                .h(px(16.0))
                                .text_color(color)
                            }))
                        }))
                        .child(text(title).text_size(px(14.0)).number_of_lines(1).min_w_0())
                        .child(when(selected, || {
                            view()
                                .text_color(colors::foreground())
                                .child(ui::icon(Icon::CHECK, 14.0))
                        }))
                },
            ))
            .child(when(
                move || field("customModel")().is_string(),
                move || {
                    ui::text_button(
                        move || {
                            SharedString::from(tr_format(
                                "モデルIDを指定: {0}",
                                [&field("customModel")().as_str().unwrap_or_default()],
                            ))
                        },
                        move || act("custom", ""),
                    )
                },
            )),
    );
    let body = view()
        .w(px(316.0))
        .min_w_0()
        .border_l_1()
        .border_l_color(colors::border())
        .child(each(
            move || field("notices")().as_array().cloned().unwrap_or_default(),
            |notice: &Value| notice["id"].as_str().unwrap_or_default().to_owned(),
            move |notice, _| {
                let notice = notice.get_untracked();
                let id = notice["id"].as_str().unwrap_or_default().to_owned();
                let dismiss = id.clone();
                view()
                    .gap_1()
                    .child(text(
                        notice["title"].as_str().unwrap_or_default().to_owned(),
                    ))
                    .child(
                        text(notice["text"].as_str().unwrap_or_default().to_owned())
                            .selectable(true)
                            .text_size(px(12.0))
                            .text_color(colors::muted()),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap_2()
                            .child(notice["recovery"].as_str().map(|recovery| {
                                ui::text_button(recovery.to_owned(), move || act("recover", &id))
                            }))
                            .child(ui::text_button(tr("閉じる"), move || {
                                act("dismiss", &dismiss)
                            })),
                    )
            },
        ))
        .child(
            view()
                .h(px(44.0))
                .px_3()
                .justify_center()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    input()
                        .auto_focus(true)
                        .h(px(32.0))
                        .text_size(px(14.0))
                        .placeholder(tr("モデルを検索"))
                        .label(tr("モデルを検索"))
                        .value(move || field("query")().as_str().unwrap_or_default().to_owned())
                        .on_change(move |value: &str| act("search", value)),
                ),
        )
        .child(when(
            move || !field("openrouter")().is_null(),
            move || openrouter_picks(field("openrouter"), disabled, act),
        ))
        .child(models)
        .child(each(
            move || field("errors")().as_array().cloned().unwrap_or_default(),
            |error: &Value| error.as_str().unwrap_or_default().to_owned(),
            |error, _| {
                text(
                    error
                        .get_untracked()
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
                .selectable(true)
                .text_size(px(12.0))
                .text_color(nsh_core::element::rgb(0xff7777))
            },
        ))
        .child(account);
    view()
        .w(px(552.0))
        .text_color(colors::foreground())
        .child(
            view()
                .flex_row()
                .h(px(346.0))
                .min_h_0()
                .child(agents)
                .child(body)
                .child(effort_column(field("quick"), disabled, act)),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_t_color(colors::border())
                .child(when(
                    move || {
                        field("quick")()["speeds"]
                            .as_array()
                            .is_some_and(|s| s.len() > 1)
                    },
                    move || speed_control(field("quick"), disabled, act),
                ))
                .child(view().flex_1())
                .child(crate::permissions::picker_control()),
        )
}

#[cfg(desktop)]
fn effort_column(
    quick: impl Fn() -> Value + Copy + 'static,
    disabled: impl Fn() -> bool + Copy + 'static,
    act: impl Fn(&str, &str) + Copy + 'static,
) -> View {
    use crate::ui::{self, Icon, colors};
    let connection = use_context::<Connection>().unwrap();
    view()
        .w(px(192.0))
        .flex_shrink_0()
        .border_l_1()
        .border_l_color(colors::border())
        .child(
            view()
                .h(px(44.0))
                .px_3()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(tr("思考の深さ"))
                        .text_size(px(12.0))
                        .text_color(colors::muted()),
                )
                .child(
                    text(move || {
                        connection
                            .settings
                            .with(|s| s.selected_model_name().unwrap_or_default())
                    })
                    .text_size(px(12.0))
                    .text_color(colors::muted())
                    .truncate()
                    .flex_1()
                    .min_w_0(),
                ),
        )
        .child(
            scroll()
                .flex_1()
                .min_h_0()
                .child(view().px_2().pt(px(10.0)).child(each(
                    move || quick()["efforts"].as_array().cloned().unwrap_or_default(),
                    |effort: &Value| effort.as_str().unwrap_or_default().to_owned(),
                    move |effort, _| {
                        let id = effort
                            .get_untracked()
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        let current = {
                            let id = id.clone();
                            let current =
                                computed(move || quick()["effort"].as_str() == Some(id.as_str()));
                            move || current.get()
                        };
                        let label =
                            agent_core::presentation::model_settings::reasoning_effort_label(
                                crate::l10n::language(),
                                &id,
                            );
                        ui::bare_button(label.clone(), move || act("effort", &id))
                            .enabled(move || !disabled())
                            .selected(current)
                            .w_full()
                            .h(px(32.0))
                            .px_2()
                            .rounded(px(2.0))
                            .hover(|s| s.bg(colors::GHOST_HOVER))
                            .child(text(label).text_size(px(14.0)).flex_1())
                            .child(when(current, || ui::icon(Icon::CHECK, 14.0)))
                    },
                ))),
        )
}

#[cfg(desktop)]
fn speed_control(
    quick: impl Fn() -> Value + Copy + 'static,
    disabled: impl Fn() -> bool + Copy + 'static,
    act: impl Fn(&str, &str) + Copy + 'static,
) -> View {
    use crate::ui::{self, colors};
    view()
        .flex_row()
        .p(px(2.0))
        .rounded(px(8.0))
        .bg(colors::RAISED)
        .child(each(
            move || quick()["speeds"].as_array().cloned().unwrap_or_default(),
            |speed: &Value| speed["id"].as_str().unwrap_or_default().to_owned(),
            move |speed, _| {
                let speed = speed.get_untracked();
                let id = speed["id"].as_str().unwrap_or_default().to_owned();
                let title = speed["title"].as_str().unwrap_or_default().to_owned();
                let current = {
                    let id = id.clone();
                    let current = computed(move || quick()["speed"].as_str() == Some(id.as_str()));
                    move || current.get()
                };
                ui::bare_button(title.clone(), move || act("speed", &id))
                    .enabled(move || !disabled())
                    .selected(current)
                    .h(px(24.0))
                    .px_2()
                    .rounded(px(6.0))
                    .style_when(current, |s| s.bg(colors::SELECTED))
                    .child(text(title).text_size(px(12.0)))
            },
        ))
}

/// The desktop client's OpenRouter shortcuts over DeepSeek's models: where the
/// selected model's requests go, Flash 4.1 through OpenRouter, and OpenRouter's models.
#[cfg(desktop)]
fn openrouter_picks(
    picks: impl Fn() -> Value + Copy + 'static,
    disabled: impl Fn() -> bool + Copy + 'static,
    act: impl Fn(&str, &str) + Copy + 'static,
) -> View {
    use crate::ui::{self, Selected};
    let flash_selected = move || picks()["flashSelected"].as_bool().unwrap_or(false);
    view()
        .items_start()
        .gap_2()
        .child(when(
            move || picks()["route"].is_string(),
            move || {
                text(move || picks()["route"].as_str().unwrap_or_default().to_owned())
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
            },
        ))
        .child(when(
            move || picks()["flash"].as_bool().unwrap_or(false),
            move || {
                ui::text_button(
                    move || {
                        SharedString::from(if flash_selected() {
                            tr("OpenRouter · Flash 4.1 選択中")
                        } else {
                            tr("OpenRouter経由のFlash 4.1を選択")
                        })
                    },
                    move || act("openrouter-flash", ""),
                )
                .selected_when(flash_selected)
                .enabled(move || !disabled())
            },
        ))
        .child(
            ui::text_button(tr("OpenRouterのモデル一覧を表示"), move || {
                act("search", OPENROUTER_MODEL_QUERY)
            })
            .enabled(move || !disabled()),
        )
}

fn picker_data(p: &Presentation, picker: &Picker) -> Value {
    let settings = &p.settings;
    let selected = settings.selected_model();
    let controls = settings.quick_controls();
    let permissions = p.snapshot.permission_control(&picker.draft);
    let account = settings.accounts().and_then(|accounts| {
        accounts
            .accounts
            .iter()
            .find(|account| {
                Some(account.provider) == picker.provider && accounts.is_selected(account)
            })
            .cloned()
    });
    json!({
        "id": picker.id, "provider": picker.provider, "query": picker.query,
        "disabled": !settings.connected() || p.composer.controls.sending,
        "providers": settings.connection_setup().agents.iter().map(|agent| json!({"id": agent.provider, "title": agent.name})).collect::<Vec<_>>(),
        "models": settings.models_matching(picker.provider, picker.query.clone()).iter().map(|model| json!({"id": model.id, "title": model.display_name})).collect::<Vec<_>>(),
        "selectedModel": settings.current_model().map(|model| model.id),
        "loading": settings.models_loading(picker.provider),
        "errors": settings.model_error_messages(picker.provider),
        "notices": settings.notices().iter().map(|notice| json!({
            "id": serde_json::to_string(&notice.id).expect("Core error IDs are serializable"),
            "title": notice.title, "text": notice.text.to_string(), "recovery": notice.recovery_label,
        })).collect::<Vec<_>>(),
        "customModel": settings.custom_model_choice(picker.provider, &picker.query).map(|model| model.id),
        "openrouter": settings.openrouter_picks(picker.provider).map(|picks| json!({
            "route": picks.route.map(|route| crate::l10n::tr_format("選択中の送信先: {0}", [&route])),
            "flash": picks.flash.is_some(), "flashSelected": picks.flash_selected,
        })),
        "quick": (selected.as_ref().map(|model| model.provider) == picker.provider).then(|| json!({
            "efforts": controls.efforts, "effort": controls.effort,
            "speeds": controls.speed_choices.iter().map(|choice| json!({"id": choice.id, "title": choice.label})).collect::<Vec<_>>(),
            "speed": controls.speed_choices.iter().find(|choice| choice.selected).map(|choice| &choice.id),
        })),
        "permissions": (permissions.provider == picker.provider && permissions.version.is_some()).then(|| json!({
            "selected": permissions.mode.and_then(|mode| PERMISSION_CHOICES.iter().position(|choice| choice.0 == mode)).map(|index| index.to_string()),
            "choices": PERMISSION_CHOICES.iter().enumerate().map(|(index, (_, title, _))| json!({"id": index.to_string(), "title": localized((*title).into())})).collect::<Vec<_>>(),
        })),
        "account": account.as_ref().map(|account| json!({"title": account.email.as_ref().unwrap_or(&account.id), "asset": crate::accounts::icon(account.provider),
            "usage": settings.account_weekly_usage(account.provider, account.id.clone()).into_iter()
                .map(|window| json!({
                    "label": agent_core::presentation::model_settings::usage_window_label(crate::l10n::language(), window.period, window.name),
                    "remainingPercent": window.remaining_percent,
                    "resetsAt": window.resets_at,
                })).collect::<Vec<_>>(),
        })),
    })
}

fn picker_intent(p: &Presentation, picker: &Picker, action: &str, value: &str) -> Option<Intent> {
    if matches!(action, "recover" | "dismiss") {
        let id = p
            .settings
            .notices()
            .into_iter()
            .find(|notice| serde_json::to_string(&notice.id).ok().as_deref() == Some(value))?
            .id;
        return Some(if action == "recover" {
            Intent::RecoverError { id }
        } else {
            Intent::DismissError { id }
        });
    }
    if !p.settings.connected() || p.composer.controls.sending {
        return None;
    }
    let thread_id = picker.draft.clone();
    Some(match action {
        "effort" | "speed"
            if p.settings.selected_model().map(|model| model.provider) != picker.provider =>
        {
            return None;
        }
        "model" => Intent::SelectModel {
            thread_id,
            model: p
                .settings
                .models_matching(picker.provider, picker.query.clone())
                .into_iter()
                .find(|model| model.id == value)?
                .model,
        },
        "custom" => Intent::SelectModel {
            thread_id,
            model: p
                .settings
                .custom_model_choice(picker.provider, &picker.query)?,
        },
        "openrouter-flash" => Intent::SelectModel {
            thread_id,
            model: p.settings.openrouter_picks(picker.provider)?.flash?,
        },
        "effort" => Intent::SelectEffort {
            thread_id,
            effort: value.into(),
        },
        "speed" => Intent::SelectServiceTier {
            thread_id,
            service_tier: value.into(),
        },
        "permission" => {
            let control = p.snapshot.permission_control(&thread_id);
            if control.provider != picker.provider {
                return None;
            }
            let (mode, _, _) = PERMISSION_CHOICES.get(value.parse::<usize>().ok()?)?;
            Intent::UpdatePermissionSettings(op::UpdatePermissionSettings {
                provider: control.provider?,
                mode: *mode,
                version: control.version?,
            })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::state::{Draft, Snapshot};
    use std::sync::Arc;

    fn fixture() -> (Snapshot, Picker) {
        let mut state = Snapshot::default();
        state.connection.connected = true;
        state.connection.storage_scope = "storage".into();
        state.catalog.models = Arc::new(serde_json::from_value(json!([
            {"id":"codex/native","model":{"provider":"codex","id":"native"},"displayName":"日本語モデル 👋", "settings": {
                "defaultReasoningEffort":"medium", "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}],
                "serviceTiers":[{"id":"priority","name":"高速","fast":true}]
            }},
            {"id":"claude/native","model":{"provider":"claude","id":"native"},"displayName":"別のモデル"}
        ])).unwrap());
        Arc::make_mut(&mut state.catalog.account).accounts = Some(Arc::new(serde_json::from_value(json!({
            "selected":{"codex":"shared", "claude":"shared"}, "accounts":[
                {"id":"shared","provider":"claude","email":"other@example.invalid"},
                {"id":"shared","provider":"codex","email":"native@example.invalid", "usage":{"fetchedAt":1,"windows":[
                    {"period":{"kind":"minutes","minutes":300}, "remainingPercent":72}, {"period":{"kind":"minutes","minutes":10080},"remainingPercent":42}
                ]}}
            ]
        })).unwrap()));
        let draft = state.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut state.input.drafts).insert(
            draft.clone(),
            Arc::new(Draft {
                model: Some(
                    serde_json::from_value(json!({"provider":"codex","id":"native"})).unwrap(),
                ),
                effort: Some("medium".into()),
                service_tier: Some("priority".into()),
                ..Default::default()
            }),
        );
        let picker = Picker {
            id: "native-picker".into(),
            host: "host".into(),
            storage: "storage".into(),
            draft,
            provider: Some(ProviderKind::Codex),
            query: "日本語".into(),
        };
        (state, picker)
    }

    #[test]
    fn native_model_form_uses_core_search_settings_and_provider_scoped_account_limits() {
        let (state, picker) = fixture();
        let p = Presentation::project(Arc::new(state), "", None).unwrap();
        let data = picker_data(&p, &picker);
        let fixture: Value = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/model-picker.json"
        ))
        .unwrap();
        assert_eq!(json!({"modelPicker": {"data": data}}), fixture);
        assert!(
            matches!(picker_intent(&p, &picker, "model", "codex/native"), Some(Intent::SelectModel {model, thread_id}) if model.provider == ProviderKind::Codex && model.id == "native" && thread_id == picker.draft)
        );
        assert!(picker_intent(&p, &picker, "model", "claude/native").is_none());
        assert!(picker_intent(&p, &picker, "model", "missing").is_none());
        assert!(
            matches!(picker_intent(&p, &picker, "custom", ""), Some(Intent::SelectModel {model, thread_id}) if model.provider == ProviderKind::Codex && model.id == "日本語" && thread_id == picker.draft)
        );
        assert!(
            matches!(picker_intent(&p, &picker, "effort", "low"), Some(Intent::SelectEffort {thread_id, effort}) if thread_id == picker.draft && effort == "low")
        );
        assert!(
            matches!(picker_intent(&p, &picker, "speed", "default"), Some(Intent::SelectServiceTier {thread_id, service_tier}) if thread_id == picker.draft && service_tier == "default")
        );
    }

    #[test]
    fn permissions_use_the_current_provider_and_version_and_invalid_actions_are_ignored() {
        use agent_protocol::permissions::{PermissionMode, PermissionSettings};
        let (mut state, mut picker) = fixture();
        state.catalog.permission_settings = Arc::new(
            [(
                ProviderKind::Codex,
                Arc::new(op::PermissionSettingsState {
                    result: Some(Ok(PermissionSettings {
                        mode: Some(PermissionMode::Ask),
                        version: "revision-2".into(),
                    })),
                }),
            )]
            .into(),
        );
        let p = Presentation::project(Arc::new(state), "", None).unwrap();
        let data = picker_data(&p, &picker);
        assert_eq!(data["permissions"]["selected"], "0");
        assert_eq!(data["permissions"]["choices"][2]["title"], "フルアクセス");
        assert!(
            matches!(picker_intent(&p, &picker, "permission", "1"), Some(Intent::UpdatePermissionSettings(request)) if request.provider == ProviderKind::Codex && request.version == "revision-2" && request.mode == PermissionMode::Auto)
        );
        for (action, value) in [
            ("unknown", ""),
            ("permission", "9"),
            ("permission", "not-an-index"),
            ("recover", "old-error"),
            ("dismiss", "old-error"),
        ] {
            assert!(picker_intent(&p, &picker, action, value).is_none());
        }
        picker.provider = Some(ProviderKind::Claude);
        let data = picker_data(&p, &picker);
        assert!(data["permissions"].is_null());
        assert!(data["quick"].is_null());
        assert_eq!(data["account"]["title"], "other@example.invalid");
        for (action, value) in [
            ("permission", "1"),
            ("effort", "low"),
            ("speed", "default"),
            ("custom", ""),
        ] {
            assert!(picker_intent(&p, &picker, action, value).is_none());
        }
    }

    #[test]
    fn deepseek_form_offers_flash_through_openrouter_once_the_host_lists_it() {
        let (mut state, mut picker) = fixture();
        picker.provider = Some(ProviderKind::DeepSeek);
        let p = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert_eq!(
            picker_data(&p, &picker)["openrouter"],
            json!({"route": null, "flash": false, "flashSelected": false})
        );
        assert!(picker_intent(&p, &picker, "openrouter-flash", "").is_none());
        let flash = r#"["openrouter","deepseek/deepseek-v4.1-flash"]"#;
        Arc::make_mut(&mut state.catalog.models).push(
            serde_json::from_value(json!({"id":"deepseek:router","credentialRoute":"OpenRouter","model":{"provider":"deepseek","id":flash},"displayName":"OpenRouter · DeepSeek V4.1 Flash"}))
                .unwrap(),
        );
        let p = Presentation::project(Arc::new(state), "", None).unwrap();
        assert_eq!(picker_data(&p, &picker)["openrouter"]["flash"], true);
        assert!(
            matches!(picker_intent(&p, &picker, "openrouter-flash", ""), Some(Intent::SelectModel {model, thread_id}) if model.provider == ProviderKind::DeepSeek && model.id == flash && thread_id == picker.draft)
        );
        picker.provider = Some(ProviderKind::Codex);
        assert!(picker_data(&p, &picker)["openrouter"].is_null());
        assert!(picker_intent(&p, &picker, "openrouter-flash", "").is_none());
    }

    #[test]
    fn a_disconnected_model_form_keeps_error_recovery_and_dismissal_available() {
        let (mut state, picker) = fixture();
        state.connection.connected = false;
        Arc::make_mut(&mut state.operations).insert(
            op::OperationKey::Accounts,
            op::OperationState {
                generation: 7,
                phase: op::OperationPhase::Failed {
                    message: "account lookup failed".into(),
                },
            },
        );
        let p = Presentation::project(Arc::new(state), "", None).unwrap();
        let id = op::ErrorId::Operation {
            key: op::OperationKey::Accounts,
            generation: 7,
        };
        let encoded = serde_json::to_string(&id).unwrap();
        assert_eq!(picker_data(&p, &picker)["notices"][0]["id"], encoded);
        assert!(
            matches!(picker_intent(&p, &picker, "recover", &encoded), Some(Intent::RecoverError {id: actual}) if actual == id)
        );
        assert!(
            matches!(picker_intent(&p, &picker, "dismiss", &encoded), Some(Intent::DismissError {id: actual}) if actual == id)
        );
        assert!(picker_intent(&p, &picker, "dismiss", "obsolete-error").is_none());
    }

    #[test]
    fn a_model_form_cannot_apply_to_a_replaced_host_account_draft_or_busy_submission() {
        let (mut state, picker) = fixture();
        assert!(picker.owns("host", "storage", &picker.draft));
        assert!(!picker.owns("other-host", "storage", &picker.draft));
        assert!(!picker.owns("host", "other-account", &picker.draft));
        assert!(!picker.owns("host", "storage", &DraftKey::from("other-draft")));
        for disconnected in [true, false] {
            state.connection.connected = !disconnected;
            if !disconnected {
                Arc::make_mut(&mut state.operations).insert(
                    op::OperationKey::Submission {
                        draft_key: picker.draft.clone(),
                    },
                    op::OperationState {
                        generation: 1,
                        phase: op::OperationPhase::Running,
                    },
                );
            }
            let p = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
            assert_eq!(picker_data(&p, &picker)["disabled"], true);
            for (action, value) in [
                ("model", "codex/native"),
                ("effort", "low"),
                ("speed", "default"),
                ("permission", "0"),
            ] {
                assert!(picker_intent(&p, &picker, action, value).is_none());
            }
        }
    }
}
