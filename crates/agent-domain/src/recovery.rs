use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelledBackgroundWork {
    pub id: String,
    pub kind: String,
    pub label: String,
}
pub fn compact_restart_label(text: &str) -> String {
    let text = text
        .split(|c: char| {
            matches!(
                c,
                '\t' | '\n'
                    | '\u{000b}'
                    | '\u{000c}'
                    | '\r'
                    | ' '
                    | '\u{00a0}'
                    | '\u{1680}'
                    | '\u{2000}'
                    ..='\u{200a}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202f}'
                        | '\u{205f}'
                        | '\u{3000}'
                        | '\u{feff}'
            )
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.encode_utf16().count() <= 160 {
        text
    } else {
        String::from_utf16_lossy(&text.encode_utf16().take(159).collect::<Vec<_>>()) + "…"
    }
}
pub fn restart_background_note(work: &[CancelledBackgroundWork]) -> String {
    let mut lines = vec!["Note: the T3 server restarted, and this background work was cancelled before it finished. It will not report back:".into()];
    lines.extend(
        work.iter()
            .take(10)
            .map(|entry| format!("- {}: {}", entry.kind, entry.label)),
    );
    if work.len() > 10 {
        lines.push(format!("- and {} more", work.len() - 10));
    }
    lines.join("\n")
}
pub fn run_ran_after(run: &Run, other: &Run) -> bool {
    match (&run.completed_at, &other.completed_at) {
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(left), Some(right)) if left != right => left > right,
        _ => run.ordinal > other.ordinal,
    }
}
pub fn original_run<'a>(run: &'a Run, runs: &'a [Run]) -> &'a Run {
    let mut current = run;
    let mut seen = BTreeSet::new();
    while let Some(source) = &current.restart_of {
        if !seen.insert(source) {
            break;
        }
        let Some(previous) = runs.iter().find(|run| &run.id == source) else {
            break;
        };
        current = previous;
    }
    current
}
pub fn restart_continuation_work(
    source: &Run,
    runs: &[Run],
    attempts: &[Attempt],
) -> Vec<CancelledBackgroundWork> {
    let mut work = source.restart_cancelled_work.clone();
    let mut current = source;
    let mut seen = BTreeSet::new();
    while let Some(previous) = &current.restart_of {
        if attempts
            .iter()
            .any(|attempt| attempt.run == current.id && attempt.status == AttemptStatus::Completed)
            || !seen.insert(previous)
        {
            break;
        }
        let Some(source) = runs.iter().find(|run| &run.id == previous) else {
            break;
        };
        let mut merged = source.restart_cancelled_work.clone();
        for entry in work {
            if !merged.iter().any(|existing| existing.id == entry.id) {
                merged.push(entry);
            }
        }
        work = merged;
        current = source;
    }
    work
}
pub fn pending_restart_work(
    run: &Run,
    runs: &[Run],
    attempts: &[Attempt],
    messages: &[Message],
) -> Vec<CancelledBackgroundWork> {
    let compact = |run: &Run| {
        messages.iter().any(|message| {
            message.id == run.message
                && message.attachments.is_empty()
                && message.text.trim().eq_ignore_ascii_case("/compact")
        })
    };
    if run.restart_of.is_some() || compact(run) {
        return vec![];
    }
    if attempts.iter().any(|attempt| {
        attempt.run == run.id
            && Some(&attempt.id) != run.attempt.as_ref()
            && attempt.status == AttemptStatus::Completed
    }) {
        return vec![];
    }
    let prompted = runs
        .iter()
        .filter(|candidate| {
            candidate.id != run.id
                && candidate.selection.instance == run.selection.instance
                && candidate.status != RunStatus::RolledBack
                && !compact(candidate)
                && attempts.iter().any(|attempt| {
                    attempt.run == candidate.id && attempt.status == AttemptStatus::Completed
                })
                && candidate.restart_of.as_ref().is_none_or(|source| {
                    runs.iter()
                        .find(|run| &run.id == source)
                        .is_some_and(|source| {
                            !restart_continuation_work(source, runs, attempts).is_empty()
                        })
                })
        })
        .collect::<Vec<_>>();
    let mut work = vec![];
    for source in runs.iter().filter(|source| {
        source.selection.instance == run.selection.instance
            && run_ran_after(run, source)
            && !prompted.iter().any(|later| run_ran_after(later, source))
    }) {
        for entry in &source.restart_cancelled_work {
            if !work
                .iter()
                .any(|existing: &CancelledBackgroundWork| existing.id == entry.id)
            {
                work.push(entry.clone());
            }
        }
    }
    work
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_note_and_roster_bounds_are_preserved() {
        let work = (0..25)
            .map(|index| CancelledBackgroundWork {
                id: index.to_string(),
                kind: "shell".into(),
                label: format!("sleep {index}"),
            })
            .collect::<Vec<_>>();
        let note = restart_background_note(&work);
        assert_eq!(note.lines().count(), 12);
        assert!(note.ends_with("- and 15 more"));
        assert_eq!(
            compact_restart_label(&"x".repeat(10_000))
                .encode_utf16()
                .count(),
            160
        );
        assert_eq!(
            compact_restart_label(&format!("{} (id {})", "d".repeat(400), "t".repeat(400)))
                .encode_utf16()
                .count(),
            160
        );
        assert_eq!(compact_restart_label("\u{feff}a\n  b\u{feff}"), "a b");
    }
}
