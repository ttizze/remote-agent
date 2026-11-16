//! `apps/nsh/parity/map.toml`: the states both clients are brought to, the
//! elements compared in each, how each side finds an element, and the
//! tolerances. The NSH side runs as an acceptance scenario
//! ([`nsh_scenario`]); T3's as the plan `scripts/nsh-parity-t3.ts` takes
//! ([`t3_plan`]).
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub viewport: Viewport,
    pub tolerance: Tolerance,
    #[serde(rename = "state")]
    pub states: Vec<State>,
    #[serde(rename = "element")]
    pub elements: Vec<Element>,
    #[serde(rename = "approved", default)]
    pub approvals: Vec<Approval>,
}

/// Recorded design decisions, retained as context in the report. Every raw
/// difference still counts against parity. It names one
/// `element` or a `group` of elements (every element whose `group` it is).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub element: Option<String>,
    pub group: Option<String>,
    /// A cause instead of an element: positions, in any state, that match
    /// once measured from this element's top on each side (they move with
    /// it, as the timeline's end moves with the composer over it).
    #[serde(default)]
    pub follows: Option<String>,
    /// The metrics approved, as the report names them (`position`, `text
    /// position (drawn)`, …); none: every metric.
    #[serde(default)]
    pub metrics: Vec<String>,
    pub reason: String,
    /// The day it was approved, `YYYY-MM-DD`.
    pub approved: String,
    pub by: String,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

/// How far a metric may differ before it is reported, in points (colours: the
/// Euclidean distance of 0–255 sRGB components).
#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tolerance {
    pub position: f64,
    pub size: f64,
    pub text: f64,
    pub color: f64,
    pub radius: f64,
}

#[derive(Debug, Deserialize, Clone, Copy, Default)]
#[serde(deny_unknown_fields)]
pub struct ToleranceOverride {
    pub position: Option<f64>,
    pub size: Option<f64>,
    pub text: Option<f64>,
    pub color: Option<f64>,
    pub radius: Option<f64>,
}

impl Tolerance {
    pub fn with(self, other: ToleranceOverride) -> Self {
        Self {
            position: other.position.unwrap_or(self.position),
            size: other.size.unwrap_or(self.size),
            text: other.text.unwrap_or(self.text),
            color: other.color.unwrap_or(self.color),
            radius: other.radius.unwrap_or(self.radius),
        }
    }
}

/// What the other client is compared with.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Reference {
    /// T3 Code rendered by `scripts/nsh-parity-t3.ts`.
    #[default]
    T3,
    /// The removed GPUI client's layout, from the values its source set
    /// (`[element.gpui]`): the sidebar keeps that client's design.
    Gpui,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub name: String,
    /// The screen the report groups the state under.
    pub screen: String,
    pub description: String,
    #[serde(default)]
    pub reference: Reference,
    /// Acceptance steps after the common start; `send <text>` sends a message
    /// (in a new chat of the fixture's project the first time).
    pub nsh: Vec<String>,
    /// Steps `scripts/nsh-parity-t3.ts` takes after adding the project.
    #[serde(default)]
    pub t3: Vec<Value>,
    pub elements: Vec<String>,
    /// Elements expected to be hidden in this state. Every other named element
    /// must be measured on both sides; missing selectors never count as a match.
    #[serde(default)]
    pub absent: Vec<String>,
    /// Horizontal positions are measured from this element's left edge (the
    /// conversation column, whose place in the window differs with the
    /// sidebar's width), unless an element names its own anchor.
    #[serde(default)]
    pub column: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Element {
    pub id: String,
    /// What the element is, for the report.
    pub what: String,
    pub kind: Kind,
    /// A CSS selector in T3's page.
    #[serde(default)]
    pub t3: Option<String>,
    pub nsh: NshMatch,
    /// GPUI's layout for a state whose reference is GPUI.
    #[serde(default)]
    pub gpui: Option<GpuiLayout>,
    /// Positions are measured from this element's frame rather than the
    /// state's column and the window's top.
    #[serde(default)]
    pub anchor: Option<String>,
    /// It sits at the end of the column (a user's message): its horizontal
    /// position is how far its right edge is from the column's, which a
    /// column of another width keeps.
    #[serde(default)]
    pub from_end: bool,
    /// Where an [`Kind::Icon`] is centred: its drawn glyph's centre is
    /// compared with this element's centre.
    #[serde(default)]
    pub centred_in: Option<String>,
    /// The part of the screen it belongs to, which an approval can name for
    /// all its elements at once.
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub tolerance: ToleranceOverride,
    /// Which of the selector's matches counts, from 0.
    #[serde(default)]
    pub t3_nth: usize,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Frame, fill and corner radius.
    Box,
    /// A box with text: also the first line's drawn height and colour.
    Text,
    /// A box with a glyph: also the glyph's drawn size, colour and centre.
    Icon,
}

/// How the NSH side finds an element in a capture's views; `{}` when NSH
/// draws no counterpart, so the element is reported missing.
#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NshMatch {
    /// A label, title, value, identifier or placeholder that is this text.
    pub text: Option<String>,
    /// One that contains this text.
    pub contains: Option<String>,
    pub role: Option<String>,
    pub class: Option<String>,
    /// Which of the matching views counts, from 0, in the order views nest.
    #[serde(default)]
    pub nth: usize,
    /// Then the ancestor this many levels up, or `"background"`: the nearest
    /// ancestor (or itself) whose layer fills a background.
    #[serde(default)]
    pub up: Option<Up>,
    /// Then down into its children, the shown child at each index in turn
    /// (an unlabelled glyph inside a labelled row).
    #[serde(default)]
    pub down: Vec<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum Up {
    Levels(usize),
    Named(String),
}

/// What GPUI's source fixes of an element's frame (in the window) and of its
/// glyph's centre (from its `centred_in` container's centre), with that source.
/// What a layout leaves to its content (a flexible width, a position below
/// rows NSH lacks) is left out and not compared.
#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct GpuiLayout {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub glyph_dx: Option<f64>,
    pub glyph_dy: Option<f64>,
    pub source: String,
}

impl Map {
    pub fn parse(text: &str) -> Result<Self, String> {
        let map: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        map.check()?;
        Ok(map)
    }

    pub fn element(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|element| element.id == id)
    }

    /// The approval that covers `metric` of `element`, if one does: one that
    /// names it, or the cause its difference `follows`.
    pub fn approval(
        &self,
        element: &str,
        metric: &str,
        follows: Option<&str>,
    ) -> Option<&Approval> {
        let group = self
            .element(element)
            .and_then(|element| element.group.as_deref());
        self.approvals.iter().find(|approval| {
            if approval.follows.is_some() {
                return approval.follows.as_deref() == follows;
            }
            let names = approval.element.as_deref() == Some(element)
                || (approval.group.is_some() && approval.group.as_deref() == group);
            names
                && (approval.metrics.is_empty()
                    || approval.metrics.iter().any(|approved| approved == metric))
        })
    }

    fn check(&self) -> Result<(), String> {
        let mut seen = BTreeMap::new();
        for element in &self.elements {
            if seen.insert(element.id.as_str(), ()).is_some() {
                return Err(format!("element {} is defined twice", element.id));
            }
            if let Some(Up::Named(name)) = &element.nsh.up
                && name != "background"
            {
                return Err(format!(
                    "element {}: up = {name:?} is not \"background\" or a number",
                    element.id
                ));
            }
        }
        for approval in &self.approvals {
            let date = approval.approved.as_bytes();
            let dated = date.len() == 10
                && date.iter().enumerate().all(|(index, byte)| {
                    if index == 4 || index == 7 {
                        *byte == b'-'
                    } else {
                        byte.is_ascii_digit()
                    }
                });
            let names = match (&approval.element, &approval.group, &approval.follows) {
                (Some(element), None, None) | (None, None, Some(element)) => {
                    self.element(element).is_some()
                }
                (None, Some(group), None) => self
                    .elements
                    .iter()
                    .any(|element| element.group.as_ref() == Some(group)),
                _ => false,
            };
            if !names || !dated {
                return Err(format!(
                    "the approval of {} must name a defined element or group and a YYYY-MM-DD date",
                    approval
                        .element
                        .as_ref()
                        .or(approval.group.as_ref())
                        .or(approval.follows.as_ref())
                        .map_or("nothing", String::as_str)
                ));
            }
        }
        for state in &self.states {
            if let Some(id) = state.absent.iter().find(|id| !state.elements.contains(id)) {
                return Err(format!(
                    "state {} expects {id} hidden but does not compare it",
                    state.name
                ));
            }
            if let Some(column) = &state.column
                && !state.elements.contains(column)
            {
                return Err(format!(
                    "state {} measures from {column}, which it does not compare",
                    state.name
                ));
            }
            for id in &state.elements {
                let element = self
                    .element(id)
                    .ok_or_else(|| format!("state {} names no element {id}", state.name))?;
                let reference = match state.reference {
                    Reference::T3 => element.t3.is_some(),
                    Reference::Gpui => element.gpui.is_some(),
                };
                if !reference {
                    return Err(format!(
                        "element {id} has no {:?} side for state {}",
                        state.reference, state.name
                    ));
                }
                for other in [&element.anchor, &element.centred_in].into_iter().flatten() {
                    if !state.elements.contains(other) {
                        return Err(format!(
                            "element {id} is measured from {other}, which state {} does not compare",
                            state.name
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Switches the app to English (T3 has only English copy), sizes the window
/// and waits for the fixture's project; every parity scenario starts so.
const START: &str = "expect \"何に取り組みますか？\" 30000\npress 設定を開く\npress \"日本語 ▾\"\npress English\nexpect \"Display language\" 10000\npress Close\n";

/// The acceptance scenario that brings the app to `state` and captures it as
/// `nsh.png` / `nsh.json`.
pub fn nsh_scenario(state: &State, viewport: Viewport) -> String {
    let mut script = format!(
        "# Generated by `cargo xtask nsh-parity` from apps/nsh/parity/map.toml: {}\n{START}window {} {}\nexpect \"検証プロジェクト\" 30000\n",
        state.name, viewport.width, viewport.height
    );
    script.push_str(&format!(
        "pointer {} {}\n",
        viewport.width - 2,
        viewport.height - 2
    ));
    let mut chat_open = false;
    for step in &state.nsh {
        match step.strip_prefix("send ") {
            Some(text) => {
                if !chat_open {
                    script.push_str("press \"New chat in this project\"\n");
                    chat_open = true;
                }
                script.push_str(&format!("type \"Ask AI\" {}\npress Send\n", quote(text)));
            }
            None => {
                script.push_str(step);
                script.push('\n');
            }
        }
    }
    // Transitions settle; endless animations keep running.
    script.push_str("wait 600\ncapture nsh\ndump nsh-tree\n");
    script
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The plan `scripts/nsh-parity-t3.ts` reads for the states whose reference is T3.
pub fn t3_plan<'a>(
    map: &Map,
    states: impl IntoIterator<Item = &'a State>,
    scale: f64,
    paths: [&str; 3],
) -> Value {
    let [t3_app, t3_root, codex_fixture] = paths;
    let states: Vec<Value> = states
        .into_iter()
        .filter(|state| state.reference == Reference::T3)
        .map(|state| {
            let elements: BTreeMap<&str, &str> = state
                .elements
                .iter()
                .filter_map(|id| map.element(id))
                .filter_map(|element| Some((element.id.as_str(), element.t3.as_deref()?)))
                .collect();
            json!({"name": state.name, "steps": state.t3, "elements": elements})
        })
        .collect();
    json!({
        "t3App": t3_app,
        "t3Root": t3_root,
        "codexFixture": codex_fixture,
        "viewport": {"width": map.viewport.width, "height": map.viewport.height, "scale": scale},
        "states": states,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r##"
[viewport]
width = 1280
height = 800

[tolerance]
position = 4
size = 4
text = 1
color = 24
radius = 2

[[state]]
name = "turn"
screen = "Conversation"
description = "A turn"
nsh = ["send [immediate] say \"hi\"", "expect done 20000", "send again"]
t3 = [{ send = "[immediate] say \"hi\"" }, { waitText = "done" }]
elements = ["composer", "answer"]
column = "composer"

[[state]]
name = "sidebar"
screen = "Sidebar"
description = "The sidebar"
reference = "gpui"
nsh = []
elements = ["search"]

[[element]]
id = "composer"
what = "Composer"
kind = "box"
t3 = "[data-chat-composer-surface]"
nsh = { text = "Ask AI", role = "AXGroup", up = "background" }

[[element]]
id = "answer"
what = "Answer"
kind = "text"
t3 = "[data-message-role=assistant]"
nsh = { text = "message.assistant-text" }
anchor = "composer"
tolerance = { text = 2 }

[[element]]
id = "search"
what = "Search"
kind = "icon"
nsh = { text = "Search", up = 1 }
gpui = { x = 20, height = 14, glyph_dy = 0, source = "sidebar.rs:744" }
"##;

    #[test]
    fn the_map_reads_states_elements_and_overrides() {
        let map = Map::parse(MAP).unwrap();
        assert_eq!(map.states.len(), 2);
        assert_eq!(map.states[1].reference, Reference::Gpui);
        let answer = map.element("answer").unwrap();
        assert_eq!(answer.kind, Kind::Text);
        assert_eq!(map.tolerance.with(answer.tolerance).text, 2.0);
        assert_eq!(map.tolerance.with(answer.tolerance).size, 4.0);
        assert_eq!(
            map.element("composer").unwrap().nsh.up,
            Some(Up::Named("background".into()))
        );
        assert_eq!(map.element("search").unwrap().nsh.up, Some(Up::Levels(1)));
    }

    #[test]
    fn a_map_naming_what_it_does_not_define_is_rejected() {
        let unknown = MAP.replace(
            "elements = [\"search\"]",
            "elements = [\"search\", \"nope\"]",
        );
        assert_eq!(
            Map::parse(&unknown).unwrap_err(),
            "state sidebar names no element nope"
        );
        let no_t3 = MAP.replace(
            "elements = [\"composer\", \"answer\"]",
            "elements = [\"composer\", \"search\"]",
        );
        assert_eq!(
            Map::parse(&no_t3).unwrap_err(),
            "element search has no T3 side for state turn"
        );
        let anchorless = MAP.replace(
            "elements = [\"composer\", \"answer\"]\ncolumn = \"composer\"",
            "elements = [\"answer\"]",
        );
        assert_eq!(
            Map::parse(&anchorless).unwrap_err(),
            "element answer is measured from composer, which state turn does not compare"
        );
        let columnless = MAP.replace(
            "elements = [\"composer\", \"answer\"]",
            "elements = [\"answer\"]",
        );
        assert_eq!(
            Map::parse(&columnless).unwrap_err(),
            "state turn measures from composer, which it does not compare"
        );
        let undated = format!(
            "{MAP}\n[[approved]]\nelement = \"answer\"\nmetrics = [\"position\"]\nreason = \"r\"\napproved = \"8 Nov\"\nby = \"user\"\n"
        );
        assert_eq!(
            Map::parse(&undated).unwrap_err(),
            "the approval of answer must name a defined element or group and a YYYY-MM-DD date"
        );
        let unknown_up = MAP.replace("up = \"background\"", "up = \"parent\"");
        assert_eq!(
            Map::parse(&unknown_up).unwrap_err(),
            "element composer: up = \"parent\" is not \"background\" or a number"
        );
    }

    #[test]
    fn the_scenario_opens_one_chat_and_captures_at_the_end() {
        let map = Map::parse(MAP).unwrap();
        let script = nsh_scenario(&map.states[0], map.viewport);
        assert!(script.starts_with("# Generated by `cargo xtask nsh-parity`"));
        assert!(script.contains("press English\n"));
        assert!(script.contains("window 1280 800\n"));
        let body = &script[script.find("検証プロジェクト\" 30000\n").unwrap()..];
        assert_eq!(
            body.lines().skip(1).collect::<Vec<_>>(),
            [
                "pointer 1278 798",
                "press \"New chat in this project\"",
                "type \"Ask AI\" \"[immediate] say \\\"hi\\\"\"",
                "press Send",
                "expect done 20000",
                "type \"Ask AI\" \"again\"",
                "press Send",
                "wait 600",
                "capture nsh",
                "dump nsh-tree",
            ]
        );
    }

    #[test]
    fn the_t3_plan_carries_only_t3_states_and_their_selectors() {
        let map = Map::parse(MAP).unwrap();
        let plan = t3_plan(&map, &map.states, 2.0, ["/app", "/t3", "/fixture"]);
        assert_eq!(
            plan["viewport"],
            json!({"width": 1280, "height": 800, "scale": 2.0})
        );
        assert_eq!(plan["states"].as_array().unwrap().len(), 1);
        assert_eq!(
            plan["states"][0]["elements"],
            json!({"answer": "[data-message-role=assistant]", "composer": "[data-chat-composer-surface]"})
        );
        assert_eq!(plan["states"][0]["steps"][1], json!({"waitText": "done"}));
    }
}
