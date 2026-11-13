//! Pairs each compared element across the reference (T3's DOM or GPUI's
//! source values) and the NSH capture, measures both the same way and lists
//! every metric that differs beyond its tolerance.
use super::{
    map::{Element, GpuiLayout, Kind, Map, NshMatch, Reference, State, Tolerance, Up},
    pixels::{Image, Ink, Rect, Rgb, distance},
};
use serde::Deserialize;
use std::collections::BTreeMap;

/// `t3.json` from `scripts/nsh-parity-t3.ts`.
#[derive(Debug, Deserialize)]
pub struct T3Capture {
    pub elements: BTreeMap<String, Vec<T3Element>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct T3Element {
    pub frame: Rect,
    pub css: T3Style,
    #[serde(default)]
    pub animations: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct T3Style {
    pub font_size: String,
    pub font_weight: String,
    pub line_height: String,
    pub border_radius: String,
}

/// `nsh.json` from the acceptance driver's `capture` step.
#[derive(Debug, Deserialize)]
pub struct NshCapture {
    pub width: f64,
    pub height: f64,
    pub views: Vec<NshView>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NshView {
    pub parent: Option<usize>,
    pub class: String,
    pub frame: Rect,
    pub hidden: bool,
    #[serde(default = "opaque")]
    pub alpha: f64,
    pub role: Option<String>,
    pub texts: BTreeMap<String, String>,
    pub layer: Option<NshLayer>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NshLayer {
    pub background: Option<[f64; 4]>,
    pub radius: f64,
    #[serde(default = "opaque")]
    pub opacity: f64,
    pub animations: Vec<String>,
}

fn opaque() -> f64 {
    1.0
}

/// One side's element, measured.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Measure {
    pub frame: Rect,
    pub radius: Option<f64>,
    /// Endless animations in it or inside it. None means the renderer does
    /// not publish animation metadata (SwiftUI can redraw without CALayer keys).
    pub animations: Option<Vec<String>>,
    pub ink: Option<Ink>,
    /// The components of `frame` (x, y, width, height) the side leaves open.
    pub unknown: [bool; 4],
    /// Where the glyph's centre is from its container's centre, when known
    /// without pixels (GPUI's layout).
    pub glyph_offset: Pair,
    /// What the reference also says about it (T3's CSS font of text, the
    /// GPUI source that sets a value), shown with its differences.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Difference {
    pub element: String,
    pub metric: &'static str,
    pub reference: String,
    pub nsh: String,
    /// The element whose vertical offset explains a position's difference:
    /// measured from its top on each side, the position matches.
    pub follows: Option<String>,
}

/// The reference's elements of a state: from T3's capture, or GPUI's layout.
pub fn reference_measures(
    map: &Map,
    state: &State,
    t3: Option<(&T3Capture, &Image)>,
) -> BTreeMap<String, Measure> {
    elements(map, state)
        .filter_map(|element| {
            let measure = match state.reference {
                Reference::T3 => {
                    t3.and_then(|(capture, image)| t3_measure(capture, image, element))
                }
                Reference::Gpui => element.gpui.as_ref().map(gpui_measure),
            }?;
            Some((element.id.clone(), measure))
        })
        .collect()
}

pub fn nsh_measures(
    map: &Map,
    state: &State,
    capture: &NshCapture,
    image: &Image,
) -> BTreeMap<String, Measure> {
    elements(map, state)
        .filter_map(|element| {
            let index = find(&capture.views, &element.nsh)?;
            Some((
                element.id.clone(),
                nsh_measure(&capture.views, index, image),
            ))
        })
        .collect()
}

fn elements<'a>(map: &'a Map, state: &'a State) -> impl Iterator<Item = &'a Element> {
    state.elements.iter().filter_map(|id| map.element(id))
}

/// A metric's two components, each unless a side leaves it open.
pub type Pair = [Option<f64>; 2];

fn t3_measure(capture: &T3Capture, image: &Image, element: &Element) -> Option<Measure> {
    let found = capture.elements.get(&element.id)?.get(element.t3_nth)?;
    let css = &found.css;
    let radius = css
        .border_radius
        .trim_end_matches("px")
        .parse::<f64>()
        .ok()
        .map(|radius| clamp_radius(radius, found.frame));
    Some(Measure {
        frame: found.frame,
        radius,
        animations: Some(found.animations.clone()),
        ink: image.ink(found.frame, radius.unwrap_or(0.0)),
        note: if element.kind == Kind::Text {
            format!(
                "CSS {} / {} weight {}",
                css.font_size, css.line_height, css.font_weight
            )
        } else {
            String::new()
        },
        ..Measure::default()
    })
}

fn gpui_measure(layout: &GpuiLayout) -> Measure {
    let frame = [layout.x, layout.y, layout.width, layout.height];
    Measure {
        frame: frame.map(|value| value.unwrap_or(0.0)),
        unknown: frame.map(|value| value.is_none()),
        glyph_offset: [layout.glyph_dx, layout.glyph_dy],
        note: layout.source.clone(),
        ..Measure::default()
    }
}

fn nsh_measure(views: &[NshView], index: usize, image: &Image) -> Measure {
    let view = &views[index];
    let animations: Vec<_> = subtree(views, index)
        .filter_map(|index| views[index].layer.as_ref())
        .flat_map(|layer| layer.animations.iter().cloned())
        .collect();
    let hosted = subtree(views, index)
        .chain(std::iter::successors(Some(index), |&node| {
            views[node].parent
        }))
        .any(|node| {
            views[node].class.contains("NSHosting") || views[node].class.starts_with("SwiftUI.")
        });
    let radius = view
        .layer
        .as_ref()
        .map(|layer| clamp_radius(layer.radius, view.frame));
    Measure {
        frame: view.frame,
        radius,
        animations: (!hosted || !animations.is_empty()).then_some(animations),
        ink: image.ink(view.frame, radius.unwrap_or(0.0)),
        ..Measure::default()
    }
}

/// A radius as drawn: CSS's `9999px` of a pill is half its shorter side.
fn clamp_radius(radius: f64, frame: Rect) -> f64 {
    radius.min(frame[2].min(frame[3]) / 2.0)
}

/// `index` and the views inside it.
fn subtree(views: &[NshView], index: usize) -> impl Iterator<Item = usize> + '_ {
    (index..views.len()).take_while(move |&other| other == index || descends(views, other, index))
}

fn descends(views: &[NshView], mut view: usize, ancestor: usize) -> bool {
    while let Some(parent) = views[view].parent {
        if parent == ancestor {
            return true;
        }
        view = parent;
    }
    false
}

/// The `nth` shown view `matcher` names, then the ancestor it asks for, then
/// the descendant. An empty matcher names none: NSH draws no counterpart.
pub fn find(views: &[NshView], matcher: &NshMatch) -> Option<usize> {
    if *matcher
        == (NshMatch {
            up: matcher.up.clone(),
            nth: matcher.nth,
            down: matcher.down.clone(),
            ..NshMatch::default()
        })
    {
        return None;
    }
    let index = views
        .iter()
        .enumerate()
        .filter(|(index, view)| shown(views, *index) && matches(view, matcher))
        .map(|(index, _)| index)
        .nth(matcher.nth)?;
    let index = match &matcher.up {
        None => Some(index),
        Some(Up::Levels(levels)) => (0..*levels).try_fold(index, |view, _| views[view].parent),
        Some(Up::Named(name)) if name == "background" => {
            std::iter::successors(Some(index), |&view| views[view].parent)
                .find(|&view| fills(&views[view]))
        }
        Some(Up::Named(_)) => None,
    }?;
    matcher.down.iter().try_fold(index, |view, &nth| {
        (view + 1..views.len())
            .filter(|&child| views[child].parent == Some(view) && shown(views, child))
            .nth(nth)
    })
}

/// Drawn: laid out on screen and, as T3's side counts it, not transparent
/// through itself or an ancestor (hover-only controls).
pub(super) fn shown(views: &[NshView], index: usize) -> bool {
    let view = &views[index];
    let transparent = |view: &NshView| {
        view.alpha == 0.0
            || view
                .layer
                .as_ref()
                .is_some_and(|layer| layer.opacity == 0.0)
    };
    !view.hidden
        && view.frame[2] > 0.0
        && view.frame[3] > 0.0
        && view.frame[1] > -1000.0
        && !std::iter::successors(Some(index), |&view| views[view].parent)
            .any(|view| transparent(&views[view]))
}

fn fills(view: &NshView) -> bool {
    view.layer
        .as_ref()
        .and_then(|layer| layer.background)
        .is_some_and(|[.., alpha]| alpha > 0.0)
}

fn matches(view: &NshView, matcher: &NshMatch) -> bool {
    let texts = || view.texts.values().map(|text| text.trim());
    matcher
        .text
        .as_deref()
        .is_none_or(|wanted| texts().any(|text| text == wanted))
        && matcher
            .contains
            .as_deref()
            .is_none_or(|wanted| texts().any(|text| text.contains(wanted)))
        && matcher
            .role
            .as_deref()
            .is_none_or(|role| view.role.as_deref() == Some(role))
        && matcher
            .class
            .as_deref()
            .is_none_or(|class| view.class == class)
}

/// Every difference of `state` beyond tolerance, in the state's element order.
pub fn differences(
    map: &Map,
    state: &State,
    reference: &BTreeMap<String, Measure>,
    nsh: &BTreeMap<String, Measure>,
) -> Vec<Difference> {
    let mut out = Vec::new();
    for element in elements(map, state) {
        let pair = (reference.get(&element.id), nsh.get(&element.id));
        if state.absent.contains(&element.id) {
            if pair.0.is_some() || pair.1.is_some() {
                out.push(difference(
                    element,
                    "expected hidden",
                    if pair.0.is_some() { "shown" } else { "hidden" }.into(),
                    if pair.1.is_some() { "shown" } else { "hidden" }.into(),
                ));
            }
            continue;
        }
        let presence = match pair {
            (Some(_), None) => Some(("present", "shown", "missing")),
            (None, Some(_)) => Some(("present", "not shown", "shown")),
            (None, None) => Some(("not measured", "missing", "missing")),
            _ => None,
        };
        if let Some((metric, reference, nsh)) = presence {
            out.push(difference(element, metric, reference.into(), nsh.into()));
        }
        let (Some(theirs), Some(ours)) = pair else {
            continue;
        };
        let frames = Frames {
            map,
            state,
            element,
            reference,
            nsh,
        };
        out.extend(
            compare(
                element,
                map.tolerance.with(element.tolerance),
                theirs,
                ours,
                &frames,
            )
            .into_iter()
            .map(|difference| noted(difference, &theirs.note)),
        );
    }
    out
}

/// The frames of a state's elements on both sides, for anchors.
struct Frames<'a> {
    map: &'a Map,
    state: &'a State,
    element: &'a Element,
    reference: &'a BTreeMap<String, Measure>,
    nsh: &'a BTreeMap<String, Measure>,
}

impl Frames<'_> {
    /// Where positions are measured from on the reference side and NSH's:
    /// the column's left edge (its right one for an element at its end) and
    /// the window's top, or the anchor's.
    fn origins(&self) -> [[f64; 2]; 2] {
        let anchor = self.element.anchor.as_ref();
        // The column itself is placed in the window.
        let column = anchor.or(self
            .state
            .column
            .as_ref()
            .filter(|column| **column != self.element.id));
        [self.reference, self.nsh].map(|side| {
            let x = column.and_then(|id| side.get(id)).map_or(0.0, |measure| {
                measure.frame[0]
                    + if self.element.from_end {
                        measure.frame[2]
                    } else {
                        0.0
                    }
            });
            let y = anchor
                .and_then(|id| side.get(id))
                .map_or(0.0, |measure| measure.frame[1]);
            [x, y]
        })
    }

    /// The causes approvals name (`follows`) that this state measures, each
    /// with its top on the reference side and NSH's.
    fn causes(&self) -> impl Iterator<Item = (&str, [f64; 2])> {
        self.map
            .approvals
            .iter()
            .filter_map(|approval| approval.follows.as_deref())
            .filter_map(|cause| {
                let top = |side: &BTreeMap<String, Measure>| Some(side.get(cause)?.frame[1]);
                Some((cause, [top(self.reference)?, top(self.nsh)?]))
            })
    }

    /// What the glyph is centred in: a box's frame, or a text's drawn line
    /// (a field's icon sits on its text's centre line).
    fn container(&self) -> [Option<Rect>; 2] {
        let id = self.element.centred_in.as_ref();
        let text = id
            .and_then(|id| self.map.element(id))
            .is_some_and(|container| container.kind == Kind::Text);
        [self.reference, self.nsh].map(|side| {
            let measure = side.get(id?)?;
            Some(if text {
                drawn_bounds(measure).unwrap_or(measure.frame)
            } else {
                measure.frame
            })
        })
    }
}

fn compare(
    element: &Element,
    tolerance: Tolerance,
    reference: &Measure,
    nsh: &Measure,
    frames: &Frames,
) -> Vec<Difference> {
    let mut out = Vec::new();
    let [origin_ref, origin_nsh] = frames.origins();
    // Text is where its glyphs are drawn: a DOM line box and an AppKit text
    // view that stretches to its row frame the same text differently.
    let drawn_text = element.kind == Kind::Text
        && [reference, nsh]
            .iter()
            .all(|measure| drawn_bounds(measure).is_some());
    let frame = |measure: &Measure| {
        if drawn_text {
            drawn_bounds(measure).unwrap_or(measure.frame)
        } else {
            measure.frame
        }
    };
    let component = |measure: &Measure, index: usize, origin: f64| {
        (!measure.unknown[index]).then(|| frame(measure)[index] - origin)
    };
    let place = |measure: &Measure, origin: [f64; 2]| {
        let x = if element.from_end {
            // Its right edge's distance from the column's.
            (!measure.unknown[0] && !measure.unknown[2])
                .then(|| origin[0] - frame(measure)[0] - frame(measure)[2])
        } else {
            component(measure, 0, origin[0])
        };
        [x, component(measure, 1, origin[1])]
    };
    let size = |measure: &Measure| [component(measure, 2, 0.0), component(measure, 3, 0.0)];
    let (position, extent) = if drawn_text {
        ("text position (drawn)", "text extent (drawn)")
    } else {
        ("position", "size")
    };
    let pairs = [
        (
            position,
            place(reference, origin_ref),
            place(nsh, origin_nsh),
            tolerance.position,
        ),
        (extent, size(reference), size(nsh), tolerance.size),
    ];
    for (metric, theirs, ours, limit) in pairs {
        if exceeds(theirs, ours, limit) {
            let mut found = difference(element, metric, points(theirs), points(ours));
            // A position that matches measured from a cause's top follows it.
            if metric == position {
                found.follows = frames
                    .causes()
                    .find(|(_, [from_ref, from_nsh])| {
                        !exceeds(
                            place(reference, [origin_ref[0], *from_ref]),
                            place(nsh, [origin_nsh[0], *from_nsh]),
                            limit,
                        )
                    })
                    .map(|(cause, _)| cause.to_owned());
            }
            out.push(found);
        }
    }
    out.extend(corner_radius(
        element,
        tolerance.radius,
        reference.radius,
        nsh.radius,
    ));
    out.extend(fill(element, tolerance, reference, nsh));
    out.extend(drawn(
        element,
        tolerance,
        reference,
        nsh,
        frames.container(),
    ));
    out.extend(animation(element, reference, nsh));
    out
}

fn corner_radius(
    element: &Element,
    tolerance: f64,
    reference: Option<f64>,
    nsh: Option<f64>,
) -> Option<Difference> {
    if element.kind == Kind::Text {
        return None;
    }
    let theirs = reference?;
    match nsh {
        Some(ours) if (theirs - ours).abs() > tolerance => Some(difference(
            element,
            "corner radius",
            format!("{theirs:.0}"),
            format!("{ours:.0}"),
        )),
        None => Some(difference(
            element,
            "corner radius not measured",
            format!("{theirs:.0}"),
            "unavailable".into(),
        )),
        _ => None,
    }
}

fn animation(element: &Element, reference: &Measure, nsh: &Measure) -> Option<Difference> {
    let theirs = reference.animations.as_ref()?;
    let metric = match &nsh.animations {
        Some(ours) if theirs.is_empty() != ours.is_empty() => "animation metadata",
        None => "animation not measured",
        _ => return None,
    };
    Some(difference(
        element,
        metric,
        animations(reference),
        animations(nsh),
    ))
}

fn noted(mut difference: Difference, note: &str) -> Difference {
    if !note.is_empty() {
        difference.reference = format!("{} ({note})", difference.reference);
    }
    difference
}

fn drawn_bounds(measure: &Measure) -> Option<Rect> {
    measure.ink.as_ref()?.bounds
}

/// The fill of a box or a button; text shares its container's.
fn fill(
    element: &Element,
    tolerance: Tolerance,
    reference: &Measure,
    nsh: &Measure,
) -> Option<Difference> {
    if element.kind == Kind::Text {
        return None;
    }
    let (theirs, ours) = (
        reference.ink.as_ref()?.background,
        nsh.ink.as_ref()?.background,
    );
    (distance(theirs, ours) > tolerance.color)
        .then(|| difference(element, "fill", hex(theirs), hex(ours)))
}

/// Text: the first line's drawn height and its colour. Glyphs: the drawn
/// size, colour and offset from the centre of what they sit in.
fn drawn(
    element: &Element,
    tolerance: Tolerance,
    reference: &Measure,
    nsh: &Measure,
    containers: [Option<Rect>; 2],
) -> Vec<Difference> {
    let mut out = Vec::new();
    let inks = [reference.ink.as_ref(), nsh.ink.as_ref()];
    if element.kind == Kind::Text
        && let [Some(Some(theirs)), Some(Some(ours))] =
            inks.map(|ink| ink.map(|ink| ink.first_line))
        && (theirs - ours).abs() > tolerance.text
    {
        out.push(difference(
            element,
            "text line height (drawn)",
            format!("{theirs:.1}"),
            format!("{ours:.1}"),
        ));
    }
    if element.kind == Kind::Icon
        && let [Some(Some(theirs)), Some(Some(ours))] = inks.map(|ink| ink.map(|ink| ink.bounds))
        && exceeds(
            [Some(theirs[2]), Some(theirs[3])],
            [Some(ours[2]), Some(ours[3])],
            tolerance.text,
        )
    {
        out.push(difference(
            element,
            "glyph size (drawn)",
            points([Some(theirs[2]), Some(theirs[3])]),
            points([Some(ours[2]), Some(ours[3])]),
        ));
    }
    if element.kind != Kind::Box
        && let [Some(Some(theirs)), Some(Some(ours))] = inks.map(|ink| ink.map(|ink| ink.color))
        && distance(theirs, ours) > tolerance.color
    {
        out.push(difference(
            element,
            "text or glyph colour",
            hex(theirs),
            hex(ours),
        ));
    }
    let offsets = [(reference, containers[0]), (nsh, containers[1])]
        .map(|(measure, container)| glyph_offset(measure, container?));
    if let [Some(theirs), Some(ours)] = offsets
        && exceeds(theirs, ours, tolerance.text)
    {
        out.push(difference(
            element,
            "glyph centre from its container's centre",
            points(theirs),
            points(ours),
        ));
    }
    out
}

fn glyph_offset(measure: &Measure, container: Rect) -> Option<Pair> {
    if measure.glyph_offset.iter().any(Option::is_some) {
        return Some(measure.glyph_offset);
    }
    let bounds = measure.ink.as_ref()?.bounds?;
    Some([
        Some(bounds[0] + bounds[2] / 2.0 - (container[0] + container[2] / 2.0)),
        Some(bounds[1] + bounds[3] / 2.0 - (container[1] + container[3] / 2.0)),
    ])
}

/// Whether a component both sides know differs by more than `limit`.
fn exceeds(a: Pair, b: Pair, limit: f64) -> bool {
    a.iter()
        .zip(b)
        .any(|(a, b)| a.zip(b).is_some_and(|(a, b)| (a - b).abs() > limit))
}

fn difference(
    element: &Element,
    metric: &'static str,
    reference: String,
    nsh: String,
) -> Difference {
    Difference {
        element: element.id.clone(),
        metric,
        reference,
        nsh,
        follows: None,
    }
}

fn points(pair: Pair) -> String {
    let [a, b] =
        pair.map(|value| value.map_or_else(|| "–".to_owned(), |value| format!("{value:.1}")));
    format!("{a}, {b}")
}

fn hex(color: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn animations(measure: &Measure) -> String {
    let Some(animations) = &measure.animations else {
        return "unavailable".into();
    };
    if animations.is_empty() {
        "none".into()
    } else {
        animations.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nsh_parity::map::Map;

    const MAP: &str = r##"
[viewport]
width = 100
height = 50

[tolerance]
position = 2
size = 2
text = 1
color = 24
radius = 2

[[state]]
name = "turn"
screen = "Conversation"
description = "A turn"
nsh = []
elements = ["column", "row", "spinner", "icon", "thinking"]
column = "column"

[[state]]
name = "sidebar"
screen = "Sidebar"
description = "The sidebar"
reference = "gpui"
nsh = []
elements = ["field", "search"]

[[element]]
id = "column"
what = "Column"
kind = "box"
t3 = ".column"
nsh = { text = "Ask AI", up = "background" }

[[element]]
id = "row"
what = "Row"
kind = "text"
t3 = ".row"
nsh = { contains = "Worked" }

[[element]]
id = "spinner"
what = "Working"
kind = "box"
t3 = ".working"
nsh = { text = "Working" }

[[element]]
id = "icon"
what = "Icon"
kind = "icon"
t3 = ".icon"
nsh = { text = "Copy", role = "AXButton" }
centred_in = "row"

[[element]]
id = "thinking"
what = "Thinking"
kind = "text"
t3 = ".thinking"
nsh = { text = "Thinking" }

[[element]]
id = "field"
what = "Search field"
kind = "box"
nsh = { text = "Search", up = 1 }
gpui = { x = 12, y = 10, width = 60, height = 24, source = "sidebar.rs" }

[[element]]
id = "search"
what = "Search icon"
kind = "icon"
nsh = { text = "Search" }
centred_in = "field"
gpui = { x = 14, y = 14, width = 16, height = 16, glyph_dx = -20, glyph_dy = 0, source = "sidebar.rs" }
"##;

    fn view(parent: Option<usize>, frame: Rect, text: &str, layer: Option<NshLayer>) -> NshView {
        NshView {
            parent,
            class: "NshView".into(),
            frame,
            hidden: false,
            alpha: 1.0,
            role: Some(
                if text == "Copy" {
                    "AXButton"
                } else {
                    "AXUnknown"
                }
                .into(),
            ),
            texts: if text.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([("label".into(), text.into())])
            },
            layer,
        }
    }

    fn layer(background: Option<[f64; 4]>, radius: f64, animations: &[&str]) -> Option<NshLayer> {
        Some(NshLayer {
            background,
            radius,
            opacity: 1.0,
            animations: animations.iter().map(|key| (*key).into()).collect(),
        })
    }

    fn measure(frame: Rect) -> Measure {
        Measure {
            frame,
            animations: Some(vec![]),
            ..Measure::default()
        }
    }

    #[test]
    fn swiftui_without_layer_metadata_is_unmeasured_rather_than_static_or_square() {
        let map = Map::parse(MAP).unwrap();
        let element = map.element("column").unwrap();
        let image = Image {
            width: 20,
            height: 20,
            pixels: vec![[0; 3]; 400],
            scale: 1.0,
        };
        let mut hosted = view(None, [0.0, 0.0, 20.0, 20.0], "bubble", None);
        hosted.class = "SwiftUI.AccessibilityNode".into();
        let ours = nsh_measure(&[hosted], 0, &image);
        assert_eq!(ours.radius, None);
        assert_eq!(ours.animations, None);
        assert_eq!(
            corner_radius(element, 2.0, Some(16.0), ours.radius)
                .unwrap()
                .metric,
            "corner radius not measured"
        );
        assert!(corner_radius(element, 2.0, None, ours.radius).is_none());
        let mut reference = measure(ours.frame);
        for keys in [vec![], vec!["shimmer".into()]] {
            reference.animations = Some(keys);
            assert_eq!(
                animation(element, &reference, &ours).unwrap().metric,
                "animation not measured"
            );
        }
        let square = nsh_measure(
            &[view(None, ours.frame, "box", layer(None, 0.0, &[]))],
            0,
            &image,
        );
        assert_eq!(square.animations, Some(vec![]));
        assert_eq!(
            corner_radius(element, 2.0, Some(16.0), square.radius)
                .unwrap()
                .metric,
            "corner radius"
        );
        assert!(corner_radius(element, 2.0, Some(2.0), square.radius).is_none());
        assert!(corner_radius(map.element("row").unwrap(), 2.0, Some(0.0), None).is_none());
    }

    #[test]
    fn views_are_found_by_text_role_and_ancestors() {
        let views = vec![
            view(None, [0.0, 0.0, 100.0, 50.0], "", layer(None, 0.0, &[])),
            view(
                Some(0),
                [10.0, 30.0, 80.0, 18.0],
                "",
                layer(Some([15.0, 15.0, 15.0, 1.0]), 22.0, &[]),
            ),
            view(
                Some(1),
                [12.0, 32.0, 70.0, 10.0],
                "Ask AI",
                layer(None, 0.0, &[]),
            ),
            view(
                Some(0),
                [10.0, 5.0, 40.0, 10.0],
                "Worked for 3s",
                layer(None, 0.0, &[]),
            ),
            view(
                Some(3),
                [10.0, 5.0, 10.0, 10.0],
                "Copy",
                layer(None, 6.0, &["spin"]),
            ),
            view(
                Some(0),
                [10.0, -999_990.0, 40.0, 10.0],
                "Worked for 9s",
                layer(None, 0.0, &[]),
            ),
        ];
        let find = |matcher: NshMatch, nth| super::find(&views, &NshMatch { nth, ..matcher });
        assert_eq!(
            find(
                NshMatch {
                    text: Some("Ask AI".into()),
                    up: Some(Up::Named("background".into())),
                    ..NshMatch::default()
                },
                0
            ),
            Some(1)
        );
        assert_eq!(
            find(
                NshMatch {
                    contains: Some("Worked".into()),
                    ..NshMatch::default()
                },
                0
            ),
            Some(3)
        );
        // Off-screen (recycled) rows are not shown.
        assert_eq!(
            find(
                NshMatch {
                    contains: Some("Worked".into()),
                    ..NshMatch::default()
                },
                1
            ),
            None
        );
        assert_eq!(
            find(
                NshMatch {
                    text: Some("Copy".into()),
                    role: Some("AXButton".into()),
                    up: Some(Up::Levels(1)),
                    ..NshMatch::default()
                },
                0
            ),
            Some(3)
        );
        // A part no text names: a shown child of the named view's ancestor
        // (the off-screen row is not counted).
        assert_eq!(
            find(
                NshMatch {
                    text: Some("Copy".into()),
                    up: Some(Up::Levels(2)),
                    down: vec![1],
                    ..NshMatch::default()
                },
                0
            ),
            Some(3)
        );
        assert_eq!(
            find(
                NshMatch {
                    text: Some("Copy".into()),
                    role: Some("AXStaticText".into()),
                    ..NshMatch::default()
                },
                0
            ),
            None
        );
        assert_eq!(find(NshMatch::default(), 0), None);
        // A hover-only control is transparent through an ancestor: not shown.
        let mut hover_only = views.clone();
        hover_only[3].alpha = 0.0;
        let copy = NshMatch {
            text: Some("Copy".into()),
            ..NshMatch::default()
        };
        assert_eq!(super::find(&views, &copy), Some(4));
        assert_eq!(super::find(&hover_only, &copy), None);
        hover_only[3].alpha = 1.0;
        hover_only[4].layer.as_mut().unwrap().opacity = 0.0;
        assert_eq!(super::find(&hover_only, &copy), None);
        // The row's glyph, down from its label.
        let glyph = |down: Vec<usize>| NshMatch {
            contains: Some("Worked".into()),
            down,
            ..NshMatch::default()
        };
        assert_eq!(super::find(&views, &glyph(vec![0])), Some(4));
        assert_eq!(super::find(&views, &glyph(vec![1])), None);
        let image = Image {
            width: 100,
            height: 50,
            pixels: vec![[0; 3]; 5000],
            scale: 1.0,
        };
        let row = nsh_measure(&views, 3, &image);
        assert_eq!(row.animations, Some(vec!["spin".into()]));
        // A radius beyond half the height draws a pill.
        assert_eq!(nsh_measure(&views, 1, &image).radius, Some(9.0));
    }

    #[test]
    fn missing_both_selectors_cannot_pass_unless_the_state_requires_absence() {
        let mut map = Map::parse(MAP).unwrap();
        map.states[0].elements = vec!["row".into()];
        let empty = BTreeMap::new();
        let missing = differences(&map, &map.states[0], &empty, &empty);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].metric, "not measured");
        map.states[0].absent = vec!["row".into()];
        assert!(differences(&map, &map.states[0], &empty, &empty).is_empty());
        let shown = BTreeMap::from([("row".into(), measure([0.0, 0.0, 10.0, 10.0]))]);
        for (reference, candidate) in [(&shown, &empty), (&empty, &shown), (&shown, &shown)] {
            let unexpected = differences(&map, &map.states[0], reference, candidate);
            assert_eq!(unexpected.len(), 1);
            assert_eq!(unexpected[0].metric, "expected hidden");
        }
    }

    #[test]
    fn missing_moved_resized_and_still_elements_are_reported() {
        let map = Map::parse(MAP).unwrap();
        let state = &map.states[0];
        let reference = BTreeMap::from([
            ("column".to_owned(), measure([20.0, 30.0, 60.0, 18.0])),
            ("row".to_owned(), measure([20.0, 5.0, 60.0, 10.0])),
            (
                "spinner".to_owned(),
                Measure {
                    animations: Some(vec!["live-tool-shine".into()]),
                    ..measure([20.0, 15.0, 60.0, 10.0])
                },
            ),
            ("thinking".to_owned(), measure([20.0, 15.0, 60.0, 10.0])),
        ]);
        let nsh = BTreeMap::from([
            // The column starts 10 points further left: positions follow it.
            ("column".to_owned(), measure([10.0, 30.0, 61.0, 18.0])),
            ("row".to_owned(), measure([13.0, 5.0, 60.0, 14.0])),
            ("spinner".to_owned(), measure([10.0, 15.0, 60.0, 10.0])),
            ("icon".to_owned(), measure([10.0, 5.0, 10.0, 10.0])),
        ]);
        let found: Vec<_> = differences(&map, state, &reference, &nsh)
            .into_iter()
            .map(|difference| {
                (
                    difference.element,
                    difference.metric,
                    difference.reference,
                    difference.nsh,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "column".into(),
                    "position",
                    "20.0, 30.0".into(),
                    "10.0, 30.0".into()
                ),
                (
                    "row".into(),
                    "position",
                    "0.0, 5.0".into(),
                    "3.0, 5.0".into()
                ),
                (
                    "row".into(),
                    "size",
                    "60.0, 10.0".into(),
                    "60.0, 14.0".into()
                ),
                (
                    "spinner".into(),
                    "animation metadata",
                    "live-tool-shine".into(),
                    "none".into()
                ),
                ("icon".into(), "present", "not shown".into(), "shown".into()),
                (
                    "thinking".into(),
                    "present",
                    "shown".into(),
                    "missing".into()
                ),
            ]
        );
    }

    #[test]
    fn an_element_at_the_columns_end_is_placed_from_its_right_edge() {
        let at_end = MAP.replace(
            "id = \"row\"\nwhat = \"Row\"\nkind = \"text\"",
            "id = \"row\"\nwhat = \"Row\"\nkind = \"text\"\nfrom_end = true",
        );
        let map = Map::parse(&at_end).unwrap();
        let state = &map.states[0];
        let positions = |nsh_row: Rect| {
            let reference = BTreeMap::from([
                ("column".to_owned(), measure([20.0, 30.0, 60.0, 18.0])),
                ("row".to_owned(), measure([50.0, 5.0, 30.0, 10.0])),
            ]);
            // A narrower column: the row keeps its distance from its end.
            let nsh = BTreeMap::from([
                ("column".to_owned(), measure([20.0, 30.0, 50.0, 18.0])),
                ("row".to_owned(), measure(nsh_row)),
            ]);
            differences(&map, state, &reference, &nsh)
                .into_iter()
                .filter(|difference| difference.element == "row")
                .map(|difference| (difference.metric, difference.reference, difference.nsh))
                .collect::<Vec<_>>()
        };
        assert_eq!(positions([40.0, 5.0, 30.0, 10.0]), []);
        assert_eq!(
            positions([30.0, 5.0, 30.0, 10.0]),
            [("position", "0.0, 5.0".into(), "10.0, 5.0".into())]
        );
    }

    #[test]
    fn a_position_that_moves_with_a_cause_follows_it() {
        let caused = format!(
            "{MAP}\n[[approved]]\nfollows = \"column\"\nreason = \"r\"\napproved = \"2026-11-09\"\nby = \"b\"\n"
        );
        let map = Map::parse(&caused).unwrap();
        let state = &map.states[0];
        let follows = |nsh_row: Rect| {
            let reference = BTreeMap::from([
                ("column".to_owned(), measure([20.0, 30.0, 60.0, 18.0])),
                ("row".to_owned(), measure([20.0, 5.0, 60.0, 10.0])),
            ]);
            // The column stands 40 points higher.
            let nsh = BTreeMap::from([
                ("column".to_owned(), measure([20.0, -10.0, 60.0, 18.0])),
                ("row".to_owned(), measure(nsh_row)),
            ]);
            differences(&map, state, &reference, &nsh)
                .into_iter()
                .filter(|difference| difference.element == "row")
                .map(|difference| (difference.metric, difference.follows))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            follows([20.0, -35.0, 60.0, 10.0]),
            [("position", Some("column".to_owned()))],
            "moved with the column"
        );
        assert_eq!(
            follows([20.0, -20.0, 60.0, 10.0]),
            [("position", None)],
            "moved otherwise"
        );
    }

    #[test]
    fn drawn_text_glyphs_and_fills_are_compared() {
        let mut map = Map::parse(MAP).unwrap();
        map.states[0].elements = vec!["row".into(), "icon".into()];
        let state = &map.states[0];
        let ink = |line: f64, color: Rgb, bounds: Rect| Ink {
            background: [10, 10, 10],
            bounds: Some(bounds),
            color: Some(color),
            first_line: Some(line),
        };
        let with = |frame: Rect, ink: Ink, radius: f64| Measure {
            ink: Some(ink),
            radius: Some(radius),
            ..measure(frame)
        };
        let reference = BTreeMap::from([
            (
                "row".to_owned(),
                with(
                    [0.0, 0.0, 60.0, 20.0],
                    ink(12.0, [245, 245, 245], [2.0, 4.0, 30.0, 12.0]),
                    0.0,
                ),
            ),
            (
                "icon".to_owned(),
                with(
                    [40.0, 0.0, 20.0, 20.0],
                    ink(14.0, [160, 160, 160], [43.0, 3.0, 14.0, 14.0]),
                    6.0,
                ),
            ),
        ]);
        let nsh = BTreeMap::from([
            (
                "row".to_owned(),
                with(
                    [0.0, 0.0, 60.0, 20.0],
                    Ink {
                        background: [40, 40, 40],
                        ..ink(10.5, [245, 245, 245], [2.0, 4.0, 30.0, 10.0])
                    },
                    0.0,
                ),
            ),
            (
                "icon".to_owned(),
                with(
                    [40.0, 0.0, 20.0, 20.0],
                    Ink {
                        background: [40, 40, 40],
                        ..ink(16.0, [100, 100, 100], [42.0, 4.0, 16.0, 16.0])
                    },
                    2.0,
                ),
            ),
        ]);
        let found: Vec<_> = differences(&map, state, &reference, &nsh)
            .into_iter()
            .filter(|difference| difference.metric != "present")
            .map(|difference| (difference.element, difference.metric))
            .collect();
        assert_eq!(
            found,
            [
                // Text shares its container's fill: only the icon's is compared.
                ("row".into(), "text line height (drawn)"),
                ("icon".into(), "corner radius"),
                ("icon".into(), "fill"),
                ("icon".into(), "glyph size (drawn)"),
                ("icon".into(), "text or glyph colour"),
                ("icon".into(), "glyph centre from its container's centre"),
            ]
        );
    }

    #[test]
    fn components_a_side_leaves_open_are_not_compared() {
        assert!(!exceeds([None, Some(1.0)], [Some(9.0), Some(1.5)], 1.0));
        assert!(exceeds([None, Some(1.0)], [Some(9.0), Some(2.5)], 1.0));
        assert_eq!(points([None, Some(2.0)]), "–, 2.0");
        let layout = GpuiLayout {
            x: Some(12.0),
            height: Some(24.0),
            source: "sizing.rs:285".into(),
            ..GpuiLayout::default()
        };
        let measure = gpui_measure(&layout);
        assert_eq!(
            (measure.frame, measure.unknown),
            ([12.0, 0.0, 0.0, 24.0], [false, true, true, false])
        );
    }

    #[test]
    fn gpui_values_stand_for_the_reference_side() {
        let map = Map::parse(MAP).unwrap();
        let state = &map.states[1];
        let reference = reference_measures(&map, state, None);
        assert_eq!(reference["search"].glyph_offset, [Some(-20.0), Some(0.0)]);
        // NSH draws the glyph 1.5 points below the field's centre line.
        let glyph = Ink {
            background: [0; 3],
            bounds: Some([16.0, 16.5, 14.0, 14.0]),
            color: Some([200; 3]),
            first_line: Some(14.0),
        };
        let nsh = BTreeMap::from([
            ("field".to_owned(), measure([12.0, 10.0, 60.0, 24.0])),
            (
                "search".to_owned(),
                Measure {
                    ink: Some(glyph),
                    ..measure([14.0, 11.0, 20.0, 20.0])
                },
            ),
        ]);
        let found = differences(&map, state, &reference, &nsh);
        assert_eq!(
            found
                .iter()
                .map(|difference| (
                    difference.element.as_str(),
                    difference.metric,
                    difference.nsh.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("search", "position", "14.0, 11.0"),
                ("search", "size", "20.0, 20.0"),
                (
                    "search",
                    "glyph centre from its container's centre",
                    "-19.0, 1.5"
                ),
            ]
        );
    }

    #[test]
    fn a_glyph_in_text_is_centred_on_the_text_as_drawn() {
        let text_field = MAP.replace(
            "id = \"field\"\nwhat = \"Search field\"\nkind = \"box\"",
            "id = \"field\"\nwhat = \"Search field\"\nkind = \"text\"",
        );
        let map = Map::parse(&text_field).unwrap();
        let state = &map.states[1];
        let ink = |bounds: Rect| Ink {
            background: [0; 3],
            bounds: Some(bounds),
            color: Some([200; 3]),
            first_line: Some(10.0),
        };
        // The text's glyphs sit high in its frame (centre 17); the icon's
        // centre is 4 points below them, though on the frame's centre (22).
        let nsh = BTreeMap::from([
            (
                "field".to_owned(),
                Measure {
                    ink: Some(ink([20.0, 12.0, 40.0, 10.0])),
                    ..measure([12.0, 10.0, 60.0, 24.0])
                },
            ),
            (
                "search".to_owned(),
                Measure {
                    ink: Some(ink([14.0, 15.0, 14.0, 14.0])),
                    ..measure([14.0, 14.0, 16.0, 16.0])
                },
            ),
        ]);
        let found = differences(&map, state, &reference_measures(&map, state, None), &nsh);
        let offset = found
            .iter()
            .find(|difference| difference.metric == "glyph centre from its container's centre")
            .unwrap();
        assert_eq!(offset.nsh, "-19.0, 5.0");
    }
}
