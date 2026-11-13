//! `report.md`: a summary by screen, then each state's side-by-side image, its
//! differences and the approved deviations among them, and every state that
//! could not be compared, with why.
use super::{
    compare::Difference,
    map::{Approval, Map, Reference, State},
};

/// What became of one state.
pub struct Outcome<'a> {
    pub state: &'a State,
    pub result: Result<Vec<Difference>, String>,
}

/// All measured differences count. Recorded design exceptions remain visible
/// context, rather than silently turning a comparison into a successful check.
#[derive(Debug, serde::Serialize)]
pub struct Summary<'a> {
    pub passed: bool,
    pub states_compared: usize,
    pub states_not_compared: usize,
    pub states_not_selected: usize,
    pub differences: usize,
    pub recorded_exceptions: usize,
    pub not_measured: usize,
    pub missing_in_candidate: usize,
    pub states: Vec<StateResult<'a>>,
}

#[derive(Debug, serde::Serialize)]
pub struct StateResult<'a> {
    pub state: &'a str,
    pub differences: &'a [Difference],
    pub error: Option<&'a str>,
}

pub fn summary<'a>(map: &Map, outcomes: &'a [Outcome]) -> Summary<'a> {
    let differences: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.result.as_ref().ok())
        .flatten()
        .collect();
    let states_not_compared = outcomes
        .iter()
        .filter(|outcome| outcome.result.is_err())
        .count();
    Summary {
        passed: !outcomes.is_empty() && states_not_compared == 0 && differences.is_empty(),
        states_compared: outcomes.len() - states_not_compared,
        states_not_compared,
        states_not_selected: map.states.len() - outcomes.len(),
        differences: differences.len(),
        recorded_exceptions: differences
            .iter()
            .filter(|difference| approval(map, difference).is_some())
            .count(),
        not_measured: differences
            .iter()
            .filter(|difference| difference.metric.ends_with("not measured"))
            .count(),
        missing_in_candidate: differences
            .iter()
            .filter(|difference| difference.nsh == "missing")
            .count(),
        states: outcomes
            .iter()
            .map(|outcome| StateResult {
                state: &outcome.state.name,
                differences: outcome.result.as_deref().unwrap_or_default(),
                error: outcome.result.as_ref().err().map(String::as_str),
            })
            .collect(),
    }
}

pub fn render(run: &str, map: &Map, outcomes: &[Outcome]) -> String {
    let totals = summary(map, outcomes);
    let verdict = if totals.passed { "MATCH" } else { "FAIL" };
    let mut report = format!(
        "# NSH parity {run}\n\nNSH (right of each table and image) against the installed T3 Code rendered by \
         `scripts/nsh-parity-t3.ts`, or, for the sidebar, the GPUI client's layout from its source. Both clients ran \
         against Bex's Codex fixture, at {}×{} points. Positions are in points from the state's column (or the element's \
         anchor) and the window's top; colours are sRGB. Tolerances: position {}, size {}, drawn text and glyphs {}, \
         colour distance {}, corner radius {}.\n\n**{verdict}: {} differences including measurement gaps, {} states not compared, {} measurements unavailable.** \
         Recorded exceptions are included in these totals and in the check's exit status.\n\n\
         Scope: macOS only; {} of {} mapped states selected. Unselected states, unpaired UI, iPhone and Windows \
         are not verified by this result. [Candidate provenance](manifest.json), [reference provenance](reference-manifest.json), \
         [captured map](captured-map.toml), [comparison map](comparison-map.toml), \
         [machine-readable results](summary.json). Animation metadata checks presence, not speed or trajectory; \
         SwiftUI redraws without published metadata and unavailable corner radii are reported as unmeasured.\n\n## Summary\n\n| Screen | States compared | Not compared | Unrecorded differences | Missing in NSH | Recorded exceptions |\n|---|---|---|---|---|---|\n",
        map.viewport.width,
        map.viewport.height,
        map.tolerance.position,
        map.tolerance.size,
        map.tolerance.text,
        map.tolerance.color,
        map.tolerance.radius,
        totals.differences,
        totals.states_not_compared,
        totals.not_measured,
        outcomes.len(),
        map.states.len()
    );
    for screen in screens(outcomes) {
        let of_screen: Vec<_> = outcomes
            .iter()
            .filter(|outcome| outcome.state.screen == screen)
            .collect();
        let (differences, approved): (Vec<_>, Vec<_>) = of_screen
            .iter()
            .filter_map(|outcome| outcome.result.as_ref().ok())
            .flatten()
            .partition(|difference| approval(map, difference).is_none());
        report.push_str(&format!(
            "| {screen} | {} | {} | {} | {} | {} |\n",
            of_screen
                .iter()
                .filter(|outcome| outcome.result.is_ok())
                .count(),
            of_screen
                .iter()
                .filter(|outcome| outcome.result.is_err())
                .count(),
            differences.len(),
            differences
                .iter()
                .filter(|difference| difference.nsh == "missing")
                .count(),
            approved.len(),
        ));
    }
    for screen in screens(outcomes) {
        report.push_str(&format!("\n## {screen}\n"));
        for outcome in outcomes
            .iter()
            .filter(|outcome| outcome.state.screen == screen)
        {
            report.push_str(&section(map, outcome));
        }
    }
    report
}

fn screens<'a>(outcomes: &'a [Outcome]) -> Vec<&'a str> {
    let mut screens: Vec<&str> = Vec::new();
    for outcome in outcomes {
        if !screens.contains(&outcome.state.screen.as_str()) {
            screens.push(&outcome.state.screen);
        }
    }
    screens
}

fn section(map: &Map, outcome: &Outcome) -> String {
    let state = outcome.state;
    let name = &state.name;
    let reference = match state.reference {
        Reference::T3 => "T3",
        Reference::Gpui => "GPUI",
    };
    let mut text = format!("\n### {name}\n\n{}\n\n", state.description);
    let (differences, approved): (Vec<_>, Vec<_>) = match &outcome.result {
        Err(reason) => {
            text.push_str(&format!("Not compared: {}\n", cell(reason)));
            return text;
        }
        Ok(differences) => differences
            .iter()
            .partition(|difference| approval(map, difference).is_none()),
    };
    text.push_str(&format!(
        "![{name}: {reference} left, NSH right; differing elements outlined]({name}/composite.png)\n\n"
    ));
    if differences.is_empty() {
        text.push_str(if approved.is_empty() {
            "No differences beyond tolerance.\n"
        } else {
            "Differences remain in the recorded exceptions below.\n"
        });
    } else {
        text.push_str(&format!(
            "| Element | Metric | {reference} | NSH |\n|---|---|---|---|\n"
        ));
        for difference in differences {
            text.push_str(&format!("{}|\n", row(map, difference)));
        }
    }
    if !approved.is_empty() {
        text.push_str(&format!(
            "\nRecorded exceptions (still counted as differences):\n\n| Element | Metric | {reference} | NSH | Reason | Recorded |\n|---|---|---|---|---|---|\n"
        ));
        for (difference, approval) in approved
            .into_iter()
            .filter_map(|difference| Some((difference, approval(map, difference)?)))
        {
            text.push_str(&format!(
                "{}| {} | {} by {} |\n",
                row(map, difference),
                cell(&approval.reason),
                approval.approved,
                approval.by
            ));
        }
    }
    text
}

fn approval<'a>(map: &'a Map, difference: &Difference) -> Option<&'a Approval> {
    map.approval(
        &difference.element,
        difference.metric,
        difference.follows.as_deref(),
    )
}

/// A difference's cells up to its NSH value, with the trailing separator.
fn row(map: &Map, difference: &Difference) -> String {
    let what = map
        .element(&difference.element)
        .map_or(difference.element.as_str(), |element| element.what.as_str());
    format!(
        "| {} (`{}`) | {} | {} | {} ",
        cell(what),
        difference.element,
        difference.metric,
        cell(&difference.reference),
        cell(&difference.nsh)
    )
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
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
description = "A finished turn."
nsh = []
elements = ["row"]

[[state]]
name = "approval"
screen = "Conversation"
description = "An approval."
nsh = []
elements = ["row"]

[[state]]
name = "search"
screen = "Sidebar"
description = "The search field."
reference = "gpui"
nsh = []
elements = ["search"]

[[element]]
id = "row"
what = "Work row | folded"
kind = "text"
t3 = ".row"
nsh = { text = "Worked" }

[[element]]
id = "search"
what = "Search icon"
kind = "icon"
nsh = { text = "Search" }
gpui = { height = 1, source = "sidebar.rs" }
"##;

    #[test]
    fn only_fully_measured_equal_states_pass_and_exceptions_do_not_hide_differences() {
        let map = Map::parse(&format!("{MAP}\n[[approved]]\nelement = \"row\"\nreason = \"kept layout\"\napproved = \"2026-11-09\"\nby = \"the user\"\n")).unwrap();
        let mut outcomes = [Outcome {
            state: &map.states[0],
            result: Ok(vec![]),
        }];
        assert!(summary(&map, &outcomes).passed);
        outcomes[0].result = Err("reference did not render".into());
        assert!(!summary(&map, &outcomes).passed);
        assert_eq!(summary(&map, &outcomes).states_not_compared, 1);
        assert_eq!(summary(&map, &outcomes).states_compared, 0);
        assert_eq!(
            summary(&map, &outcomes).states[0].error,
            Some("reference did not render")
        );
        outcomes[0].result = Ok(vec![Difference {
            element: "row".into(),
            metric: "size",
            reference: "20".into(),
            nsh: "24".into(),
            follows: None,
        }]);
        let result = summary(&map, &outcomes);
        assert!(!result.passed);
        assert_eq!(result.differences, 1);
        assert_eq!(result.recorded_exceptions, 1);
        assert_eq!(result.states_compared, 1);
        assert_eq!(result.states_not_selected, 2);
        assert_eq!(result.missing_in_candidate, 0);
        assert_eq!(result.not_measured, 0);
        assert!(render("run", &map, &outcomes).contains("**FAIL:"));
        outcomes[0].result = Ok(vec![
            Difference {
                element: "row".into(),
                metric: "corner radius not measured",
                reference: "16".into(),
                nsh: "unavailable".into(),
                follows: None,
            },
            Difference {
                element: "row".into(),
                metric: "present",
                reference: "shown".into(),
                nsh: "missing".into(),
                follows: None,
            },
        ]);
        let result = summary(&map, &outcomes);
        assert!(!result.passed);
        assert_eq!(result.not_measured, 1);
        assert_eq!(result.missing_in_candidate, 1);
        assert_eq!(result.differences, 2);
        assert!(!summary(&map, &[]).passed);
    }

    #[test]
    fn the_report_summarises_by_screen_and_details_each_state() {
        let map = Map::parse(MAP).unwrap();
        let missing = Difference {
            element: "row".into(),
            metric: "present",
            reference: "shown".into(),
            nsh: "missing".into(),
            follows: None,
        };
        let moved = Difference {
            element: "search".into(),
            metric: "glyph centre from its container's centre",
            reference: "0.0, 0.0".into(),
            nsh: "0.0, 1.5".into(),
            follows: None,
        };
        let outcomes = [
            Outcome {
                state: &map.states[0],
                result: Ok(vec![missing]),
            },
            Outcome {
                state: &map.states[1],
                result: Err("the fixture Host did not\nbecome ready".into()),
            },
            Outcome {
                state: &map.states[2],
                result: Ok(vec![moved]),
            },
        ];
        let text = render("run-1", &map, &outcomes);
        for line in [
            "| Conversation | 1 | 1 | 1 | 1 | 0 |",
            "| Sidebar | 1 | 0 | 1 | 0 | 0 |",
            "![turn: T3 left, NSH right; differing elements outlined](turn/composite.png)",
            "| Work row \\| folded (`row`) | present | shown | missing |",
            "Not compared: the fixture Host did not become ready",
            "| Element | Metric | GPUI | NSH |",
            "| Search icon (`search`) | glyph centre from its container's centre | 0.0, 0.0 | 0.0, 1.5 |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(text.find("## Conversation").unwrap() < text.find("## Sidebar").unwrap());
    }

    #[test]
    fn approved_deviations_are_listed_apart_from_differences() {
        let approved = format!(
            "{MAP}\n[[approved]]\nelement = \"row\"\nmetrics = [\"position\"]\nreason = \"Bex keeps its layout\"\napproved = \"2026-11-08\"\nby = \"the user\"\n\n[[approved]]\ngroup = \"sidebar\"\nreason = \"GPUI's own\"\napproved = \"2026-11-08\"\nby = \"the user\"\n"
        )
        .replace("nsh = { text = \"Search\" }", "nsh = { text = \"Search\" }\ngroup = \"sidebar\"");
        let map = Map::parse(&approved).unwrap();
        let difference = |metric| Difference {
            element: "row".into(),
            metric,
            reference: "0.0, 5.0".into(),
            nsh: "9.0, 5.0".into(),
            follows: None,
        };
        // A group's approval covers every metric of its elements.
        let glyph = Difference {
            element: "search".into(),
            ..difference("glyph size (drawn)")
        };
        let outcomes = [
            Outcome {
                state: &map.states[0],
                result: Ok(vec![difference("position"), difference("size")]),
            },
            Outcome {
                state: &map.states[2],
                result: Ok(vec![glyph]),
            },
        ];
        let text = render("run-1", &map, &outcomes);
        for line in [
            "| Conversation | 1 | 0 | 1 | 0 | 1 |",
            "| Sidebar | 1 | 0 | 0 | 0 | 1 |",
            "| Search icon (`search`) | glyph size (drawn) | 0.0, 5.0 | 9.0, 5.0 | GPUI's own | 2026-11-08 by the user |",
            "| Work row \\| folded (`row`) | size | 0.0, 5.0 | 9.0, 5.0 |",
            "Recorded exceptions (still counted as differences):",
            "| Work row \\| folded (`row`) | position | 0.0, 5.0 | 9.0, 5.0 | Bex keeps its layout | 2026-11-08 by the user |",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(!text.contains("| position | 0.0, 5.0 | 9.0, 5.0 |\n"));
    }

    #[test]
    fn a_cause_approves_only_the_positions_that_follow_it() {
        let map = Map::parse(&format!(
            "{MAP}\n[[approved]]\nfollows = \"search\"\nreason = \"moves with the search field\"\napproved = \"2026-11-09\"\nby = \"the coordinator\"\n"
        ))
        .unwrap();
        let moved = |follows: Option<&str>| Difference {
            element: "row".into(),
            metric: "position",
            reference: "0.0, 5.0".into(),
            nsh: "0.0, 40.0".into(),
            follows: follows.map(str::to_owned),
        };
        let outcomes = [Outcome {
            state: &map.states[0],
            result: Ok(vec![moved(Some("search")), moved(None)]),
        }];
        let text = render("run-1", &map, &outcomes);
        assert!(
            text.contains("| Conversation | 1 | 0 | 1 | 0 | 1 |"),
            "{text}"
        );
        assert!(text.contains("| moves with the search field | 2026-11-09 by the coordinator |"));
    }
}
