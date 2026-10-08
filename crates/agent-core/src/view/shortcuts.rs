//! App-launch shortcut records shared by native launchers.
use serde::{Deserialize, Serialize};

pub const MAX_RECENT_THREAD_SHORTCUTS: usize = 3;
pub const NEW_THREAD_SHORTCUT_ID: &str = "new-thread";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RecentThreadShortcut {
    pub project_id: String,
    pub thread_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AppShortcut {
    pub id: String,
    pub title: String,
    pub destination: String,
}

pub fn thread_destination(thread: &RecentThreadShortcut) -> String {
    format!(
        "/threads/{}/{}",
        percent_encode(&thread.project_id),
        percent_encode(&thread.thread_id)
    )
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![char::from(byte)]
            }
            byte => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

/// Builds the static new-thread shortcut followed by the most recent threads.
pub fn build(recents: &[RecentThreadShortcut]) -> Vec<AppShortcut> {
    let mut shortcuts = vec![AppShortcut {
        id: NEW_THREAD_SHORTCUT_ID.into(),
        title: "New thread".into(),
        destination: "/new".into(),
    }];
    shortcuts.extend(
        recents
            .iter()
            .take(MAX_RECENT_THREAD_SHORTCUTS)
            .map(|thread| {
                let destination = thread_destination(thread);
                AppShortcut {
                    id: format!("thread:{destination}"),
                    title: if thread.title.trim().is_empty() {
                        "Thread".into()
                    } else {
                        thread.title.trim().into()
                    },
                    destination,
                }
            }),
    );
    shortcuts
}

/// Adds an opened thread to the front of the recents, retaining known titles
/// when a shell update arrives before the thread title.
pub fn record(
    current: &[RecentThreadShortcut],
    opened: RecentThreadShortcut,
) -> Vec<RecentThreadShortcut> {
    let existing = current.iter().find(|thread| {
        thread.project_id == opened.project_id && thread.thread_id == opened.thread_id
    });
    let title = if opened.title.trim().is_empty() {
        existing.map_or(opened.title.clone(), |thread| thread.title.clone())
    } else {
        opened.title.clone()
    };
    let mut result = vec![RecentThreadShortcut { title, ..opened }];
    let project_id = result[0].project_id.clone();
    let thread_id = result[0].thread_id.clone();
    result.extend(
        current
            .iter()
            .filter(|thread| !(thread.project_id == project_id && thread.thread_id == thread_id))
            .cloned(),
    );
    result.truncate(MAX_RECENT_THREAD_SHORTCUTS);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(project_id: &str, thread_id: &str, title: &str) -> RecentThreadShortcut {
        RecentThreadShortcut {
            project_id: project_id.into(),
            thread_id: thread_id.into(),
            title: title.into(),
        }
    }

    #[test]
    fn destinations_encode_each_path_segment() {
        assert_eq!(
            thread_destination(&thread("a/b", "thread 1", "")),
            "/threads/a%2Fb/thread%201"
        );
    }

    #[test]
    fn recent_records_are_bounded_and_keep_a_known_title() {
        let current = vec![thread("p", "t", "Known")];
        let next = record(&current, thread("p", "t", " "));
        assert_eq!(next[0].title, "Known");
        let many = vec![
            thread("p", "1", "1"),
            thread("p", "2", "2"),
            thread("p", "3", "3"),
        ];
        assert_eq!(record(&many, thread("p", "4", "4")).len(), 3);
    }
}
