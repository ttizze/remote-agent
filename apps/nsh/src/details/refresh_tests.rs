use super::*;
use crate::{AppHandles, application::Application};
use agent_core::{presentation::view::Presentation, state::Snapshot};
use agent_protocol::models::{ChangedFile, WorkspaceReview};
use nsh_core::{
    ApproxTextEngine, Core, EventKind, EventPayload, MountItem, NodeId, ViewKind, element::Root,
};
use std::collections::{BTreeMap, VecDeque};

struct ReviewFixture;

#[derive(Clone, PartialEq, Props)]
struct ReviewProps {
    directory: String,
}

impl Component for ReviewFixture {
    type Props = ReviewProps;
    type Event = NoEvent;

    fn view(props: ReviewProps, _: Scope<NoEvent>) -> impl IntoElement {
        let (runtime, application) = Application::new(props.directory.into(), None).unwrap();
        let service = application.clone();
        cleanup(move || runtime.block_on(service.shutdown()).unwrap());
        let initial = review("First");
        let replacement = review("Second");
        let publications = stored(VecDeque::from([
            publication("/fixture", Some(initial.clone()), false),
            publication("/fixture", Some(initial), true),
            publication("/fixture", Some(replacement.clone()), false),
            publication("/fixture", None, true),
            publication("/fixture", Some(replacement.clone()), false),
            publication("/another-workspace", Some(review("Other workspace")), false),
            publication("/fixture", Some(replacement), false),
        ]));
        let publication = signal(Shared(publications.with(|p| p.pop_front().unwrap())));
        let state = signal(Shared(application.snapshot())).read_only();
        let client = stored(application.client.clone());
        provide_context(Connection::new(
            AppHandles {
                application: stored(application),
                state,
                client,
                pairing: computed(|| false),
            },
            computed(|| "fixture".to_owned()),
            computed(move || publication.get()),
            None,
        ));
        view()
            .h_full()
            .child(
                nsh_base::button()
                    .accessibility_label("Advance review")
                    .on_press(move || {
                        publication.set(Shared(publications.with(|p| p.pop_front().unwrap())));
                    }),
            )
            .child(diff("/fixture".into(), "fixture".into()))
    }
}

fn publication(
    cwd: &str,
    review: Option<Arc<WorkspaceReview>>,
    running: bool,
) -> Arc<Presentation> {
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    let pane = Arc::make_mut(snapshot.panes.primary_mut());
    Arc::make_mut(&mut pane.navigation).cwd = "/fixture".into();
    let workspace = Arc::make_mut(&mut pane.workspace);
    workspace.review_cwd = Some(cwd.into());
    workspace.review = review;
    if running {
        Arc::make_mut(&mut snapshot.operations).insert(
            op::OperationKey::WorkspaceReview { pane: "".into() },
            op::OperationState {
                generation: 1,
                phase: op::OperationPhase::Running,
            },
        );
    }
    Presentation::project(Arc::new(snapshot), "", None).unwrap()
}

fn review(text: &str) -> Arc<WorkspaceReview> {
    Arc::new(WorkspaceReview {
        branch: "main".into(),
        additions: 1,
        deletions: 1,
        files: vec![ChangedFile {
            path: "tracked.txt".into(),
            status: "modified".into(),
            additions: Some(1),
            deletions: Some(1),
        }],
        diff: format!(
            "diff --git a/tracked.txt b/tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-original\n+{text}\n"
        ),
    })
}

#[derive(Default)]
struct Mounted {
    lists: BTreeSet<NodeId>,
    texts: BTreeMap<NodeId, String>,
    advance: Option<NodeId>,
}

impl Mounted {
    fn commit(&mut self, core: &mut Core<ApproxTextEngine>, retained: Option<NodeId>) {
        let Some(batch) = core.commit() else { return };
        for item in batch.items {
            match item {
                MountItem::Create {
                    id,
                    kind: ViewKind::Scroll,
                } => {
                    self.lists.insert(id);
                }
                MountItem::Text { id, text, .. } => {
                    self.texts.insert(id, text.text.clone());
                }
                MountItem::ViewProps { id, props }
                    if props.accessibility.label.as_deref() == Some("Advance review") =>
                {
                    self.advance = Some(id);
                }
                MountItem::Remove { id } | MountItem::Delete { id } => {
                    assert_ne!(
                        Some(id),
                        retained,
                        "refresh must keep the native diff list mounted"
                    );
                    self.lists.remove(&id);
                    self.texts.remove(&id);
                }
                _ => {}
            }
        }
    }

    fn until_text(
        &mut self,
        core: &mut Core<ApproxTextEngine>,
        text: &str,
        retained: Option<NodeId>,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            core.run_component_tasks();
            self.commit(core, retained);
            if self.texts.values().any(|shown| shown == text) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "diff did not show {text}"
            );
            std::thread::yield_now();
        }
    }

    fn advance(&self, core: &mut Core<ApproxTextEngine>) {
        assert!(core.component_event(self.advance.unwrap(), EventKind::Press, &EventPayload::None));
    }
}

#[test]
fn refresh_keeps_the_diff_list_mounted_until_replacement_and_clears_invalidated_reviews() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut core = Core::new(
        ApproxTextEngine,
        nsh_core::geometry::size(px(600.0), px(400.0)),
    );
    core.set_components(nsh_base::registry());
    core.set_task_runtime(runtime.handle().clone());
    let directory_path = directory.path().to_string_lossy().into_owned();
    core.mount_root(&Root::new::<ReviewFixture>(move || ReviewProps {
        directory: directory_path.clone(),
    }));
    let mut mounted = Mounted::default();
    mounted.until_text(&mut core, "+First", None);
    assert_eq!(mounted.lists.len(), 1);
    let list = *mounted.lists.first().unwrap();

    mounted.advance(&mut core);
    mounted.commit(&mut core, Some(list));
    assert!(mounted.texts.values().any(|text| text == "+First"));

    // Hold the sole blocking worker so a replacement review cannot finish
    // parsing before the assertion observes the intermediate native view.
    let (release, waiting) = std::sync::mpsc::channel();
    let held = runtime.spawn_blocking(move || {
        let _ = waiting.recv();
    });
    mounted.advance(&mut core);
    core.run_component_tasks();
    mounted.commit(&mut core, Some(list));
    assert!(mounted.texts.values().any(|text| text == "+First"));
    release.send(()).unwrap();
    runtime.block_on(held).unwrap();
    mounted.until_text(&mut core, "+Second", Some(list));
    assert_eq!(mounted.lists, [list].into());

    mounted.advance(&mut core);
    mounted.commit(&mut core, None);
    assert!(mounted.lists.is_empty());
    assert!(!mounted.texts.values().any(|text| text == "+Second"));
    mounted.advance(&mut core);
    mounted.until_text(&mut core, "+Second", None);
    assert_eq!(mounted.lists.len(), 1);
    mounted.advance(&mut core);
    core.run_component_tasks();
    mounted.commit(&mut core, None);
    assert!(mounted.lists.is_empty());
    assert!(!mounted.texts.values().any(|text| text == "+Second"));
    assert!(
        !mounted
            .texts
            .values()
            .any(|text| text == "+Other workspace")
    );
    mounted.advance(&mut core);
    mounted.until_text(&mut core, "+Second", None);
    assert_eq!(mounted.lists.len(), 1);
    drop(core);
}
