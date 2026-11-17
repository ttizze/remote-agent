//! The desktop's first-run setup, which ends once its completion is saved, and
//! the project import it shares with the settings.
use super::*;
use agent_core::{presentation::onboarding::Finish, state::operations as op};

/// Present in the client's data once the setup has ended.
pub(super) const COMPLETED: &str = "onboarding.completed";

impl Application {
    /// Opens the import on the selected Host and scans its history there.
    pub(crate) fn open_import(&self) {
        let state = self.snapshot();
        let host = state.host.clone();
        if self.stop.is_cancelled()
            || !state
                .publications
                .get(&host)
                .is_some_and(|value| value.snapshot.connection.connected)
        {
            return;
        }
        self.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.onboarding = state.onboarding.open_import();
        });
        self.client
            .dispatch(&host, None, Intent::ScanProjects(op::ScanProjects {}));
    }

    pub(crate) fn close_import(&self) {
        self.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.onboarding = state.onboarding.back();
        });
    }

    /// Imports the selected projects; one that brings all of them ends the setup.
    pub(crate) fn import_projects(self: &Arc<Self>) {
        let host = self.snapshot().host.clone();
        let response =
            self.client
                .request(&host, None, Intent::ImportProjects(op::ImportProjects {}));
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        self.jobs.spawn_on(
            async move {
                let imported = tokio::select! {
                    _ = stop.cancelled() => return,
                    result = response.recv() => matches!(result, Ok(Ok(_))),
                };
                if let Some(owner) = owner.upgrade()
                    && imported
                    && owner
                        .client
                        .snapshot(&host)
                        .is_some_and(|snapshot| snapshot.project_import_succeeded())
                {
                    owner.finish_onboarding();
                }
            },
            &self.runtime,
        );
    }

    /// Skipping or finishing the import: a first run saves its completion.
    pub(crate) fn finish_onboarding(self: &Arc<Self>) {
        let next = match self.snapshot().onboarding.finish() {
            Finish::Closed(next) => next,
            Finish::Save => return self.save_onboarding(),
        };
        self.state
            .send_modify(|state| Arc::make_mut(state).onboarding = next);
    }

    fn save_onboarding(self: &Arc<Self>) {
        let path = self.onboarding_path.clone();
        let owner = Arc::downgrade(self);
        self.jobs.spawn_on(
            async move {
                let saved = tokio::task::spawn_blocking(move || {
                    agent_local_host::save_private_bytes(&path, b"completed\n")
                })
                .await
                .map_err(|reason| reason.to_string())
                .and_then(|saved| saved.map_err(|reason| reason.to_string()));
                let Some(owner) = owner.upgrade() else {
                    return;
                };
                owner.state.send_modify(|state| {
                    let state = Arc::make_mut(state);
                    match saved {
                        Ok(()) => state.onboarding = state.onboarding.completed(),
                        Err(reason) => state.failure = Some(reason),
                    }
                });
            },
            &self.runtime,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::presentation::onboarding::OnboardingPage;

    async fn until(app: &Application, ready: impl Fn(&State) -> bool) -> Arc<State> {
        let mut updates = app.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = updates.borrow_and_update().clone();
                if ready(&state) {
                    return state;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("application publication stalled")
    }

    #[test]
    fn a_first_run_ends_once_its_completion_is_saved_and_stays_ended_on_the_next_launch() {
        let root = tempfile::tempdir().unwrap();
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        assert_eq!(
            app.snapshot().onboarding.page(),
            Some(OnboardingPage::Environment)
        );
        // An import cannot open before the selected Host is reached.
        app.open_import();
        assert_eq!(
            app.snapshot().onboarding.page(),
            Some(OnboardingPage::Environment)
        );
        // A failed save keeps the setup, and says why.
        std::fs::create_dir(root.path().join(COMPLETED)).unwrap();
        app.finish_onboarding();
        runtime.block_on(until(&app, |state| state.failure.is_some()));
        assert!(app.snapshot().onboarding.page().is_some());
        std::fs::remove_dir(root.path().join(COMPLETED)).unwrap();
        app.dismiss_failure();
        app.finish_onboarding();
        runtime.block_on(until(&app, |state| state.onboarding.page().is_none()));
        assert!(root.path().join(COMPLETED).is_file());
        runtime.block_on(app.shutdown()).unwrap();
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        assert_eq!(app.snapshot().onboarding.page(), None);
        runtime.block_on(app.shutdown()).unwrap();
    }
}
