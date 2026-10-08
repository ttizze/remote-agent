use super::{Row, notice, page_container, section};
use crate::app::{
    Desktop,
    ui::{color, icon},
};
use agent_core::{
    state::Intent,
    view::usage::{UsagePreferences, UsageSummaryInput},
};
use agent_protocol::usage::PriceOverride;
use chrono::Local;
use gpui_kit::{
    component::{
        Sizable, StyledExt,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::BTreeMap;

pub(super) struct UsageState {
    aliases: Entity<InputState>,
    prices: Entity<InputState>,
    aliases_value: Option<String>,
    prices_value: Option<String>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl UsageState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let aliases = cx.new(|cx| InputState::new(window, cx).placeholder("{\"short-name\":\"provider/model\"}"));
        let prices = cx.new(|cx| InputState::new(window, cx).placeholder("{\"provider/model\":{...}}"));
        let subscriptions = vec![
            cx.subscribe_in(&aliases, window, |view, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    view.commit_usage_preferences(window, cx);
                }
            }),
            cx.subscribe_in(&prices, window, |view, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    view.commit_usage_preferences(window, cx);
                }
            }),
        ];
        Self {
            aliases,
            prices,
            aliases_value: None,
            prices_value: None,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn sync(&mut self, preferences: &UsagePreferences, window: &mut Window, cx: &mut Context<Desktop>) {
        let aliases = serde_json::to_string(
            &preferences
                .model_aliases
                .iter()
                .map(|(model, target)| (model.clone(), target.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap_or_else(|_| "{}".into());
        if self.aliases_value.as_deref() != Some(aliases.as_str()) {
            self.aliases_value = Some(aliases.clone());
            self.aliases
                .update(cx, |input, cx| input.set_value(aliases, window, cx));
        }
        let prices = serde_json::to_string(
            &preferences
                .price_overrides
                .iter()
                .map(|(model, price)| (model.clone(), price.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap_or_else(|_| "{}".into());
        if self.prices_value.as_deref() != Some(prices.as_str()) {
            self.prices_value = Some(prices.clone());
            self.prices
                .update(cx, |input, cx| input.set_value(prices, window, cx));
        }
    }
}

pub(super) fn summary_input(
    snapshot: &agent_core::state::Snapshot,
) -> agent_core::view::usage::UsageSummaryInput {
    let now = Local::now();
    let start = now.date_naive() - chrono::Days::new(30);
    let preferences = snapshot.usage_preferences();
    UsageSummaryInput {
        since_day: start.format("%Y-%m-%d").to_string(),
        until_day: now.format("%Y-%m-%d").to_string(),
        time_zone: now.format("%:z").to_string(),
        resolution: Some(agent_protocol::usage::Resolution::Day),
        since_time: None,
        until_time: None,
        model_aliases: preferences.model_aliases,
        price_overrides: preferences.price_overrides,
    }
}

impl Desktop {
    fn commit_usage_preferences(&mut self, window: &mut Window, cx: &mut Context<Desktop>) {
        let aliases_text = self.settings.usage.aliases.read(cx).value();
        let prices_text = self.settings.usage.prices.read(cx).value();
        let aliases = match serde_json::from_str::<BTreeMap<String, String>>(&aliases_text) {
            Ok(aliases) => aliases,
            Err(_) => {
                self.settings.usage.error = Some("Model aliases must be valid JSON.".into());
                self.settings.usage.aliases_value = None;
                self.settings.usage.prices_value = None;
                self.settings.usage.sync(&self.snapshot.usage_preferences(), window, cx);
                return;
            }
        };
        let prices = match serde_json::from_str::<BTreeMap<String, PriceOverride>>(&prices_text) {
            Ok(prices) => prices,
            Err(_) => {
                self.settings.usage.error = Some("Price overrides must be valid JSON.".into());
                self.settings.usage.aliases_value = None;
                self.settings.usage.prices_value = None;
                self.settings.usage.sync(&self.snapshot.usage_preferences(), window, cx);
                return;
            }
        };
        self.settings.usage.error = None;
        let preferences = UsagePreferences {
            model_aliases: aliases.into_iter().collect(),
            price_overrides: prices.into_iter().collect(),
        };
        self.perform_then(Intent::SetUsagePreferences { preferences }, |view, result, _, _| {
            if result.is_ok() {
                view.perform(Intent::LoadUsageSummary {
                    input: summary_input(&view.snapshot),
                });
            }
        });
    }
}

impl Desktop {
    pub(super) fn render_usage(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        let preferences = self.snapshot.usage_preferences();
        self.settings.usage.sync(&preferences, window, cx);
        let view = self.snapshot.usage_page();
        let refresh = Button::new("refresh-usage")
            .icon(icon("refresh-cw"))
            .ghost()
            .xsmall()
            .label("Refresh")
            .on_click(cx.listener(|view, _, _, _| {
                view.perform(Intent::LoadUsageSummary {
                    input: summary_input(&view.snapshot),
                });
            }));
        let refresh_rates = Button::new("refresh-usage-rates")
            .ghost()
            .xsmall()
            .label("Refresh rates")
            .on_click(cx.listener(|view, _, _, _| {
                view.perform_then(Intent::RefreshUsageRates, |view, result, _, _| {
                    if result.is_ok() {
                        view.perform(Intent::LoadUsageSummary {
                            input: summary_input(&view.snapshot),
                        });
                    }
                });
            }));
        let actions = h_flex()
            .gap_2()
            .child(refresh)
            .child(refresh_rates)
            .into_any_element();
        let summary = section(
            Some("Usage"),
            None,
            Some(actions),
            vec![
                Row::new("Tokens")
                    .description(format!(
                        "{} tokens across {} sessions",
                        view.total_tokens_label, view.sessions
                    ))
                    .render(),
                Row::new("Estimated cost")
                    .description(format!("{} · {}", view.cost_label, view.pricing_status))
                    .render(),
            ],
        );
        let mut rows = Vec::with_capacity(view.rows.len());
        for row in view.rows {
            rows.push(
                Row::new(format!("{} · {}", row.provider, row.model))
                    .description(format!(
                        "{} · {} · {}",
                        row.day,
                        agent_core::view::usage::format::token_count(row.tokens),
                        row.cost_label
                    ))
                    .render(),
            );
        }
        let details = section(Some("By day and model"), None, None, rows);
        let chart_rows = view
            .chart
            .iter()
            .map(|point| {
                Row::new(point.day.clone())
                    .description(format!(
                        "{} tokens · {}",
                        agent_core::view::usage::format::token_count(point.tokens),
                        agent_core::view::usage::format::usd(point.cost_usd)
                    ))
                    .render()
            })
            .collect();
        let chart = section(Some("Daily trend"), None, None, chart_rows);
        let preference_error = self
            .settings
            .usage
            .error
            .clone()
            .map(|error| notice(error))
            .unwrap_or_else(|| div().into_any_element());
        let mut preference_rows = vec![preference_error];
        preference_rows.extend([
            Row::new("Model aliases")
                .description("Map a transcript model name to the model used for pricing, as a JSON object.")
                .control(
                    Input::new(&self.settings.usage.aliases)
                        .small()
                        .w(px(360.))
                        .aria_label("Model aliases"),
                )
                .render(),
            Row::new("Price overrides")
                .description("Override input and output prices per million tokens, as a JSON object.")
                .control(
                    Input::new(&self.settings.usage.prices)
                        .small()
                        .w(px(360.))
                        .aria_label("Price overrides"),
                )
                .render(),
        ]);
        let preferences_section = section(
            Some("Pricing and model mapping"),
            None,
            None,
            preference_rows,
        );
        let limit_rows = self
            .snapshot
            .usage_limits()
            .into_iter()
            .map(|account| {
                let windows = account
                    .windows
                    .into_iter()
                    .map(|window| format!("{} {}% left", window.label, window.remaining_percent))
                    .collect::<Vec<_>>();
                let credits = (account.reset_credit_count > 0)
                    .then(|| format!("{} reset credit(s)", account.reset_credit_count));
                Row::new(account.email.unwrap_or(account.id)).description(
                    windows
                        .into_iter()
                        .chain(credits)
                        .chain(account.external_label)
                        .collect::<Vec<_>>()
                        .join(" · "),
                )
                .render()
            })
            .collect();
        let limits = section(Some("Limits"), None, None, limit_rows);
        let notice = view.error.map(|error| {
            v_flex()
                .gap_1()
                .px_4()
                .py_3()
                .rounded(px(10.))
                .bg(color("errorSurface"))
                .text_color(color("errorForeground"))
                .child(error)
                .into_any_element()
        });
        page_container(
            896.,
            vec![
                summary.into_any_element(),
                notice.unwrap_or_else(|| div().into_any_element()),
                limits.into_any_element(),
                chart.into_any_element(),
                details.into_any_element(),
                preferences_section.into_any_element(),
            ],
        )
    }
}
