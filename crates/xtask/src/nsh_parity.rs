//! `cargo xtask nsh-parity [--appearance] [--compare RUN | --t3 RUN] [STATE-FILTER...]`: brings the Mac NSH
//! app and T3 Code to the same states (`apps/nsh/parity/map.toml`), captures
//! both, pairs their elements and writes `target/nsh-parity/run-<time>/report.md`
//! with a side-by-side image and the differences of each state.
//!
//! - NSH: each state is an acceptance scenario ([`map::nsh_scenario`]) against a
//!   fresh fixture Host, ending in the driver's `capture` (screenshot and every
//!   view's frame, accessibility texts and layer).
//! - T3: `scripts/nsh-parity-t3.ts` runs the installed T3 Code's own server and
//!   web client (`T3_CODE_APP`, the Nightly by default), apart from the user's
//!   data, with Bex's Codex fixture as its Codex, in headless Chromium at the
//!   same size and scale. The checkout `T3_CODE_ROOT` lends it Playwright and
//!   the Codex schema.
//! - The sidebar keeps the GPUI client's design: its states compare with the
//!   layout GPUI's source sets, recorded in the map.
//!
//! `--compare RUN` measures an earlier run's captures again (after editing the
//! map) without running either client; `--t3 RUN` renders T3's side of it again
//! first (after editing T3 steps or selectors).
//! The default compares element presence only. `--appearance` also measures
//! position, size, colours, typography, corner radii and animation metadata.
//! `--catalog` lists the complete source-derived T3 component checklist without
//! building or launching either client.
mod catalog;
mod compare;
mod inventory;
mod manifest;
mod map;
mod pixels;
mod report;

use crate::{
    Result,
    nsh_acceptance::{self, Tools},
    supervision,
};
use compare::{Difference, Measure, Mode, NshCapture, T3Capture};
use map::{Map, Reference, State};
use pixels::{Image, Rgb};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const MAP: &str = "apps/nsh/parity/map.toml";
const T3_SCRIPT: &str = "scripts/nsh-parity-t3.ts";
const T3_APP: &str = "/Applications/T3 Code (Nightly).app";
const DIFFERS: Rgb = [255, 64, 64];
const MATCHES: Rgb = [64, 200, 120];

pub async fn run(arguments: &[String]) -> Result<()> {
    let (mode, arguments) = match arguments {
        [flag, rest @ ..] if flag == "--appearance" => (Mode::Appearance, rest),
        _ => (Mode::Presence, arguments),
    };
    let (flag, existing, filters) = match arguments {
        [flag, run, filters @ ..] if flag == "--compare" || flag == "--t3" => {
            (flag.as_str(), Some(run), filters)
        }
        _ => ("", None, arguments),
    };
    let cwd = std::env::current_dir()?;
    // T3's server runs the Codex relay from its own checkout: paths are absolute.
    let existing = existing.map(|run| cwd.join(run));
    let target = nsh_acceptance::target(&cwd);
    if matches!(arguments, [flag] if flag == "--catalog") {
        return catalog::write(&cwd, &target);
    }
    let map = Map::parse(&fs::read_to_string(cwd.join(MAP))?)
        .map_err(|error| format!("{MAP}: {error}"))?;
    let states = select(&map, filters)?;
    let records = match existing {
        Some(records) => {
            if flag == "--t3" {
                capture_reference(&cwd, &map, &states, &Tools::locate(&target)?, &records).await?;
            }
            records
        }
        None => capture(&cwd, &target, &map, &states).await?,
    };
    fs::copy(cwd.join(MAP), records.join("comparison-map.toml"))?;
    let outcomes: Vec<report::Outcome> = states
        .iter()
        .map(|state| report::Outcome {
            state,
            result: compare_state(&map, state, &records.join(&state.name), mode),
        })
        .collect();
    let name = records
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = records.join("report.md");
    let inventory = if mode == Mode::Appearance {
        inventory::render(&records, &states)
    } else {
        String::new()
    };
    let report = report::render(&name, &map, &outcomes, mode) + &inventory;
    fs::write(&path, report)?;
    let summary = report::summary(&map, &outcomes, mode);
    fs::write(
        records.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!(
        "{} differences in {} states; {} not compared; {} not measured\nreport: {}",
        summary.differences,
        summary.states_compared,
        summary.states_not_compared,
        summary.not_measured,
        path.display()
    );
    if !summary.passed {
        return Err(format!("parity differs; see {}", path.display()).into());
    }
    Ok(())
}

fn select<'a>(map: &'a Map, filters: &[String]) -> Result<Vec<&'a State>> {
    let states: Vec<&State> = map
        .states
        .iter()
        .filter(|state| {
            filters.is_empty()
                || filters
                    .iter()
                    .any(|filter| state.name.contains(filter.as_str()))
        })
        .collect();
    if states.is_empty() {
        let names: Vec<&str> = map.states.iter().map(|state| state.name.as_str()).collect();
        return Err(format!("no state matches; available: {}", names.join(", ")).into());
    }
    Ok(states)
}

/// Runs both clients through `states`; the run's directory.
async fn capture(cwd: &Path, target: &Path, map: &Map, states: &[&State]) -> Result<PathBuf> {
    let tools = Tools::locate(target)?;
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();
    let records = target.join("nsh-parity").join(format!("run-{seconds}"));
    fs::create_dir_all(&records)?;
    manifest::write(cwd, &records, &tools.app, &tools.codex, &cwd.join(MAP))?;
    let cancel = supervision::cancellation();
    for state in states {
        let directory = records.join(&state.name);
        fs::create_dir_all(&directory)?;
        let script = directory.join("nsh.scenario");
        fs::write(&script, map::nsh_scenario(state, map.viewport))?;
        println!("nsh {}: running", state.name);
        let failure = match nsh_acceptance::scenario(&script, &directory, &tools, &cancel).await {
            Ok(report) => report.failure(),
            Err(error) => Some(error.to_string()),
        };
        if let Some(failure) = &failure {
            fs::write(directory.join("nsh-error.txt"), failure)?;
        }
        println!(
            "nsh {}: {}",
            state.name,
            failure.as_deref().unwrap_or("captured")
        );
        if *cancel.borrow() {
            return Err("cancelled".into());
        }
    }
    capture_reference(cwd, map, states, &tools, &records).await?;
    Ok(records)
}

/// One owned status is replaced on every reference attempt. A failed or
/// interrupted rerender must never reuse old screenshots as successful ones.
async fn capture_reference(
    cwd: &Path,
    map: &Map,
    states: &[&State],
    tools: &Tools,
    records: &Path,
) -> Result<()> {
    if !states.iter().any(|state| state.reference == Reference::T3) {
        return Ok(());
    }
    let status = records.join("reference-status.json");
    fs::write(
        &status,
        serde_json::to_vec(&serde_json::json!({"error": "capture incomplete"}))?,
    )?;
    let error = render_t3(cwd, map, states, tools, records)
        .await
        .err()
        .map(|error| error.to_string());
    fs::write(
        status,
        serde_json::to_vec(&serde_json::json!({"error": error}))?,
    )?;
    Ok(())
}

async fn render_t3(
    cwd: &Path,
    map: &Map,
    states: &[&State],
    tools: &Tools,
    records: &Path,
) -> Result<()> {
    let root = std::env::var("T3_CODE_ROOT")
        .map_err(|_| "T3_CODE_ROOT must name a T3 Code checkout with its dependencies installed")?;
    // The NSH window's pixels per point, so both screenshots have the same.
    let scale = states
        .iter()
        .find_map(|state| fs::read(records.join(&state.name).join("nsh.png")).ok())
        .and_then(|bytes| Image::decode(&bytes, map.viewport.width.into()).ok())
        .map_or(2.0, |image| image.scale);
    let app = std::env::var("T3_CODE_APP").unwrap_or_else(|_| T3_APP.into());
    manifest::reference(records, &app, Path::new(&root), &tools.codex)?;
    let plan = map::t3_plan(
        map,
        states.iter().copied(),
        scale,
        [&app, &root, &path(&tools.codex)],
    );
    let plan_path = records.join("t3-plan.json");
    fs::write(&plan_path, serde_json::to_vec_pretty(&plan)?)?;
    let status = tokio::process::Command::new("node")
        .arg(cwd.join(T3_SCRIPT))
        .arg(&plan_path)
        .arg(records)
        .status()
        .await?;
    if !status.success() {
        return Err(format!("{T3_SCRIPT} exited with {status}").into());
    }
    Ok(())
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The state's differences, writing its side-by-side image; why it could not
/// be compared otherwise.
fn compare_state(
    map: &Map,
    state: &State,
    directory: &Path,
    mode: Mode,
) -> std::result::Result<Vec<Difference>, String> {
    let read =
        |name: &str| fs::read(directory.join(name)).map_err(|error| format!("{name}: {error}"));
    if let Ok(reason) = fs::read_to_string(directory.join("nsh-error.txt")) {
        return Err(format!(
            "the NSH scenario failed: {reason} (see {}/nsh.scenario and app.log)",
            state.name
        ));
    }
    let width = f64::from(map.viewport.width);
    let nsh_image = Image::decode(&read("nsh.png")?, width)?;
    let nsh_capture: NshCapture =
        serde_json::from_slice(&read("nsh.json")?).map_err(|error| error.to_string())?;
    if (nsh_capture.width - width).abs() > 0.5
        || (nsh_capture.height - f64::from(map.viewport.height)).abs() > 0.5
    {
        return Err(format!(
            "the NSH window was {}×{} points, expected {width}×{}",
            nsh_capture.width, nsh_capture.height, map.viewport.height
        ));
    }
    let nsh = compare::nsh_measures(map, state, &nsh_capture, &nsh_image);
    let (reference, reference_image) = match state.reference {
        Reference::T3 => {
            reference_succeeded(
                directory.parent().ok_or("state has no run directory")?,
                &state.name,
            )?;
            let image = Image::decode(&read("t3.png")?, width)?;
            let capture: T3Capture =
                serde_json::from_slice(&read("t3.json")?).map_err(|error| error.to_string())?;
            (
                compare::reference_measures(map, state, Some((&capture, &image))),
                image,
            )
        }
        Reference::Gpui => (
            compare::reference_measures(map, state, None),
            nsh_image.clone(),
        ),
    };
    let differences = compare::differences(map, state, &reference, &nsh, mode);
    let composite = composite(
        [reference_image, nsh_image],
        [&reference, &nsh],
        &differences,
    );
    fs::write(directory.join("composite.png"), composite.encode())
        .map_err(|error| error.to_string())?;
    Ok(differences)
}

fn reference_succeeded(records: &Path, state: &str) -> std::result::Result<(), String> {
    let read =
        |file: &str| fs::read(records.join(file)).map_err(|error| format!("{file}: {error}"));
    let status: BTreeMap<String, Option<String>> =
        serde_json::from_slice(&read("reference-status.json")?)
            .map_err(|error| format!("reference status: {error}"))?;
    match status.get("error") {
        Some(None) => {}
        Some(Some(error)) => return Err(format!("reference capture failed: {error}")),
        None => return Err("reference capture has no status".into()),
    }
    let summary: BTreeMap<String, Option<String>> =
        serde_json::from_slice(&read("t3-summary.json")?)
            .map_err(|error| format!("reference summary: {error}"))?;
    match summary.get(state) {
        Some(None) => Ok(()),
        Some(Some(error)) => Err(format!("T3 did not reach the state: {error}")),
        None => Err(format!("T3 did not capture {state}")),
    }
}

/// Both screenshots side by side, each compared element outlined: red where it
/// differs, green where it matches.
fn composite(
    images: [Image; 2],
    measures: [&BTreeMap<String, Measure>; 2],
    differences: &[Difference],
) -> Image {
    let [mut left, mut right] = images;
    for (image, measures) in [(&mut left, measures[0]), (&mut right, measures[1])] {
        for (id, measure) in measures {
            let differs = differences
                .iter()
                .any(|difference| &difference.element == id);
            image.outline(measure.frame, if differs { DIFFERS } else { MATCHES });
        }
    }
    pixels::side_by_side(&left, &right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicitly_successful_reference_attempt_and_state_are_usable() {
        let run = tempfile::tempdir().unwrap();
        let status = run.path().join("reference-status.json");
        let summary = run.path().join("t3-summary.json");
        assert!(reference_succeeded(run.path(), "turn").is_err());
        fs::write(&status, r#"{"error":null}"#).unwrap();
        for invalid in [
            "invalid",
            "{}",
            r#"{"other":null}"#,
            r#"{"turn":"timeout"}"#,
        ] {
            fs::write(&summary, invalid).unwrap();
            assert!(reference_succeeded(run.path(), "turn").is_err());
        }
        fs::write(&summary, r#"{"turn":null}"#).unwrap();
        assert!(reference_succeeded(run.path(), "turn").is_ok());
        for invalid in [
            "invalid",
            "{}",
            r#"{"error":"capture incomplete"}"#,
            r#"{"error":"failed"}"#,
        ] {
            fs::write(&status, invalid).unwrap();
            assert!(reference_succeeded(run.path(), "turn").is_err());
        }
        fs::write(&status, r#"{"error":null}"#).unwrap();
        assert!(reference_succeeded(run.path(), "turn").is_ok());
    }
}
