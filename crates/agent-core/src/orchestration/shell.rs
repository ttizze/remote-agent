//! Shell data has its own application cursor; transcript events never become
//! sidebar rows. Subscription generations remain owned by the store.
use agent_protocol::orchestration::shell::*;

pub(crate) fn project(project: &ProjectShell) -> crate::models::Project {
    crate::models::Project {
        id: project.id.clone(),
        name: project.title.clone(),
        roots: vec![crate::models::ProjectRoot {
            path: project.workspace_root.clone(),
        }],
        icon: None,
    }
}

pub fn receive(
    current: Option<&ShellSnapshot>,
    item: ShellStreamItem,
    allow_snapshot_reset: bool,
) -> Result<Option<ShellSnapshot>, String> {
    validate_item(&item)?;
    if let ShellStreamItem::Snapshot { snapshot, .. } = item {
        if !allow_snapshot_reset
            && current.is_some_and(|current| snapshot.snapshot_sequence < current.snapshot_sequence)
        {
            return Err("shell snapshot precedes the live cursor".into());
        }
        return Ok(Some(snapshot));
    }
    if item == ShellStreamItem::SnapshotRequired {
        return Ok(None);
    }
    let Some(mut snapshot) = current.cloned() else {
        return if item == ShellStreamItem::Synchronized {
            Ok(None)
        } else {
            Err("shell delta has no snapshot".into())
        };
    };
    let sequence = match &item {
        ShellStreamItem::ProjectUpdated { sequence, .. }
        | ShellStreamItem::ProjectRemoved { sequence, .. }
        | ShellStreamItem::ThreadUpdated { sequence, .. }
        | ShellStreamItem::ThreadRemoved { sequence, .. } => *sequence,
        _ => return Ok(Some(snapshot)),
    };
    // One application event can remove a thread from one location and add it
    // to another. Equal-sequence deltas are idempotent and must all apply.
    if sequence < snapshot.snapshot_sequence {
        return Ok(Some(snapshot));
    }
    match item {
        ShellStreamItem::ProjectUpdated { project, .. } => {
            if let Some(previous) = snapshot
                .projects
                .iter_mut()
                .find(|saved| saved.id == project.id)
            {
                if previous.as_ref() != project.as_ref() {
                    *previous = project.into();
                }
            } else {
                snapshot.projects.push(project.into());
            }
        }
        ShellStreamItem::ProjectRemoved { project_id, .. } => {
            snapshot.projects.retain(|project| project.id != project_id)
        }
        ShellStreamItem::ThreadUpdated {
            location, thread, ..
        } => {
            let (target, other) = match location {
                ShellLocation::Active => (&mut snapshot.threads, &mut snapshot.archived_threads),
                ShellLocation::Archive => (&mut snapshot.archived_threads, &mut snapshot.threads),
            };
            other.retain(|saved| saved.thread.id != thread.thread.id);
            if let Some(previous) = target
                .iter_mut()
                .find(|saved| saved.thread.id == thread.thread.id)
            {
                if previous.as_ref() != thread.as_ref() {
                    *previous = thread.into();
                }
            } else {
                target.push(thread.into());
            }
        }
        ShellStreamItem::ThreadRemoved {
            location,
            thread_id,
            ..
        } => match location {
            ShellLocation::Active => snapshot
                .threads
                .retain(|thread| thread.thread.id != thread_id),
            ShellLocation::Archive => snapshot
                .archived_threads
                .retain(|thread| thread.thread.id != thread_id),
        },
        _ => {}
    }
    snapshot.snapshot_sequence = sequence;
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests;
