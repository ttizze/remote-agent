//! Coherent native presentation, projected independently of state transitions.
use super::{
    conversation::{RenderedConversation, project_conversation},
    list::ThreadList,
};
use crate::state::{Changed, PaneState, Snapshot};
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Presentation {
    pub snapshot: Arc<Snapshot>,
    pub pane_id: String,
    pub list: Option<Arc<ThreadList>>,
    pub conversation: Option<Arc<RenderedConversation>>,
    pub composer: Arc<super::composer::ComposerPresentation>,
    pub context: Arc<super::context::ConversationContext>,
    pub agents: Arc<Vec<super::agents::AgentSummary>>,
    pub settings: Arc<super::settings::SettingsPresentation>,
}

#[derive(Clone, Copy)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PresentationChanges {
    pub changed: bool,
    pub groups: Changed,
    pub list: bool,
    pub conversation: bool,
    pub composer: bool,
    pub composer_controls: bool,
    pub context: bool,
    pub agents: bool,
    pub settings: bool,
    pub reconnected: bool,
}

impl Presentation {
    fn metadata(snapshot: Arc<Snapshot>, pane_id: &str, previous: Option<&Self>) -> Option<Self> {
        let pane = snapshot.panes.get(pane_id)?.clone();
        let composer = super::composer::ComposerPresentation::project(
            &snapshot,
            &pane,
            previous.map(|value| &value.composer),
        );
        let context = super::context::ConversationContext::project(
            &snapshot,
            &pane,
            pane_id,
            previous.map(|value| &value.context),
        );
        let settings = super::settings::SettingsPresentation::project(
            &snapshot,
            &pane,
            composer.controls.provider,
            previous,
        );
        Some(Self {
            pane_id: pane_id.into(),
            snapshot,
            list: None,
            conversation: None,
            composer,
            context,
            agents: super::agents::empty(),
            settings,
        })
    }

    pub(crate) fn pending(snapshot: Arc<Snapshot>, pane_id: &str) -> Option<Arc<Self>> {
        Self::metadata(snapshot, pane_id, None).map(Arc::new)
    }

    pub fn project(
        snapshot: Arc<Snapshot>,
        pane_id: &str,
        previous: Option<&Self>,
    ) -> Option<Arc<Self>> {
        let previous = previous.filter(|value| value.pane_id == pane_id);
        let mut next = Self::metadata(snapshot, pane_id, previous)?;
        let snapshot = next.snapshot.clone();
        let pane = next.pane().clone();
        let list = if !pane_id.is_empty() {
            None
        } else if let Some(previous) = previous
            && snapshot
                .list_source()
                .equivalent(&previous.snapshot.list_source())
        {
            previous.list.clone()
        } else {
            let source = snapshot.list_source();
            if snapshot.lists.recent_chats {
                source.recent_thread_list()
            } else {
                source.thread_list()
            }
            .map(Arc::new)
        };
        let source = snapshot.conversation_thread(&pane).map(|source| {
            // Pending input before session creation has no native Thread. Retain
            // that placeholder across unrelated publications as well.
            previous
                .and_then(|previous| previous.conversation.as_ref())
                .filter(|previous| source.id.is_none() && previous.source.id.is_none())
                .map_or(source, |previous| previous.source.clone())
        });
        let conversation = source.map(|source| {
            project_conversation(
                snapshot.catalog.ui_language,
                &pane.navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                source,
                &previous.and_then(|previous| previous.conversation.clone()),
            )
        });
        next.agents = super::agents::project(&snapshot, &pane, previous);
        next.list = list;
        next.conversation = conversation;
        Some(Arc::new(next))
    }

    /// The projected pane; a presentation exists only while its pane is open.
    pub fn pane(&self) -> &Arc<PaneState> {
        self.snapshot
            .panes
            .get(&self.pane_id)
            .expect("a presentation is projected only for an open pane")
    }

    fn same_context(&self, latest: &Snapshot) -> bool {
        latest.panes.get(&self.pane_id).is_some_and(|pane| {
            self.snapshot.catalog.ui_language == latest.catalog.ui_language
                && self.pane().navigation.draft_key == pane.navigation.draft_key
                && self.pane().navigation.thread_id == pane.navigation.thread_id
                && self.snapshot.connection.epoch == latest.connection.epoch
                && self.pane().epoch == pane.epoch
                && self.snapshot.connection.storage_scope == latest.connection.storage_scope
        })
    }

    /// State acknowledgments remain immediate while native content catches up.
    /// A retired pane never falls back to another pane's selection.
    pub(crate) fn current(snapshot: Arc<Snapshot>, content: &Arc<Self>) -> Option<Arc<Self>> {
        if Arc::ptr_eq(&snapshot, &content.snapshot) {
            return Some(content.clone());
        }
        let mut next = Self::metadata(snapshot, &content.pane_id, Some(content))?;
        let same_host =
            next.snapshot.connection.storage_scope == content.snapshot.connection.storage_scope;
        let same_conversation = same_host
            && next.pane().navigation.draft_key == content.pane().navigation.draft_key
            && next.pane().navigation.thread_id == content.pane().navigation.thread_id;
        if same_host {
            next.list = content.list.clone();
        }
        if same_conversation {
            next.conversation = content.conversation.clone();
            if next.pane().observed_agents == content.pane().observed_agents
                && next.pane().observed_agents.is_some()
            {
                next.agents = content.agents.clone();
            }
        }
        Some(Arc::new(next))
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Presentation {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn from_snapshot(
        snapshot: Arc<Snapshot>,
        pane_id: String,
    ) -> Result<Arc<Self>, crate::client::AgentError> {
        Self::project(snapshot, &pane_id, None)
            .ok_or_else(|| crate::client::error("pane is closed"))
    }
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn empty(language: crate::localization::UiLanguage) -> Arc<Self> {
        let mut snapshot = Snapshot::default();
        snapshot.catalog.ui_language = language;
        Self::pending(Arc::new(snapshot), "").expect("primary pane always exists")
    }
    pub fn pane_id(&self) -> String {
        self.pane_id.clone()
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.clone()
    }
    pub fn conversation(&self) -> Option<Arc<RenderedConversation>> {
        self.conversation.clone()
    }
    pub fn composer(&self) -> Arc<super::composer::ComposerPresentation> {
        self.composer.clone()
    }
    /// The compact workspace-change summary shown above a conversation's
    /// composer by native clients.
    pub fn composer_review(&self, is_side_chat: bool) -> Option<super::composer::ComposerReview> {
        self.pane().composer_review(is_side_chat)
    }
    pub fn conversation_context(&self) -> Arc<super::context::ConversationContext> {
        self.context.clone()
    }
    pub fn thread_list(&self) -> Option<ThreadList> {
        self.list.as_deref().cloned()
    }
    pub fn agents(&self) -> Vec<super::agents::AgentSummary> {
        self.agents.as_ref().clone()
    }
    pub fn settings(&self) -> Arc<super::settings::SettingsPresentation> {
        self.settings.clone()
    }
    pub fn changes(&self, previous: Arc<Self>) -> PresentationChanges {
        use crate::state::Same;
        let list = !self.list.same(&previous.list);
        let conversation = !self.conversation.same(&previous.conversation);
        let composer = !self.composer.equivalent(&previous.composer);
        let context = self.context.as_ref() != previous.context.as_ref();
        let agents = !self.agents.same(&previous.agents);
        let settings = !self.settings.equivalent(&previous.settings);
        let groups = self
            .snapshot
            .changes_from(&previous.snapshot)
            .selected_pane(!Arc::ptr_eq(self.pane(), previous.pane()));
        PresentationChanges {
            changed: groups.any
                || list
                || conversation
                || composer
                || context
                || agents
                || settings,
            groups,
            list,
            conversation,
            composer,
            composer_controls: self.composer.controls != previous.composer.controls,
            context,
            agents,
            settings,
            reconnected: self.snapshot.connection.connected
                && self.snapshot.connection.connection_generation
                    != previous.snapshot.connection.connection_generation,
        }
    }
}

/// One ordered projector per Store. Watch coalesces queued snapshots, so a long
/// history cannot queue one expensive job per token or block dispatch/recovery.
/// A stream may publish an earlier coherent prefix while the next is queued;
/// results for a replaced navigation or connection are always discarded.
pub(crate) fn observe(
    mut snapshots: watch::Receiver<Arc<Snapshot>>,
    initial: Arc<Presentation>,
    stop: CancellationToken,
    project: impl Fn(Arc<Snapshot>, &str, Option<&Presentation>) -> Option<Arc<Presentation>>
    + Send
    + Sync
    + 'static,
) -> watch::Receiver<Arc<Presentation>> {
    let pane = initial.pane_id.clone();
    let (publish, presentations) = watch::channel(initial);
    let project = Arc::new(project);
    tokio::spawn(async move {
        let mut previous = None;
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            if !publish_current(snapshot.clone(), &publish) {
                return;
            }
            let held = previous.clone();
            let project = project.clone();
            let target = pane.clone();
            let mut projecting =
                tokio::task::spawn_blocking(move || project(snapshot, &target, held.as_deref()));
            let mut catch_up = false;
            let next = loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => return,
                    result = &mut projecting => match result { Ok(Some(next)) => break next, _ => return },
                    result = snapshots.changed() => {
                        if result.is_err() { return; }
                        let latest = snapshots.borrow_and_update().clone();
                        // Input/navigation acknowledge immediately; only the coherent
                        // transcript prefix waits for the worker.
                        if !publish_current(latest, &publish) { return; }
                        catch_up = true;
                    }
                }
            };
            let latest = snapshots.borrow().clone();
            if next.same_context(&latest) {
                let Some(current) = Presentation::current(latest, &next) else {
                    return;
                };
                publish.send_replace(current);
                previous = Some(next);
            }
            if catch_up || snapshots.has_changed().unwrap_or(false) {
                continue;
            }
            tokio::select! {
                biased;
                _ = stop.cancelled() => return,
                result = snapshots.changed() => if result.is_err() { return; },
            }
        }
    });
    presentations
}

fn publish_current(snapshot: Arc<Snapshot>, publish: &watch::Sender<Arc<Presentation>>) -> bool {
    let previous = publish.borrow().clone();
    let Some(current) = Presentation::current(snapshot, &previous) else {
        return false;
    };
    if current.changes(previous).changed {
        publish.send_replace(current);
    }
    true
}
