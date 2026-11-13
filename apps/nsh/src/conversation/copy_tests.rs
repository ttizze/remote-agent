use super::*;
use nsh_core::{
    ApproxTextEngine, Core, EventKind, EventPayload, MountItem, NodeId,
    clipboard::{ClipboardBackend, MemoryClipboard},
    element::Root,
    time::Clock,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

struct CopyFixture;

impl Component for CopyFixture {
    type Props = ();
    type Event = NoEvent;

    fn view(_: (), _: Scope<NoEvent>) -> impl IntoElement {
        let version = signal(0);
        view()
            .child(copy_action("発言をコピー", move || {
                match version.get_untracked() {
                    0 => Some("original **message**".into()),
                    1 => Some("updated 日本語 message".into()),
                    _ => None,
                }
            }))
            .child(copy_action("回答をコピー", || Some("answer".into())))
            .child(
                view()
                    .text_color(look::muted())
                    .child(crate::ui::icon(crate::ui::Icon::CHECK, 12.0)),
            )
            .child(
                nsh_base::button()
                    .accessibility_label("Update message")
                    .on_press(move || version.update(|version| *version += 1)),
            )
    }
}

fn commit(
    core: &mut Core<ApproxTextEngine>,
    labels: &mut BTreeMap<NodeId, String>,
    icons: &mut BTreeMap<NodeId, String>,
) {
    core.run_component_tasks();
    if let Some(batch) = core.commit() {
        for item in batch.items {
            match item {
                MountItem::ViewProps { id, props } => {
                    if let Some(label) = props.accessibility.label {
                        labels.insert(id, label.to_string());
                    }
                }
                MountItem::AccessibilityLabel { id, label } => {
                    if let Some(label) = label {
                        labels.insert(id, label.to_string());
                    } else {
                        labels.remove(&id);
                    }
                }
                MountItem::Delete { id } => {
                    labels.remove(&id);
                    icons.remove(&id);
                }
                MountItem::Image {
                    id,
                    source: Some(source),
                    ..
                } => {
                    icons.insert(id, source.uri.to_string());
                }
                MountItem::Canvas { id, drawing } => {
                    icons.insert(id, format!("{:?}", drawing.ops));
                }
                _ => {}
            }
        }
    }
    core.run_component_tasks();
}

#[test]
fn copying_messages_confirms_each_button_restarts_on_repeat_and_restores_its_label() {
    let clock = Clock::manual(std::time::SystemTime::UNIX_EPOCH);
    let _clock = clock.enter();
    let clipboard = Arc::new(MemoryClipboard::default());
    let mut core = Core::new(
        ApproxTextEngine,
        nsh_core::geometry::size(px(320.0), px(200.0)),
    );
    core.set_components(nsh_base::registry());
    core.set_clipboard(Some(clipboard.clone()));
    core.mount_root(&Root::new::<CopyFixture>(|| ()));
    let mut labels = BTreeMap::new();
    let mut icons = BTreeMap::new();
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(icons.len(), 3);
    let copy_icon = icons
        .values()
        .find(|icon| icons.values().filter(|other| other == icon).count() == 2)
        .unwrap()
        .clone();
    let check_icon = icons
        .values()
        .find(|icon| **icon != copy_icon)
        .unwrap()
        .clone();
    assert_eq!(icons.values().filter(|icon| **icon == copy_icon).count(), 2);
    let find = |label| {
        *labels
            .iter()
            .find(|(_, shown)| shown.as_str() == label)
            .unwrap()
            .0
    };
    let (user, answer, update) = (
        find(tr("発言をコピー")),
        find(tr("回答をコピー")),
        find("Update message"),
    );
    let press = |core: &mut Core<ApproxTextEngine>, button| {
        assert!(core.component_event(button, EventKind::Press, &EventPayload::None));
    };
    press(&mut core, user);
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(clipboard.text().as_deref(), Some("original **message**"));
    assert_eq!(labels[&user], tr("コピーしました"));
    assert_eq!(labels[&answer], tr("回答をコピー"));
    assert_eq!(
        icons.values().filter(|icon| **icon == check_icon).count(),
        2
    );

    clock.advance_to(clock.now() + Duration::from_secs(1));
    press(&mut core, update);
    press(&mut core, user);
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(clipboard.text().as_deref(), Some("updated 日本語 message"));
    clock.advance_to(clock.now() + Duration::from_millis(1500));
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(labels[&user], tr("コピーしました"));
    assert_eq!(
        icons.values().filter(|icon| **icon == check_icon).count(),
        2
    );
    clock.advance_to(clock.now() + Duration::from_millis(500));
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(labels[&user], tr("発言をコピー"));
    assert_eq!(icons.values().filter(|icon| **icon == copy_icon).count(), 2);

    press(&mut core, answer);
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(clipboard.text().as_deref(), Some("answer"));
    assert_eq!(labels[&answer], tr("コピーしました"));
    assert_eq!(labels[&user], tr("発言をコピー"));
    assert_eq!(
        icons.values().filter(|icon| **icon == check_icon).count(),
        2
    );
    press(&mut core, update);
    press(&mut core, user);
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(clipboard.text().as_deref(), Some("answer"));
    assert_eq!(labels[&user], tr("発言をコピー"));
    clock.advance_to(clock.now() + Duration::from_secs(2));
    commit(&mut core, &mut labels, &mut icons);
    assert_eq!(labels[&answer], tr("回答をコピー"));
    assert_eq!(icons.values().filter(|icon| **icon == copy_icon).count(), 2);
}
