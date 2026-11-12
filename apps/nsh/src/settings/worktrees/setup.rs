use super::*;
use agent_protocol::workspace::SetupScript;

pub(super) fn editor(
    inputs: Signal<Option<WorktreeSettings>>,
    project: Signal<Option<String>>,
    enabled: impl Fn() -> bool + Copy + 'static,
    edit: impl Fn(&dyn Fn(&mut WorktreeSettings)) + Copy + 'static,
) -> View {
    let inherited = move || {
        project.get().is_some_and(|id| {
            inputs.with(|settings| {
                settings
                    .as_ref()
                    .is_none_or(|settings| !settings.project_setup_scripts.contains_key(&id))
            })
        })
    };
    let script = computed(move || {
        inputs.with(|settings| {
            settings
                .as_ref()
                .and_then(|settings| {
                    project
                        .get()
                        .and_then(|id| settings.project_setup_scripts.get(&id))
                        .or(settings.setup_script.as_ref())
                })
                .cloned()
        })
    });
    let change = move |update: &dyn Fn(&mut SetupScript)| {
        let project = project.get_untracked();
        edit(&|settings| {
            let empty = || SetupScript {
                name: String::new(),
                command: String::new(),
                asynchronous: false,
            };
            let target = match &project {
                Some(id) => settings
                    .project_setup_scripts
                    .entry(id.clone())
                    .or_insert_with(empty),
                None => settings.setup_script.get_or_insert_with(empty),
            };
            update(target);
        });
    };
    let editable = move || enabled() && !inherited();
    view()
        .gap_3()
        .child(heading(tr("設定スクリプト")))
        .child(when(
            move || project.with(Option::is_some),
            move || {
                ui::setting(
                    tr("すべてのプロジェクトと同じ設定"),
                    || "".into(),
                    switch(
                        tr("すべてのプロジェクトと同じ設定"),
                        inherited,
                        enabled,
                        move |inherit| {
                            let Some(id) = project.get_untracked() else {
                                return;
                            };
                            edit(&|settings| {
                                if inherit {
                                    settings.project_setup_scripts.remove(&id);
                                } else {
                                    let script =
                                        settings.setup_script.clone().unwrap_or_else(|| {
                                            SetupScript {
                                                name: String::new(),
                                                command: String::new(),
                                                asynchronous: false,
                                            }
                                        });
                                    settings.project_setup_scripts.insert(id.clone(), script);
                                }
                            });
                        },
                    ),
                )
            },
        ))
        .child(field(
            tr("スクリプト名"),
            move || {
                script.with(|script| {
                    script
                        .as_ref()
                        .map(|script| script.name.clone())
                        .unwrap_or_default()
                })
            },
            editable,
            move |name| change(&|script| script.name = name.into()),
        ))
        .child(
            field(
                tr("実行するコマンド"),
                move || {
                    script.with(|script| {
                        script
                            .as_ref()
                            .map(|script| script.command.clone())
                            .unwrap_or_default()
                    })
                },
                editable,
                move |command| change(&|script| script.command = command.into()),
            )
            .multiline(true)
            .label(tr("実行するコマンド"))
            .min_h(px(72.)),
        )
        .child(ui::setting(
            tr("エージェントと並行して実行"),
            || tr("オフの場合、スクリプトの成功を待ってからエージェントを開始します。").into(),
            switch(
                tr("エージェントと並行して実行"),
                move || {
                    script.with(|script| script.as_ref().is_some_and(|script| script.asynchronous))
                },
                editable,
                move |value| change(&|script| script.asynchronous = value),
            ),
        ))
}
