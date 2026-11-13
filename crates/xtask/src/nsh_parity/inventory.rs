//! What the map pairs nothing for: per state, the texts and labels only one
//! client shows, from the whole of each capture, so a difference outside the
//! map's elements still reaches the report.
use super::{
    compare::{self, NshCapture},
    map::State,
};
use std::{collections::BTreeSet, fs, path::Path};

pub fn render(records: &Path, states: &[&State]) -> String {
    let mut text = String::from("\n## Texts and labels only one client shows\n");
    for state in states {
        let directory = records.join(&state.name);
        let read = |name: &str| fs::read_to_string(directory.join(name)).ok();
        let (Some(outline), Some(capture)) = (read("t3-outline.txt"), read("nsh.json")) else {
            continue;
        };
        let Ok(capture) = serde_json::from_str(&capture) else {
            continue;
        };
        let (t3, nsh) = (t3_texts(&outline), nsh_texts(&capture));
        text.push_str(&format!(
            "\n### {}\n\nOnly in T3: {}\n\nOnly in NSH: {}\n",
            state.name,
            listed(&only(&t3, &nsh)),
            listed(&only(&nsh, &t3)),
        ));
    }
    text
}

fn listed(texts: &[&str]) -> String {
    if texts.is_empty() {
        return "none".into();
    }
    texts
        .iter()
        .map(|text| format!("`{}`", text.replace('`', "'")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One client breaks a line into spans the other draws whole, so a text counts
/// as shown when it is part of any of the other's.
fn only<'a>(texts: &'a BTreeSet<String>, others: &BTreeSet<String>) -> Vec<&'a str> {
    texts
        .iter()
        .filter(|text| !others.iter().any(|other| other.contains(text.as_str())))
        .map(String::as_str)
        .collect()
}

/// The own text and `aria-label` of each visible line of `t3-outline.txt`.
fn t3_texts(outline: &str) -> BTreeSet<String> {
    outline
        .lines()
        .filter(|line| visible(line))
        .flat_map(|line| [own_text(line), aria_label(line)])
        .flatten()
        .filter(|text| worded(text))
        .collect()
}

/// Screen-reader-only spans are a point wide.
fn visible(line: &str) -> bool {
    let frame = line
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(frame, _)| frame.split(',').filter_map(|n| n.parse::<f64>().ok()));
    frame.is_some_and(|numbers| {
        let numbers: Vec<_> = numbers.collect();
        matches!(numbers[..], [_, _, width, height] if width > 1.0 && height > 1.0)
    })
}

fn own_text(line: &str) -> Option<String> {
    let start = line.find(" \"")? + 2;
    let end = line
        .rfind("\" .")
        .or_else(|| line.rfind("\" animations="))?;
    (start < end).then(|| line[start..end].trim().to_owned())
}

fn aria_label(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("aria-label=")?;
    let words = rest
        .split(' ')
        .take_while(|word| !(word.starts_with('"') || word.starts_with('.') || word.contains('=')));
    Some(words.collect::<Vec<_>>().join(" "))
}

/// Visible labels, values and placeholders; identifiers name implementation
/// nodes and are not text the user sees.
fn nsh_texts(capture: &NshCapture) -> BTreeSet<String> {
    capture
        .views
        .iter()
        .enumerate()
        .filter(|(index, view)| {
            compare::shown(&capture.views, *index)
                && view.frame[0] < capture.width
                && view.frame[1] < capture.height
                && view.frame[0] + view.frame[2] > 0.0
                && view.frame[1] + view.frame[3] > 0.0
        })
        .flat_map(|(_, view)| {
            view.texts
                .iter()
                .filter(|(key, _)| key.as_str() != "identifier")
                .map(|(_, value)| value.trim().to_owned())
        })
        .filter(|text| worded(text))
        .collect()
}

fn worded(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTLINE: &str = "button [10,20,30,28] aria-label=Toggle right panel data-slot=toggle .flex\n\
         span [336,264,129,16]  \"Waiting for approval\" .inline-flex\n\
         span [7,101,1,1]  \"hidden from sight\" .sr-only\n\
         span [336,213,108,23]  \"Running gradlew\" animations=live-tool-shine .block\n\
         span [1,2,3,4]  \"·\" .text-muted";

    #[test]
    fn t3_lines_give_their_visible_texts_and_labels() {
        let expected: BTreeSet<String> = [
            "Toggle right panel",
            "Waiting for approval",
            "Running gradlew",
        ]
        .map(String::from)
        .into();
        assert_eq!(t3_texts(OUTLINE), expected);
    }

    #[test]
    fn nsh_inventory_omits_identifiers_and_hidden_or_offscreen_text() {
        let node = |frame, alpha, texts| serde_json::json!({"parent":null,"class":"View","frame":frame,"hidden":false,"alpha":alpha,"role":null,"texts":texts,"layer":null});
        let capture = serde_json::from_value(serde_json::json!({"width":100,"height":100,"views":[
            node([0,0,100,100],1,serde_json::json!({"label":"Bex","identifier":"implementation.detail"})),
            node([10,10,80,24],1,serde_json::json!({"placeholder":"Ask AI","value":""})),
            node([10,50,80,24],0,serde_json::json!({"label":"Hidden action"})),
            node([10,150,80,24],1,serde_json::json!({"label":"Outside window"})),
        ]})).unwrap();
        let expected: BTreeSet<String> = ["Bex", "Ask AI"].map(String::from).into();
        assert_eq!(nsh_texts(&capture), expected);
    }

    #[test]
    fn a_span_of_the_others_whole_line_is_shown_and_the_rest_is_not() {
        let t3: BTreeSet<String> = ["確認", "Stash"].map(String::from).into();
        let nsh: BTreeSet<String> = ["検索の確認です"].map(String::from).into();
        assert_eq!(only(&t3, &nsh), ["Stash"]);
        assert_eq!(only(&nsh, &t3), ["検索の確認です"]);
    }
}
