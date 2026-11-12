use super::*;
use crate::l10n::tr;
use crate::ui::{self, Icon};
use agent_core::presentation::conversation::{PreparationDetail, PreparationStep};

pub(super) fn card(
    connection: Connection,
    row: Memo<Option<Row>>,
    clock: Signal<std::time::SystemTime>,
) -> View {
    let setup =
        computed(move || row.with(|row| row.as_ref().and_then(|row| row.preparation.clone())));
    let opened = signal(false);
    let workspace = use_context::<crate::workbench::Workbench>();
    let external = use_context::<crate::native::ExternalActions>();
    view().w_full().px_1().text_size(px(14.)).text_color(look::muted())
        .child(view().flex_row().items_center().pb_2().mb_1().border_b(px(1.)).border_color(look::rule())
            .child(each(move || vec![setup.with(|setup| setup.as_ref().is_some_and(|setup| setup.shimmer))], |moving: &bool| *moving, move |moving, _| {
                let label = move || setup.with(|setup| setup.as_ref().map(|setup| setup.text.clone()).unwrap_or_default());
                if moving.get_untracked() { crate::shimmer::line(None::<fn() -> View>, label, crate::shimmer::Sweep::Focus).into_element() }
                else { text(label).flex_1().into_element() }
            }))
            .child(text(move || setup.with(|setup| setup.as_ref().map(|setup| elapsed(setup.started_at_ms, setup.ended_at_ms, clock)).unwrap_or_default())).text_size(px(12.))))
        .child(each(
            move || setup.with(|setup| setup.as_ref().filter(|setup| setup.show_steps).map(|setup| setup.steps.clone()).unwrap_or_default()),
            |step: &PreparationStep| step.id.clone(),
            move |step, _| stage(step, clock),
        ))
        .child(text(move || setup.with(|setup| setup.as_ref().and_then(|setup| setup.error.clone()).unwrap_or_default())).text_size(px(12.)).text_color(look::DESTRUCTIVE))
        .child(view().flex_row().flex_wrap().gap_2().ml(px(30.)).mt_1()
            .child(nsh_base::button().on_press(move || opened.set(!opened.get_untracked())).child(text(move || format!("{} {}", if opened.get() { "⌄" } else { "›" }, tr("詳細")))).text_size(px(12.)))
            .child(when(move || setup.with(|setup| setup.as_ref().is_some_and(|setup| setup.terminal.is_some())) && (workspace.is_some() || external.is_some()), move || {
                nsh_base::button().on_press(move || { if let Some(terminal) = setup.with_untracked(|setup| setup.as_ref().and_then(|setup| setup.terminal.clone())) {
                    if let Some(workspace) = workspace { workspace.open_setup_terminal(terminal); }
                    else if let Some(external) = external { external.emit(serde_json::json!({"action":"setupTerminal", "text":serde_json::json!({"handle":terminal.handle,"cwd":terminal.cwd}).to_string()})); }
                } }).child(text(tr("ターミナルを開く"))).text_size(px(12.))
            }))
            .child(when(move || setup.with(|setup| setup.as_ref().is_some_and(|setup| setup.work_locally.is_some())), move || {
                nsh_base::button().on_press(move || { if let Some(retry) = setup.with_untracked(|setup| setup.as_ref().and_then(|setup| setup.work_locally.clone())) { connection.dispatch(Intent::RetryPreparation(retry)); } }).child(text(tr("ローカルで作業"))).text_size(px(12.))
            }))
            .child(when(move || setup.with(|setup| setup.as_ref().is_some_and(|setup| setup.cancel.is_some())), move || {
                nsh_base::button().on_press(move || { if let Some(cancel) = setup.with_untracked(|setup| setup.as_ref().and_then(|setup| setup.cancel.clone())) { connection.dispatch(Intent::Interrupt(cancel)); } }).child(text(tr("キャンセル"))).text_size(px(12.))
            }))
            .child(when(move || setup.with(|setup| setup.as_ref().is_some_and(|setup| setup.retry.is_some())), move || {
                nsh_base::button().on_press(move || { if let Some(retry) = setup.with_untracked(|setup| setup.as_ref().and_then(|setup| setup.retry.clone())) { connection.dispatch(Intent::RetryPreparation(retry)); } }).child(text(tr("再試行"))).text_size(px(12.))
            })))
        .child(when(move || opened.get(), move || view().ml(px(30.)).mt_1().child(each(
            move || setup.with(|setup| setup.as_ref().map(|setup| setup.details.clone()).unwrap_or_default()),
            |detail: &PreparationDetail| detail.label.clone(),
            |detail, _| text(move || detail.with(|detail| format!("{}  {}", detail.label, detail.value))).text_size(px(12.)).selectable(true),
        ))))
}

fn stage(step: ReadSignal<PreparationStep>, clock: Signal<std::time::SystemTime>) -> View {
    view()
        .w_full()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .min_h(px(28.))
                .opacity(move || {
                    if step.with(|step| step.state == "pending") {
                        0.4_f32
                    } else {
                        1.0_f32
                    }
                })
                .child(view().w(px(24.)).items_center().child(each(
                    move || vec![step.with(|step| step.state.clone())],
                    |state: &String| state.clone(),
                    |state, _| {
                        let state = state.get_untracked();
                        if state == "skipped" {
                            text("−").into_element()
                        } else {
                            ui::icon(
                                match state.as_str() {
                                    "done" => Icon::CHECK,
                                    "running" => Icon::LOADER,
                                    "failed" => Icon::CLOSE,
                                    "warning" => Icon::ALERT,
                                    _ => Icon::STATUS_WORKING,
                                },
                                16.,
                            )
                            .into_element()
                        }
                    },
                )))
                .child(view().flex_1().min_w_0().child(each(
                    move || vec![step.with(|step| step.state == "running")],
                    |running: &bool| *running,
                    move |running, _| {
                        let label = move || step.with(|step| step.label.clone());
                        if running.get_untracked() {
                            crate::shimmer::line(
                                None::<fn() -> View>,
                                label,
                                crate::shimmer::Sweep::Focus,
                            )
                            .into_element()
                        } else {
                            text(label).number_of_lines(1).into_element()
                        }
                    },
                )))
                .child(
                    text(move || step.with(|step| step.detail.clone().unwrap_or_default()))
                        .text_size(px(12.)),
                )
                .child(
                    text(move || {
                        step.with(|step| {
                            if matches!(step.state.as_str(), "pending" | "skipped") {
                                String::new()
                            } else {
                                elapsed(step.started_at_ms, step.ended_at_ms, clock)
                            }
                        })
                    })
                    .text_size(px(12.)),
                ),
        )
        .child(when(
            move || step.with(|step| !step.tail.is_empty()),
            move || {
                text(move || step.with(|step| step.tail.join("\n")))
                    .ml(px(30.))
                    .p_2()
                    .bg(look::CODE)
                    .rounded(px(6.))
                    .font_family("monospace")
                    .text_size(px(11.))
                    .selectable(true)
            },
        ))
}

fn elapsed(start: Option<u64>, end: Option<u64>, clock: Signal<std::time::SystemTime>) -> String {
    let Some(start) = start else {
        return String::new();
    };
    let now = clock
        .try_get()
        .unwrap_or_else(std::time::SystemTime::now)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    agent_core::presentation::conversation::preparation_elapsed(
        crate::l10n::language(),
        start,
        end,
        now,
    )
}
