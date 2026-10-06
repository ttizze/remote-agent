//! The thread list of one shell location: its snapshot, cursor and sync status.
use agent_domain::ThreadId;
use agent_protocol::conversation::{ShellLocation, ShellSnapshot, ShellUpdate, SubscribeShell};
use agent_protocol::models::Project;
use serde::{Deserialize, Serialize};

pub const SHELL_SYNC_ERROR: &str = "Could not synchronize environment data.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellStatus {
    #[default]
    Empty,
    Cached,
    Synchronizing,
    Live,
}

#[derive(Debug, Clone, Default)]
pub struct ShellCache {
    pub location: ShellLocation,
    pub snapshot: Option<ShellSnapshot>,
    pub status: ShellStatus,
    pub error: Option<String>,
    /// Advances on every change a view could show.
    pub revision: u64,
    awaiting_completion: bool,
    /// The connection epoch whose stream delivered the last full snapshot; only
    /// that epoch resumes from the cursor.
    authoritative_epoch: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellApplied {
    pub changed: bool,
    pub removed: Vec<ThreadId>,
}

fn root(project: &Project) -> Option<&str> {
    project.roots.first().map(|root| root.path.as_str())
}

/// A project change can arrive without the identity an earlier list resolved;
/// keep it while the project stays at the same root.
fn retain_identity(previous: Option<&Project>, mut next: Project) -> Project {
    if next.repository_identity.is_none()
        && let Some(previous) = previous
        && previous.repository_identity.is_some()
        && root(previous) == root(&next)
    {
        next.repository_identity = previous.repository_identity.clone();
    }
    next
}

fn retain_identities(previous: &[Project], next: Vec<Project>) -> Vec<Project> {
    next.into_iter()
        .map(|project| {
            let prior = previous.iter().find(|p| p.id == project.id);
            retain_identity(prior, project)
        })
        .collect()
}

/// A full snapshot replaces structure and sequence, even below the cache's.
pub fn merge_shell_snapshot(
    previous: Option<&ShellSnapshot>,
    next: ShellSnapshot,
) -> ShellSnapshot {
    let Some(previous) = previous else {
        return next;
    };
    ShellSnapshot {
        projects: retain_identities(&previous.projects, next.projects),
        ..next
    }
}

fn upsert_by<T>(items: &mut Vec<T>, item: T, same: impl Fn(&T, &T) -> bool) {
    match items.iter_mut().find(|candidate| same(candidate, &item)) {
        Some(existing) => *existing = item,
        None => items.push(item),
    }
}

/// Applies one committed change above the snapshot sequence. Returns whether it
/// applied, and the thread it removed.
pub fn apply_shell_update(
    snapshot: &mut ShellSnapshot,
    update: &ShellUpdate,
) -> (bool, Option<ThreadId>) {
    let sequence = match update {
        ShellUpdate::ThreadUpdated { sequence, .. }
        | ShellUpdate::ThreadRemoved { sequence, .. }
        | ShellUpdate::ProjectUpdated { sequence, .. }
        | ShellUpdate::ProjectRemoved { sequence, .. } => *sequence,
        _ => return (false, None),
    };
    if sequence <= snapshot.snapshot_sequence {
        return (false, None);
    }
    snapshot.snapshot_sequence = sequence;
    let removed = match update {
        ShellUpdate::ThreadUpdated { thread, .. } => {
            upsert_by(&mut snapshot.threads, (**thread).clone(), |a, b| {
                a.id == b.id
            });
            None
        }
        ShellUpdate::ThreadRemoved { thread_id, .. } => {
            snapshot.threads.retain(|thread| &thread.id != thread_id);
            Some(thread_id.clone())
        }
        ShellUpdate::ProjectUpdated { project, .. } => {
            let previous = snapshot.projects.iter().find(|p| p.id == project.id);
            let project = retain_identity(previous, (**project).clone());
            upsert_by(&mut snapshot.projects, project, |a, b| a.id == b.id);
            None
        }
        ShellUpdate::ProjectRemoved { project_id, .. } => {
            snapshot
                .projects
                .retain(|project| &project.id != project_id);
            None
        }
        _ => None,
    };
    (true, removed)
}

impl ShellCache {
    pub fn new(location: ShellLocation) -> Self {
        Self {
            location,
            ..Self::default()
        }
    }
    pub fn from_cache(snapshot: ShellSnapshot) -> Self {
        Self {
            snapshot: Some(snapshot),
            status: ShellStatus::Cached,
            ..Self::default()
        }
    }
    fn status_without_live_data(&self) -> ShellStatus {
        if self.snapshot.is_some() {
            ShellStatus::Cached
        } else {
            ShellStatus::Empty
        }
    }
    pub fn sequence(&self) -> Option<u64> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot_sequence)
    }

    /// A new epoch reloads the authoritative snapshot; a resubscribe within the
    /// epoch that delivered it resumes from the cursor.
    pub fn subscribe(&mut self, epoch: u64) -> SubscribeShell {
        self.awaiting_completion = true;
        self.status = ShellStatus::Synchronizing;
        self.error = None;
        self.revision += 1;
        let resume = self.authoritative_epoch == Some(epoch);
        SubscribeShell {
            after_sequence: self.sequence().filter(|_| resume),
            request_completion_marker: true,
            location: self.location,
        }
    }

    pub fn connecting(&mut self) {
        self.status = ShellStatus::Synchronizing;
        self.error = None;
        self.revision += 1;
    }

    pub fn ready(&mut self) {
        if self.status != ShellStatus::Live {
            self.connecting();
        }
    }

    pub fn disconnected(&mut self) {
        self.awaiting_completion = false;
        self.status = self.status_without_live_data();
        self.revision += 1;
    }

    pub fn stream_error(&mut self) {
        self.awaiting_completion = false;
        self.status = self.status_without_live_data();
        self.error = Some(SHELL_SYNC_ERROR.into());
        self.revision += 1;
    }

    /// Applies one received batch with a single state change.
    pub fn apply(&mut self, items: Vec<ShellUpdate>, epoch: u64) -> ShellApplied {
        let mut out = ShellApplied::default();
        let mut waiting = self.awaiting_completion;
        let mut received_snapshot = false;
        for item in items {
            match item {
                ShellUpdate::Synchronized => {
                    waiting = false;
                    if self.snapshot.is_some() {
                        self.status = ShellStatus::Live;
                        self.error = None;
                        out.changed = true;
                    }
                }
                ShellUpdate::Failed(_) => {}
                ShellUpdate::Snapshot(snapshot) => {
                    self.snapshot = Some(merge_shell_snapshot(self.snapshot.as_ref(), snapshot));
                    received_snapshot = true;
                    self.mark_applied(waiting);
                    out.changed = true;
                }
                ShellUpdate::Projects { projects, .. } => {
                    // The project list opens a resumed stream; it is not a
                    // cursor step, so the replayed rows after it still apply.
                    let Some(snapshot) = self.snapshot.as_mut() else {
                        continue;
                    };
                    snapshot.projects = retain_identities(&snapshot.projects, projects);
                    self.mark_applied(waiting);
                    out.changed = true;
                }
                update => {
                    let Some(snapshot) = self.snapshot.as_mut() else {
                        continue;
                    };
                    let (_, removed) = apply_shell_update(snapshot, &update);
                    out.removed.extend(removed);
                    self.mark_applied(waiting);
                    out.changed = true;
                }
            }
        }
        self.awaiting_completion = waiting;
        if received_snapshot {
            self.authoritative_epoch = Some(epoch);
        }
        if out.changed {
            self.revision += 1;
        }
        out
    }

    fn mark_applied(&mut self, waiting: bool) {
        self.status = if waiting {
            ShellStatus::Synchronizing
        } else {
            ShellStatus::Live
        };
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::*;
    use agent_domain::ThreadShell;
    use agent_protocol::models::{ProjectRoot, RepositoryIdentity, RepositoryLocator};

    fn identity(key: &str) -> RepositoryIdentity {
        RepositoryIdentity {
            canonical_key: format!("github.com/example/{key}"),
            locator: RepositoryLocator {
                source: "git-remote".into(),
                remote_name: "origin".into(),
                remote_url: format!("https://github.com/example/{key}.git"),
            },
            web_url: None,
            root_path: None,
            display_name: None,
            provider: None,
            owner: None,
            name: None,
        }
    }
    fn project(name: &str) -> Project {
        Project {
            id: "project".into(),
            name: name.into(),
            roots: vec![ProjectRoot {
                path: "/workspace/project".into(),
            }],
            ..Default::default()
        }
    }
    fn thread(id: &str) -> ThreadShell {
        let mut row = agent_domain::shell(&thread_state("Thread")).unwrap();
        row.id = ThreadId::new(id).unwrap();
        row
    }
    fn shell(sequence: u64) -> ShellSnapshot {
        ShellSnapshot {
            snapshot_sequence: sequence,
            projects: vec![project("Project")],
            threads: vec![thread("thread")],
        }
    }
    fn project_updated(project: Project, sequence: u64) -> ShellUpdate {
        ShellUpdate::ProjectUpdated {
            sequence,
            project: Box::new(project),
        }
    }

    #[test]
    fn updates_a_thread_in_place_without_moving_its_siblings() {
        let mut snapshot = ShellSnapshot {
            threads: ["a", "b", "c"].map(thread).to_vec(),
            ..shell(0)
        };
        let mut updated = thread("b");
        updated.title = "Streaming".into();
        apply_shell_update(
            &mut snapshot,
            &ShellUpdate::ThreadUpdated {
                sequence: 1,
                thread: Box::new(updated.clone()),
            },
        );
        let ids: Vec<_> = snapshot.threads.iter().map(|t| t.id.to_string()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(snapshot.threads[0], thread("a"));
        assert_eq!(snapshot.threads[1], updated);
        assert_eq!(snapshot.threads[2], thread("c"));
    }

    #[test]
    fn ignores_stale_project_updates_without_mutating_the_snapshot() {
        let original = shell(4);
        for sequence in [3, 4] {
            let mut next = original.clone();
            let (applied, _) = apply_shell_update(
                &mut next,
                &project_updated(project("Stale Title"), sequence),
            );
            assert!(!applied);
            assert_eq!(next, original);
        }
    }

    #[test]
    fn applies_project_updates_and_removals() {
        let mut snapshot = shell(0);
        apply_shell_update(&mut snapshot, &project_updated(project("Updated"), 1));
        assert_eq!(snapshot.projects[0].name, "Updated");
        assert_eq!(snapshot.snapshot_sequence, 1);
        apply_shell_update(
            &mut snapshot,
            &ShellUpdate::ProjectRemoved {
                sequence: 2,
                project_id: "project".into(),
            },
        );
        assert!(snapshot.projects.is_empty());
    }

    #[test]
    fn keeps_prior_repository_identity_when_a_delta_arrives_without_one() {
        let mut snapshot = shell(0);
        let resolved = Project {
            repository_identity: Some(identity("repo")),
            ..project("Project")
        };
        apply_shell_update(&mut snapshot, &project_updated(resolved, 1));
        assert_eq!(
            snapshot.projects[0].repository_identity,
            Some(identity("repo"))
        );
        apply_shell_update(
            &mut snapshot,
            &project_updated(project("Still same root"), 2),
        );
        assert_eq!(snapshot.projects[0].name, "Still same root");
        assert_eq!(
            snapshot.projects[0].repository_identity,
            Some(identity("repo"))
        );
    }

    #[test]
    fn does_not_keep_prior_repository_identity_after_a_root_move() {
        let mut snapshot = shell(0);
        let resolved = Project {
            repository_identity: Some(identity("repo")),
            ..project("Project")
        };
        apply_shell_update(&mut snapshot, &project_updated(resolved, 1));
        let mut moved = project("Project");
        moved.roots[0].path = "/tmp/other-root".into();
        apply_shell_update(&mut snapshot, &project_updated(moved, 2));
        assert_eq!(snapshot.projects[0].roots[0].path, "/tmp/other-root");
        assert_eq!(snapshot.projects[0].repository_identity, None);
    }

    #[test]
    fn merges_full_snapshot_projects_without_dropping_known_identity() {
        let previous = ShellSnapshot {
            projects: vec![Project {
                repository_identity: Some(identity("repo")),
                ..project("Project")
            }],
            ..shell(0)
        };
        let next = merge_shell_snapshot(
            Some(&previous),
            ShellSnapshot {
                projects: vec![project("Refreshed")],
                ..shell(2)
            },
        );
        assert_eq!(next.projects[0].name, "Refreshed");
        assert_eq!(next.projects[0].repository_identity, Some(identity("repo")));
    }

    #[test]
    fn an_authoritative_lower_sequence_reset_replaces_client_ahead_state() {
        let mut ahead = thread("thread");
        ahead.title = "Local-only thread".into();
        let previous = ShellSnapshot {
            snapshot_sequence: 9,
            projects: vec![Project {
                repository_identity: Some(identity("repo")),
                ..project("Client ahead")
            }],
            threads: vec![ahead],
        };
        let mut server = thread("thread");
        server.title = "Server thread".into();
        let next = merge_shell_snapshot(
            Some(&previous),
            ShellSnapshot {
                snapshot_sequence: 3,
                projects: vec![project("Server reset")],
                threads: vec![server],
            },
        );
        assert_eq!(next.snapshot_sequence, 3);
        assert_eq!(next.projects[0].name, "Server reset");
        assert_eq!(next.projects[0].repository_identity, Some(identity("repo")));
        assert_eq!(next.threads[0].title, "Server thread");
    }

    #[test]
    fn removes_a_thread_that_left_the_location() {
        let mut cache = ShellCache::new(ShellLocation::Active);
        cache.subscribe(1);
        cache.apply(vec![ShellUpdate::Snapshot(shell(1))], 1);
        let applied = cache.apply(
            vec![ShellUpdate::ThreadRemoved {
                sequence: 5,
                thread_id: thread_id(),
            }],
            1,
        );
        assert_eq!(applied.removed, [thread_id()]);
        let snapshot = cache.snapshot.unwrap();
        assert!(snapshot.threads.is_empty());
        assert_eq!(snapshot.snapshot_sequence, 5);
    }

    #[test]
    fn publishes_synchronizing_data_then_live_and_stays_live_when_ready() {
        let mut cache = ShellCache::new(ShellLocation::Active);
        cache.subscribe(1);
        cache.connecting();
        cache.apply(vec![ShellUpdate::Snapshot(shell(1))], 1);
        assert_eq!(cache.status, ShellStatus::Synchronizing);
        assert_eq!(cache.snapshot, Some(shell(1)));
        cache.apply(vec![ShellUpdate::Synchronized], 1);
        assert_eq!(cache.status, ShellStatus::Live);
        cache.ready();
        assert_eq!(cache.status, ShellStatus::Live);
        assert_eq!(cache.snapshot, Some(shell(1)));
    }

    #[test]
    fn batches_live_events_into_one_change_per_received_batch() {
        for (chunk, expected) in [(usize::MAX, vec![51]), (16, vec![17, 33, 49, 51])] {
            let mut cache = ShellCache::new(ShellLocation::Active);
            cache.subscribe(1);
            cache.apply(
                vec![
                    ShellUpdate::Snapshot(ShellSnapshot {
                        threads: vec![],
                        ..shell(1)
                    }),
                    ShellUpdate::Synchronized,
                ],
                1,
            );
            let events: Vec<_> = (0..50)
                .map(|index| ShellUpdate::ThreadUpdated {
                    sequence: 2 + index,
                    thread: Box::new(thread(&format!("thread-{index}"))),
                })
                .collect();
            let mut observed = vec![];
            for batch in events.chunks(chunk.min(events.len())) {
                let before = cache.revision;
                cache.apply(batch.to_vec(), 1);
                assert_eq!(cache.revision, before + 1);
                observed.push(cache.sequence().unwrap());
            }
            assert_eq!(observed, expected);
            let ids: Vec<_> = cache
                .snapshot
                .as_ref()
                .unwrap()
                .threads
                .iter()
                .map(|t| t.id.to_string())
                .collect();
            assert_eq!(
                ids,
                (0..50).map(|i| format!("thread-{i}")).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_new_session_reloads_the_authoritative_snapshot_over_a_warm_cache() {
        let mut cache = ShellCache::from_cache(shell(5));
        assert_eq!(cache.status, ShellStatus::Cached);
        let request = cache.subscribe(1);
        assert_eq!(request.after_sequence, None);
        assert!(request.request_completion_marker);
        assert_eq!(cache.status, ShellStatus::Synchronizing);
        assert_eq!(cache.snapshot, Some(shell(5)));
        cache.apply(
            vec![ShellUpdate::Snapshot(shell(9)), ShellUpdate::Synchronized],
            1,
        );
        assert_eq!(cache.status, ShellStatus::Live);
        assert_eq!(cache.sequence(), Some(9));
    }

    #[test]
    fn resubscribes_from_the_in_memory_cursor_within_the_same_session() {
        let mut cache = ShellCache::from_cache(shell(1));
        let mut requested = vec![cache.subscribe(1).after_sequence];
        cache.apply(
            vec![ShellUpdate::Snapshot(shell(10)), ShellUpdate::Synchronized],
            1,
        );
        cache.apply(vec![ShellUpdate::Snapshot(shell(40))], 1);
        requested.push(cache.subscribe(1).after_sequence);
        cache.apply(vec![ShellUpdate::Synchronized], 1);
        requested.push(cache.subscribe(1).after_sequence);
        // A replacement session reloads the snapshot.
        requested.push(cache.subscribe(2).after_sequence);
        assert_eq!(requested, [None, Some(40), Some(40), None]);
    }

    #[test]
    fn a_resumed_project_list_does_not_skip_the_replayed_rows() {
        let mut cache = ShellCache::new(ShellLocation::Active);
        cache.subscribe(1);
        cache.apply(vec![ShellUpdate::Snapshot(shell(4))], 1);
        let mut renamed = thread("thread");
        renamed.title = "Renamed while away".into();
        cache.apply(
            vec![
                ShellUpdate::Projects {
                    sequence: 9,
                    projects: vec![project("Renamed project")],
                },
                ShellUpdate::ThreadUpdated {
                    sequence: 7,
                    thread: Box::new(renamed),
                },
            ],
            1,
        );
        let snapshot = cache.snapshot.unwrap();
        assert_eq!(snapshot.projects[0].name, "Renamed project");
        assert_eq!(snapshot.threads[0].title, "Renamed while away");
        assert_eq!(snapshot.snapshot_sequence, 7);
    }

    #[test]
    fn disconnects_and_failures_keep_cached_data() {
        let mut cache = ShellCache::new(ShellLocation::Active);
        cache.subscribe(1);
        cache.apply(
            vec![ShellUpdate::Snapshot(shell(1)), ShellUpdate::Synchronized],
            1,
        );
        cache.disconnected();
        assert_eq!(cache.status, ShellStatus::Cached);
        cache.stream_error();
        assert_eq!(cache.error.as_deref(), Some(SHELL_SYNC_ERROR));
        assert_eq!(cache.snapshot, Some(shell(1)));
        cache.subscribe(2);
        assert_eq!(cache.error, None);
    }
}
