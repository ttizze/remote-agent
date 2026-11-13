use super::*;
use crate::{AppHandles, application::Application, shared::Shared};
use agent_core::{presentation::view::Presentation, state::Snapshot};
use nsh_core::{
    ApproxTextEngine, Core, EventKind, EventPayload, MountItem, NodeId,
    element::Root,
    keymap::{KeyMatch, Keymap, Keystroke},
};

struct FileTreeFixture;

#[derive(Clone, PartialEq, Props)]
struct FixtureProps {
    directory: String,
}

impl Component for FileTreeFixture {
    type Props = FixtureProps;
    type Event = NoEvent;

    fn view(props: FixtureProps, _: Scope<NoEvent>) -> impl IntoElement {
        let (runtime, application) = Application::new(props.directory.into(), None).unwrap();
        let service = application.clone();
        cleanup(move || runtime.block_on(service.shutdown()).unwrap());
        let state = signal(Shared(application.snapshot())).read_only();
        let client = stored(application.client.clone());
        provide_context(Connection::new(
            AppHandles {
                application: stored(application),
                state,
                client,
                pairing: computed(|| false),
            },
            computed(|| "fixture".into()),
            computed(|| {
                Shared(Presentation::project(Arc::new(Snapshot::default()), "", None).unwrap())
            }),
            None,
        ));
        provide_context(Workbench::new());
        provide_context(Browser::new(Preview::new()));
        tree("fixture-tree".into(), computed(|| None))
    }
}

#[derive(Default)]
struct Mounted {
    area: Option<NodeId>,
    keys: Arc<Keymap>,
}

impl Mounted {
    fn commit(&mut self, core: &mut Core<ApproxTextEngine>) {
        while let Some(batch) = core.commit() {
            for item in batch.items {
                match item {
                    MountItem::ViewProps { id, props } if props.focusable == Some(true) => {
                        self.area = Some(id);
                    }
                    MountItem::Keymap(keys) => self.keys = keys,
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn tree_keys_follow_focus_and_leave_text_editing_to_native_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_string_lossy().into_owned();
    let mut core = Core::new(
        ApproxTextEngine,
        nsh_core::geometry::size(px(600.0), px(400.0)),
    );
    core.set_components(nsh_base::registry());
    core.mount_root(&Root::new::<FileTreeFixture>(move || FixtureProps {
        directory: path.clone(),
    }));
    let mut mounted = Mounted::default();
    mounted.commit(&mut core);
    assert!(
        mounted.keys.is_empty(),
        "an unfocused tree must not take keys"
    );
    let area = mounted.area.expect("the tree must have a focusable area");

    // The native responder can move to a composer, search field or file
    // editor before its blur event reaches Core. The table must already
    // let those fields edit. Repeated focus changes must restore tree keys.
    for _ in 0..2 {
        assert!(core.component_event(area, EventKind::Focus, &EventPayload::None));
        mounted.commit(&mut core);
        for key in [
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "enter",
            "space",
            "shift+up",
            "shift+down",
        ] {
            let key = Keystroke::parse_with(key, true).unwrap();
            for repeat in [false, true] {
                assert!(matches!(
                    mounted.keys.find(key, false, repeat),
                    KeyMatch::Fire(_)
                ));
                assert_eq!(mounted.keys.find(key, true, repeat), KeyMatch::None);
            }
        }
        assert!(core.component_event(area, EventKind::Blur, &EventPayload::None));
        mounted.commit(&mut core);
        assert!(
            mounted.keys.is_empty(),
            "a blurred tree must release its keys"
        );
    }
}
