//! Runs a scenario on the app's own windows through AppKit's accessibility tree,
//! as VoiceOver sees them: NSH views and AppKit controls answer the same
//! `NSAccessibility` labels, values and presses. Menus are tracked from AppKit's
//! notifications, since a menu's items live outside the windows' trees.
//!
//! A repeating timer in the run loop's common modes takes the steps, so they go
//! on while a menu or a modal alert runs its own loop. A press that opens one
//! returns only when it closes, so the step is recorded before pressing and the
//! press runs in a callout of its own, inside which the next ticks run.
//!
//! The driver posts mid-scenario `fixture` steps to the fixture Host's control
//! server, whose port the runner passes as `BEX_ACCEPTANCE_FIXTURE`. It keeps
//! the clipboard's items from the start and puts them back at the end, since
//! the app's copy actions write the user's clipboard.
//!
//! The runner reads `result.json` from the output directory: `passed` (null while
//! running) and one entry per step with `line`, `source`, `status` (`passed`,
//! `failed` or `skipped`), `message`, `elapsedMs` and the step's `artifact` file;
//! `evidence` names the screenshot and tree written when a step fails. A
//! `relaunch` step quits with `relaunch` set; the runner starts the app again
//! with `BEX_ACCEPTANCE_RESUME`, and the driver goes on after the steps the file
//! already records. A `capture` step's artifact is its screenshot; its views
//! are the `.json` beside it.
use super::script::{Composition, Find, Key, Line, Modifiers, Step};
use objc2::{
    MainThreadMarker, Message, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSMenu, NSMenuDidBeginTrackingNotification, NSMenuDidEndTrackingNotification,
    NSModalResponseOK, NSOpenPanel, NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardWriting, NSSavePanel, NSScreen, NSView, NSWindow,
};
use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
use objc2_foundation::{
    NSArray, NSAttributedString, NSData, NSDate, NSDictionary, NSNotification,
    NSNotificationCenter, NSNumber, NSPoint, NSRange, NSRect, NSRunLoop, NSRunLoopCommonModes,
    NSSize, NSString, NSTimer, NSURL,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

const TICK: f64 = 0.2;

// Darwin's per-thread CPU clock (time.h). Unlike wall time or sample counts,
// this excludes time the main run loop slept waiting for events.
fn main_thread_ms() -> f64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(clock_id: u32) -> u64;
    }
    const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
    unsafe { clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) as f64 / 1_000_000.0 }
}

/// Schedules `lines` on the main run loop. The returned flag says whether every
/// step passed once the app returns from its event loop.
/// `taken` are the steps an earlier process of the scenario recorded.
pub(crate) fn start(
    scenario: PathBuf,
    lines: Vec<Line>,
    taken: Vec<Value>,
    output: PathBuf,
    fixture: Option<u16>,
    mtm: MainThreadMarker,
) -> Rc<Cell<bool>> {
    let passed = Rc::new(Cell::new(false));
    let driver = Rc::new(RefCell::new(Driver {
        scenario,
        output,
        fixture,
        clipboard: Clipboard::save(),
        lines,
        index: taken.len(),
        since: Instant::now(),
        ready: false,
        finished: false,
        steps: taken,
        evidence: Vec::new(),
        menus: Vec::new(),
        passed: passed.clone(),
    }));
    let center = NSNotificationCenter::defaultCenter();
    for (name, open) in [
        (unsafe { NSMenuDidBeginTrackingNotification }, true),
        (unsafe { NSMenuDidEndTrackingNotification }, false),
    ] {
        let driver = driver.clone();
        let block = block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
            let notification = unsafe { notification.as_ref() };
            let Some(menu) = notification
                .object()
                .and_then(|object| object.downcast::<NSMenu>().ok())
            else {
                return;
            };
            let mut driver = driver.borrow_mut();
            driver
                .menus
                .retain(|tracked| !std::ptr::eq(&**tracked, &*menu));
            if open {
                driver.menus.push(menu);
            }
        });
        // The observers last as long as the process: the driver ends it.
        std::mem::forget(unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        });
    }
    let block = block2::RcBlock::new(move |_: NonNull<NSTimer>| tick(&driver, mtm));
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(TICK, true, &block) };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    passed
}

struct Driver {
    scenario: PathBuf,
    output: PathBuf,
    /// The fixture Host's control port.
    fixture: Option<u16>,
    clipboard: Clipboard,
    lines: Vec<Line>,
    index: usize,
    since: Instant,
    ready: bool,
    finished: bool,
    steps: Vec<Value>,
    evidence: Vec<String>,
    menus: Vec<Retained<NSMenu>>,
    passed: Rc<Cell<bool>>,
}

impl Driver {
    fn record(&mut self, status: &str, message: &str, artifact: Option<String>) {
        let line = &self.lines[self.index];
        eprintln!(
            "acceptance-step {}",
            json!({"source": line.source, "at": NSDate::timeIntervalSinceReferenceDate_class()})
        );
        self.steps.push(json!({
            "line": line.number,
            "source": line.source,
            "status": status,
            "message": message,
            "elapsedMs": self.since.elapsed().as_millis() as u64,
            "mainThreadMs": main_thread_ms(),
            "artifact": artifact,
        }));
        self.index += 1;
        self.since = Instant::now();
        ACTIVATING.set(false);
        self.write(None, false);
    }

    fn write(&self, passed: Option<bool>, relaunch: bool) {
        let report = json!({
            "scenario": self.scenario,
            "passed": passed,
            "relaunch": relaunch,
            "steps": self.steps,
            "evidence": self.evidence,
        });
        if let Err(reason) = std::fs::write(
            self.output.join("result.json"),
            serde_json::to_vec_pretty(&report).unwrap_or_default(),
        ) {
            eprintln!("acceptance: result.json: {reason}");
        }
    }
}

/// What a step found this tick.
enum Attempt {
    Pending(String),
    Passed(String, Option<String>),
    Press(String, Target),
    Failed(String),
    /// Quit for the runner to start the app again.
    Relaunch,
}

fn tick(driver: &Rc<RefCell<Driver>>, mtm: MainThreadMarker) {
    let (timeout, step, elapsed, menus, output, fixture) = {
        let mut driver = driver.borrow_mut();
        if driver.finished {
            return;
        }
        if !driver.ready {
            let Some(window) = main_window(mtm) else {
                return;
            };
            // Typed keys reach the key window's monitors only while the app is active.
            NSApplication::sharedApplication(mtm).activate();
            window.makeKeyAndOrderFront(None);
            driver.ready = true;
            driver.since = Instant::now();
        }
        let step = driver.lines[driver.index].step.clone();
        (
            limit(&step),
            step,
            driver.since.elapsed(),
            driver.menus.clone(),
            driver.output.clone(),
            driver.fixture,
        )
    };
    let result = attempt(&step, elapsed, &menus, &output, fixture, mtm);
    match result {
        Attempt::Passed(message, artifact) => {
            driver.borrow_mut().record("passed", &message, artifact)
        }
        Attempt::Press(message, target) => {
            driver.borrow_mut().record("passed", &message, None);
            // A press that opens a menu returns when the menu closes, and a timer
            // does not fire again during its own callout: the press gets a callout
            // of its own, and the next steps run in the menu's loop meanwhile.
            let press = block2::RcBlock::new(move |_: NonNull<NSTimer>| target.press());
            let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.0, false, &press) };
            unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        }
        Attempt::Pending(reason) if elapsed >= timeout => return fail(driver, &reason, mtm),
        Attempt::Pending(_) => {}
        Attempt::Failed(reason) => return fail(driver, &reason, mtm),
        Attempt::Relaunch => {
            driver.borrow_mut().record(
                "passed",
                "quit for the runner to start the app again",
                None,
            );
            return finish(driver, None);
        }
    }
    let done = {
        let driver = driver.borrow();
        !driver.finished && driver.index == driver.lines.len()
    };
    if done {
        finish(driver, Some(true));
    }
}

/// How long `step` may take.
fn limit(step: &Step) -> Duration {
    match step {
        Step::Press(find)
        | Step::Type(find, _)
        | Step::Compose(find, _)
        | Step::Paste(find)
        | Step::Undo(find)
        | Step::Scroll(find, ..)
        | Step::Select(find, _)
        | Step::Menu(find)
        | Step::Drag(find, _)
        | Step::DragBy(find, _)
        | Step::Click(_, find)
        | Step::Hover(find)
        | Step::Expect(find)
        | Step::ExpectAbsent(find)
        | Step::ExpectMenuBar(find)
        | Step::ExpectReadingOrder(find)
        | Step::ExpectScrolled(find, ..)
        | Step::ExpectCount(find, _)
        | Step::ExpectEnabled(find, _)
        | Step::ExpectHeight(find, ..)
        | Step::ExpectApart(find, _)
        | Step::ExpectInside(find) => find.timeout,
        Step::Wait(duration)
        | Step::ExpectClipboard(_, duration)
        | Step::ExpectFile(_, _, duration) => *duration,
        _ => super::script::DEFAULT_TIMEOUT,
    }
}

/// `fixture` is the fixture Host's control port.
fn attempt(
    step: &Step,
    elapsed: Duration,
    menus: &[Retained<NSMenu>],
    output: &Path,
    fixture: Option<u16>,
    mtm: MainThreadMarker,
) -> Attempt {
    if let Some(result) = expectation(step, menus, mtm) {
        return result;
    }
    match step {
        Step::Fixture(_) | Step::FirstLaunch | Step::WithoutCodex => {
            Attempt::Passed("prepared by the runner".into(), None)
        }
        Step::Control(path) => done(
            fixture
                .ok_or_else(|| "the runner gave no fixture control port".to_owned())
                .and_then(|port| post(port, path))
                .map(|()| format!("posted {path}")),
        ),
        Step::Wait(duration) if elapsed >= *duration => {
            Attempt::Passed(format!("waited {} ms", duration.as_millis()), None)
        }
        Step::Wait(_) => Attempt::Pending(String::new()),
        Step::Screenshot(name) => artifact(screenshot(mtm, output, name)),
        Step::Window(width, height) => done(resize(mtm, *width, *height)),
        Step::Capture(name) => artifact(capture(mtm, output, name)),
        Step::Dump(name) => artifact(dump(&Tree::read(mtm, menus), output, name)),
        Step::Compose(find, composition) => {
            on_element(&Tree::read(mtm, menus), find, |tree, index| {
                compose(tree, index, composition)
                    .map(|()| format!("{composition:?} in {}", tree.nodes[index].describe()))
            })
        }
        Step::Press(find) => press(&Tree::read(mtm, menus), find),
        Step::Type(find, text) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            let origin = fixture.map(|port| format!("http://127.0.0.1:{port}"));
            let text = match (text.contains(super::script::FIXTURE_ORIGIN), origin) {
                (false, _) => text.clone(),
                (true, Some(origin)) => text.replace(super::script::FIXTURE_ORIGIN, &origin),
                (true, None) => return Err("the runner gave no fixture control port".into()),
            };
            insert(tree, index, &text)
                .map(|()| format!("typed into {}", tree.nodes[index].describe()))
        }),
        Step::Paste(find) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            paste(tree, index).map(|()| format!("pasted into {}", tree.nodes[index].describe()))
        }),
        Step::Undo(find) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            undo(tree, index).map(|()| format!("undid in {}", tree.nodes[index].describe()))
        }),
        Step::Scroll(find, lines, held) => {
            on_element(&Tree::read(mtm, menus), find, |tree, index| {
                scroll(tree, index, *lines, held, mtm).map(|()| {
                    format!(
                        "scrolled {lines} lines over {}",
                        tree.nodes[index].describe()
                    )
                })
            })
        }
        Step::DragBy(find, points) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            drag_by(tree, index, *points).map(|()| {
                format!(
                    "dragged {} by {points} points",
                    tree.nodes[index].describe()
                )
            })
        }),
        Step::Select(find, text) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            select(tree, index, text)
                .map(|()| format!("selected {text:?} in {}", tree.nodes[index].describe()))
        }),
        Step::Menu(find) => click(&Tree::read(mtm, menus), find, Click::Secondary),
        Step::Click(modifiers, find) => click(
            &Tree::read(mtm, menus),
            find,
            Click::Primary(flags(modifiers)),
        ),
        Step::Drag(from, to) => drag(&Tree::read(mtm, menus), from, to),
        Step::Drop => done(release()),
        Step::Hover(find) => on_element(&Tree::read(mtm, menus), find, |tree, index| {
            hover(tree, index).map(|()| format!("hovered over {}", tree.nodes[index].describe()))
        }),
        Step::Choose(path) => choose(path, mtm),
        Step::ExpectFile(path, contains, _) => file_written(path, contains.as_deref()),
        Step::ExpectClipboard(text, _) => {
            let copied = Clipboard::text();
            if copied.contains(text.as_str()) {
                Attempt::Passed(format!("the clipboard has {copied:?}"), None)
            } else {
                Attempt::Pending(format!("the clipboard has {copied:?}"))
            }
        }
        Step::Copy { text, image } => {
            Clipboard::write(text, *image);
            Attempt::Passed(format!("copied {text:?}, with an image: {image}"), None)
        }
        Step::Key(key) => {
            done(post_key(key, mtm).map(|()| format!("posted key code {}", key.code)))
        }
        Step::Activate => activate(mtm),
        Step::DenyMicrophone => done(deny_microphone()),
        Step::Relaunch => Attempt::Relaunch,
        Step::Expect(_)
        | Step::ExpectAbsent(_)
        | Step::ExpectMenuBar(_)
        | Step::ExpectReadingOrder(_)
        | Step::ExpectScrolled(..)
        | Step::ExpectApart(..)
        | Step::ExpectInside(_)
        | Step::ExpectCount(..)
        | Step::ExpectEnabled(..)
        | Step::ExpectHeight(..) => unreachable!("expectations are attempted first"),
    }
}

/// A step that waits until something is so, or none for a step that acts.
fn expectation(step: &Step, menus: &[Retained<NSMenu>], mtm: MainThreadMarker) -> Option<Attempt> {
    Some(match step {
        Step::Expect(find) => shown(&Tree::read(mtm, menus), find, true),
        Step::ExpectAbsent(find) => shown(&Tree::read(mtm, menus), find, false),
        Step::ExpectMenuBar(find) => shown(&Tree::menu_bar(mtm), find, true),
        Step::ExpectReadingOrder(find) => reading_order(&Tree::read(mtm, menus), find),
        Step::ExpectScrolled(find, from, to) => {
            element_is(&Tree::read(mtm, menus), find, |object| {
                match (text(object, sel!(accessibilityRole)), scrolled(object)) {
                    (Some(role), Some(at)) if role == "AXScrollArea" => (
                        (*from..=*to).contains(&at),
                        format!("a scroll area scrolled to {at:.3}"),
                    ),
                    (role, _) => (
                        false,
                        format!("{} with no vertical scroll bar", role.unwrap_or_default()),
                    ),
                }
            })
        }
        Step::ExpectApart(find, other) => apart(&Tree::read(mtm, menus), find, other),
        Step::ExpectInside(find) => inside(&Tree::read(mtm, menus), find),
        Step::ExpectCount(find, count) => counted(&Tree::read(mtm, menus), find, *count),
        Step::ExpectEnabled(find, enabled) => element_is(&Tree::read(mtm, menus), find, |object| {
            let actual = !responds(object, sel!(isAccessibilityEnabled))
                || unsafe { msg_send![object, isAccessibilityEnabled] };
            (
                actual == *enabled,
                if actual { "enabled" } else { "disabled" }.into(),
            )
        }),
        Step::ExpectHeight(find, points, above) => {
            element_is(&Tree::read(mtm, menus), find, |object| {
                let frame: NSRect = unsafe { msg_send![object, accessibilityFrame] };
                let height = frame.size.height;
                (
                    (height > *points) == *above,
                    format!("{height} points tall"),
                )
            })
        }
        _ => return None,
    })
}

thread_local! {
    /// Whether the current `activate` step has asked Launch Services.
    static ACTIVATING: Cell<bool> = const { Cell::new(false) };
}

/// Asks Launch Services to bring the app forward, once per step: it reaches
/// this running app by its bundle, as a click in the Dock does.
fn activate(mtm: MainThreadMarker) -> Attempt {
    let app = NSApplication::sharedApplication(mtm);
    if app.isActive() {
        return Attempt::Passed("the app is active".into(), None);
    }
    if !ACTIVATING.replace(true) {
        let bundle = objc2_foundation::NSBundle::mainBundle()
            .bundlePath()
            .to_string();
        if !bundle.ends_with(".app") {
            return Attempt::Failed(format!("{bundle} is not an app bundle to activate"));
        }
        // Should Launch Services start another copy instead, it gets this
        // one's isolated Host rather than the user's.
        let mut open = std::process::Command::new("/usr/bin/open");
        open.args(["-a", &bundle]);
        for name in ["AGENT_STATE_DIR", "AGENT_ISOLATED_HOST"] {
            match std::env::var(name) {
                Ok(value) => open.args(["--env", &format!("{name}={value}")]),
                Err(_) => return Attempt::Failed(format!("{name} is not set")),
            };
        }
        if let Err(reason) = open.stdin(std::process::Stdio::null()).spawn() {
            return Attempt::Failed(format!("open -a {bundle}: {reason}"));
        }
    }
    Attempt::Pending("the app is not active yet".into())
}

/// Whether the element `press` would find is as `state` wants: it answers
/// whether it is, and how it is (`enabled`, `64 points tall`).
fn element_is(
    tree: &Tree,
    find: &Find,
    state: impl FnOnce(&AnyObject) -> (bool, String),
) -> Attempt {
    let index = match tree.labelled(find) {
        Ok(matches) => matches[0],
        Err(reason) => return Attempt::Pending(reason),
    };
    let node = &tree.nodes[index];
    let Target::Element(object) = &node.target else {
        return Attempt::Failed(format!("{} is no element", node.describe()));
    };
    let (wanted, actual) = state(object);
    let message = format!("{} is {actual}", node.describe());
    if wanted {
        Attempt::Passed(message, None)
    } else {
        Attempt::Pending(message)
    }
}

/// Whether the rows inside the element `find` names (their texts, folds and
/// buttons) read from the top of the screen down, in accessibility's order.
fn reading_order(tree: &Tree, find: &Find) -> Attempt {
    let index = match tree.labelled(find) {
        Ok(found) => found[0],
        Err(reason) => return Attempt::Pending(reason),
    };
    let depth = tree.nodes[index].depth;
    let texts: Vec<usize> = (index + 1..tree.nodes.len())
        .take_while(|&node| tree.nodes[node].depth > depth)
        .filter(|&node| {
            matches!(
                tree.nodes[node].role.as_str(),
                "AXTextArea" | "AXStaticText" | "AXButton"
            )
        })
        .filter(|&node| tree.frame(node).size.height > 0.0)
        .collect();
    if texts.len() < 2 {
        return Attempt::Pending(format!("{} elements to order", texts.len()));
    }
    // Screen coordinates grow upward: what is below reads after what is
    // above. Elements side by side on a line share its middle.
    let middle = |node: usize| {
        let frame = tree.frame(node);
        frame.origin.y + frame.size.height / 2.0
    };
    for pair in texts.windows(2) {
        if middle(pair[1]) > middle(pair[0]) + 2.0 {
            return Attempt::Pending(format!(
                "{} reads before {}, which is above it",
                tree.nodes[pair[0]].describe(),
                tree.nodes[pair[1]].describe()
            ));
        }
    }
    Attempt::Passed(format!("{} elements read top to bottom", texts.len()), None)
}

/// Where a scroll area's vertical scroll bar says it is scrolled to.
fn scrolled(object: &AnyObject) -> Option<f64> {
    if !responds(object, sel!(accessibilityVerticalScrollBar)) {
        return None;
    }
    let bar: Option<Retained<AnyObject>> =
        unsafe { msg_send![object, accessibilityVerticalScrollBar] };
    let value: Option<Retained<AnyObject>> = unsafe { msg_send![&*bar?, accessibilityValue] };
    Some(value?.downcast::<NSNumber>().ok()?.doubleValue())
}

fn done(result: Result<String, String>) -> Attempt {
    match result {
        Ok(message) => Attempt::Passed(message, None),
        Err(reason) => Attempt::Failed(reason),
    }
}

/// Whether some element after `find`'s anchor shows its text, as `present` wants.
fn shown(tree: &Tree, find: &Find, present: bool) -> Attempt {
    match (
        tree.start(find)
            .map(|start| tree.containing(&find.text, start)),
        present,
    ) {
        (Ok(Some(index)), true) => {
            Attempt::Passed(format!("found in {}", tree.nodes[index].describe()), None)
        }
        (Ok(Some(index)), false) => {
            Attempt::Pending(format!("still shown in {}", tree.nodes[index].describe()))
        }
        (Ok(None), true) => Attempt::Pending(format!("no element shows {:?}", find.text)),
        (Ok(None), false) => Attempt::Passed(format!("no element shows {:?}", find.text), None),
        // Without its anchor, nothing after it is shown.
        (Err(reason), true) => Attempt::Pending(reason),
        (Err(reason), false) => Attempt::Passed(reason, None),
    }
}

/// Whether exactly `count` elements after `find`'s anchor show its text.
fn counted(tree: &Tree, find: &Find, count: usize) -> Attempt {
    let start = match tree.start(find) {
        Ok(start) => start,
        Err(reason) => return Attempt::Pending(reason),
    };
    let shown = (start..tree.nodes.len())
        .filter(|&index| {
            tree.nodes[index]
                .texts
                .iter()
                .any(|(_, text)| text.contains(find.text.as_str()))
        })
        .count();
    let message = format!("{shown} elements show {:?}", find.text);
    if shown == count {
        Attempt::Passed(message, None)
    } else {
        Attempt::Pending(message)
    }
}

/// Whether the elements `find` names and those labelled `other` all show apart.
fn apart(tree: &Tree, find: &Find, other: &str) -> Attempt {
    let (first, second) = match (
        tree.labelled(find),
        tree.labelled(&Find {
            text: other.to_owned(),
            after: None,
            timeout: find.timeout,
        }),
    ) {
        (Ok(first), Ok(second)) => (first, second),
        (Err(reason), _) | (_, Err(reason)) => return Attempt::Pending(reason),
    };
    for &a in &first {
        for &b in &second {
            let (one, two) = (tree.frame(a), tree.frame(b));
            let shared = |start: f64, length: f64, other: f64, other_length: f64| {
                (start + length).min(other + other_length) - start.max(other)
            };
            // AppKit's buttons report a point of their bezel past their bounds.
            if one.size.width > 0.0
                && one.size.height > 0.0
                && two.size.width > 0.0
                && two.size.height > 0.0
                && shared(one.origin.x, one.size.width, two.origin.x, two.size.width) > 1.0
                && shared(one.origin.y, one.size.height, two.origin.y, two.size.height) > 1.0
            {
                return Attempt::Pending(format!(
                    "{} ({one:?}) overlaps {} ({two:?})",
                    tree.nodes[a].describe(),
                    tree.nodes[b].describe()
                ));
            }
        }
    }
    Attempt::Passed(
        format!(
            "{} elements apart from {} labelled {other:?}",
            first.len(),
            second.len()
        ),
        None,
    )
}

/// Whether every element `find` names lies whole inside its window.
fn inside(tree: &Tree, find: &Find) -> Attempt {
    let found = match tree.labelled(find) {
        Ok(found) => found,
        Err(reason) => return Attempt::Pending(reason),
    };
    for &index in &found {
        let Some(window) = &tree.nodes[index].window else {
            continue;
        };
        let (frame, bounds) = (tree.frame(index), window.frame());
        if frame.origin.x < bounds.origin.x
            || frame.origin.y < bounds.origin.y
            || frame.origin.x + frame.size.width > bounds.origin.x + bounds.size.width
            || frame.origin.y + frame.size.height > bounds.origin.y + bounds.size.height
        {
            return Attempt::Pending(format!(
                "{} reaches past its window ({frame:?} in {bounds:?})",
                tree.nodes[index].describe()
            ));
        }
    }
    Attempt::Passed(
        format!("{} elements inside their window", found.len()),
        None,
    )
}

fn press(tree: &Tree, find: &Find) -> Attempt {
    let matches = match tree.labelled(find) {
        Ok(matches) => matches,
        Err(reason) => return Attempt::Pending(reason),
    };
    if let Some(target) = matches.iter().find_map(|&index| tree.link(index)) {
        return Attempt::Press(
            format!("clicked {}", tree.nodes[matches[0]].describe()),
            target,
        );
    }
    // An element that takes no press (a label in a row) passes it to the
    // nearest ancestor that does.
    match matches
        .iter()
        .flat_map(|&index| tree.lineage(index))
        .find(|&index| tree.nodes[index].target.pressable())
    {
        Some(index) => Attempt::Press(
            format!("pressed {}", tree.nodes[index].describe()),
            tree.nodes[index].target.clone(),
        ),
        None => Attempt::Pending(format!(
            "{} takes no press now (disabled?)",
            tree.nodes[matches[0]].describe()
        )),
    }
}

/// `act` on the first element `find` names, once one is shown.
fn on_element(
    tree: &Tree,
    find: &Find,
    act: impl FnOnce(&Tree, usize) -> Result<String, String>,
) -> Attempt {
    // A menu item takes no text, wheel or selection: the element of the same
    // name is meant while the menu that offered it closes.
    match tree.labelled(find).map(|matches| {
        matches
            .into_iter()
            .find(|&index| !matches!(tree.nodes[index].target, Target::MenuItem { .. }))
    }) {
        Ok(Some(index)) => done(act(tree, index)),
        Ok(None) => Attempt::Pending(format!("only a menu item is labelled {:?}", find.text)),
        Err(reason) => Attempt::Pending(reason),
    }
}

fn artifact(written: Result<String, String>) -> Attempt {
    match written {
        Ok(file) => Attempt::Passed(format!("wrote {file}"), Some(file)),
        Err(reason) => Attempt::Failed(reason),
    }
}

fn fail(driver: &Rc<RefCell<Driver>>, reason: &str, mtm: MainThreadMarker) {
    let (output, menus) = {
        let driver = driver.borrow();
        (driver.output.clone(), driver.menus.clone())
    };
    let evidence: Vec<String> = [
        screenshot(mtm, &output, "failure"),
        dump(&Tree::read(mtm, &menus), &output, "failure-tree"),
        windows(mtm, &output),
    ]
    .into_iter()
    .filter_map(|written| {
        written
            .map_err(|reason| eprintln!("acceptance: {reason}"))
            .ok()
    })
    .collect();
    {
        let mut driver = driver.borrow_mut();
        driver.evidence = evidence;
        driver.record("failed", reason, None);
        while driver.index < driver.lines.len() {
            driver.record("skipped", "", None);
        }
    }
    finish(driver, Some(false));
}

/// Ends the driver's process with the scenario's result, or with none for the
/// runner to start the app again.
fn finish(driver: &Rc<RefCell<Driver>>, passed: Option<bool>) {
    let menus = {
        let mut driver = driver.borrow_mut();
        driver.finished = true;
        driver.passed.set(passed.unwrap_or(true));
        driver.write(passed, passed.is_none());
        driver.clipboard.restore();
        std::mem::take(&mut driver.menus)
    };
    for menu in menus {
        menu.cancelTracking();
    }
    nsh_platform_macos::quit_app();
}

/// The window the app draws in: the largest visible one.
fn main_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    NSApplication::sharedApplication(mtm)
        .orderedWindows()
        .iter()
        .filter(|window| window.isVisible() && window.contentView().is_some())
        .max_by(|a, b| {
            let area = |window: &NSWindow| window.frame().size.width * window.frame().size.height;
            area(a).total_cmp(&area(b))
        })
}

/// What a press acts on.
#[derive(Clone)]
enum Target {
    Element(Retained<AnyObject>),
    MenuItem {
        menu: Retained<NSMenu>,
        index: isize,
        root: Retained<NSMenu>,
    },
    /// A mouse click on an element of `view` (the element itself, or one
    /// AppKit's text views expose, as a link), placed when it is made.
    Click {
        element: Retained<AnyObject>,
        view: Retained<NSView>,
        click: Click,
    },
    /// A press at a point of a window, moved to another and held there.
    Drag {
        window: Retained<NSWindow>,
        from: NSPoint,
        to: NSPoint,
    },
    /// A link of a text view at a character, which offers no press of its own.
    Link {
        view: Retained<AnyObject>,
        link: Retained<AnyObject>,
        index: usize,
    },
}

impl Target {
    /// Whether it offers `AXPress` and is enabled. AppKit's controls do not
    /// report through the press's result whether it did anything, so this is
    /// asked first.
    fn pressable(&self) -> bool {
        match self {
            Self::Element(object) => {
                let press = sel!(accessibilityPerformPress);
                responds(object, press)
                    && (!responds(object, sel!(isAccessibilitySelectorAllowed:))
                        || unsafe { msg_send![&**object, isAccessibilitySelectorAllowed: press] })
                    && (!responds(object, sel!(isAccessibilityEnabled))
                        || unsafe { msg_send![&**object, isAccessibilityEnabled] })
            }
            Self::MenuItem { menu, index, .. } => menu
                .itemAtIndex(*index)
                .is_some_and(|item| item.isEnabled()),
            Self::Click { .. } | Self::Link { .. } | Self::Drag { .. } => true,
        }
    }

    fn press(&self) {
        match self {
            Self::Element(object) => {
                let _: bool = unsafe { msg_send![&**object, accessibilityPerformPress] };
            }
            Self::MenuItem { menu, index, root } => {
                // As a click on the item: its action, then the menu closes.
                menu.performActionForItemAtIndex(*index);
                root.cancelTracking();
            }
            Self::Link { view, link, index } => {
                let _: () = unsafe { msg_send![&**view, clickedOnLink: &**link, atIndex: *index] };
            }
            Self::Drag { window, from, to } => {
                // A click on an inactive window only activates it: NSH's
                // views take no first mouse.
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).activate();
                }
                window.makeKeyAndOrderFront(None);
                mouse(window, NSEventType::LeftMouseDown, *from);
                // In steps, as a hand moves, so a drag's claim sees it start
                // and its label follows.
                for step in 1..=DRAG_STEPS {
                    let along = f64::from(step) / f64::from(DRAG_STEPS);
                    let at = NSPoint::new(
                        from.x + (to.x - from.x) * along,
                        from.y + (to.y - from.y) * along,
                    );
                    mouse(window, NSEventType::LeftMouseDragged, at);
                }
                HELD.with_borrow_mut(|held| *held = Some((window.clone(), *to)));
            }
            Self::Click {
                element,
                view,
                click,
            } => click.send(element.clone(), view.clone()),
        }
    }
}

struct Node {
    target: Target,
    parent: Option<usize>,
    depth: usize,
    role: String,
    /// The attributes that have text, by name.
    texts: Vec<(&'static str, String)>,
    window: Option<Retained<NSWindow>>,
}

impl Node {
    fn describe(&self) -> String {
        let mut text = self
            .texts
            .first()
            .map(|(_, text)| text.clone())
            .unwrap_or_default();
        if text.chars().count() > 60 {
            text = text.chars().take(60).collect::<String>() + "…";
        }
        format!("{} {text:?}", self.role)
    }
}

/// Every element of the visible windows and open menus, parents before children.
struct Tree {
    nodes: Vec<Node>,
}

const MAX_NODES: usize = 50_000;

impl Tree {
    fn read(mtm: MainThreadMarker, menus: &[Retained<NSMenu>]) -> Self {
        let mut tree = Self { nodes: Vec::new() };
        let app = NSApplication::sharedApplication(mtm);
        // Ordered windows leave out panels (Quick Look's), which follow them.
        let ordered = app.orderedWindows();
        let panels: Vec<_> = app
            .windows()
            .into_iter()
            .filter(|window| !ordered.containsObject(window))
            .collect();
        for window in ordered.into_iter().chain(panels) {
            if window.isVisible() {
                let object = unsafe { Retained::cast_unchecked::<AnyObject>(window.clone()) };
                tree.walk(object, None, 0, &window);
            }
        }
        for root in menus {
            tree.menu(root, root, None, 0);
        }
        tree
    }

    fn walk(
        &mut self,
        object: Retained<AnyObject>,
        parent: Option<usize>,
        depth: usize,
        window: &Retained<NSWindow>,
    ) {
        if self.nodes.len() >= MAX_NODES || depth > 200 {
            return;
        }
        let mut texts = Vec::new();
        for (name, selector) in [
            ("label", sel!(accessibilityLabel)),
            ("title", sel!(accessibilityTitle)),
            ("value", sel!(accessibilityValue)),
            ("identifier", sel!(accessibilityIdentifier)),
            ("placeholder", sel!(accessibilityPlaceholderValue)),
        ] {
            if let Some(text) = text(&object, selector)
                && !text.trim().is_empty()
                && !texts.iter().any(|(_, seen)| *seen == text)
            {
                texts.push((name, text));
            }
        }
        let index = self.nodes.len();
        let children: Option<Retained<NSArray<AnyObject>>> =
            if responds(&object, sel!(accessibilityChildren)) {
                unsafe { msg_send![&*object, accessibilityChildren] }
            } else {
                None
            };
        self.nodes.push(Node {
            role: text(&object, sel!(accessibilityRole)).unwrap_or_default(),
            target: Target::Element(object),
            parent,
            depth,
            texts,
            window: Some(window.clone()),
        });
        for child in children.iter().flat_map(|children| children.iter()) {
            self.walk(child, Some(index), depth + 1, window);
        }
    }

    /// The app's menu bar and its menus, which the windows' tree leaves out.
    fn menu_bar(mtm: MainThreadMarker) -> Self {
        let mut tree = Self { nodes: Vec::new() };
        if let Some(bar) = NSApplication::sharedApplication(mtm).mainMenu() {
            tree.menu(&bar, &bar, None, 0);
        }
        tree
    }

    fn menu(
        &mut self,
        menu: &Retained<NSMenu>,
        root: &Retained<NSMenu>,
        parent: Option<usize>,
        depth: usize,
    ) {
        for index in 0..menu.numberOfItems() {
            let Some(item) = menu.itemAtIndex(index) else {
                continue;
            };
            if item.isSeparatorItem() || item.isHidden() {
                continue;
            }
            let node = self.nodes.len();
            self.nodes.push(Node {
                target: Target::MenuItem {
                    menu: menu.clone(),
                    index,
                    root: root.clone(),
                },
                parent,
                depth,
                role: "AXMenuItem".into(),
                texts: vec![("title", item.title().to_string())],
                window: None,
            });
            if let Some(submenu) = item.submenu() {
                self.menu(&submenu, root, Some(node), depth + 1);
            }
        }
    }

    /// Where `find`'s search begins: after its anchor, if it names one.
    fn start(&self, find: &Find) -> Result<usize, String> {
        let Some(anchor) = &find.after else {
            return Ok(0);
        };
        self.nodes
            .iter()
            .position(|node| {
                node.texts
                    .iter()
                    .any(|(_, text)| super::script::labels(text, anchor))
            })
            .map(|index| index + 1)
            .ok_or_else(|| format!("no element is labelled {anchor:?} to search after"))
    }

    fn containing(&self, needle: &str, start: usize) -> Option<usize> {
        (start..self.nodes.len()).find(|&index| {
            self.nodes[index]
                .texts
                .iter()
                .any(|(_, text)| text.contains(needle))
        })
    }

    /// Elements whose text [`super::script::labels`] `find`'s; open menus' items first.
    fn labelled(&self, find: &Find) -> Result<Vec<usize>, String> {
        let start = self.start(find)?;
        let mut found: Vec<usize> = (start..self.nodes.len())
            .filter(|&index| {
                self.nodes[index]
                    .texts
                    .iter()
                    .any(|(_, text)| super::script::labels(text, &find.text))
            })
            .collect();
        found.sort_by_key(|&index| !matches!(self.nodes[index].target, Target::MenuItem { .. }));
        if found.is_empty() {
            return Err(self.missing(&find.text));
        }
        Ok(found)
    }

    /// A click on the link `index` is, as its text view takes one: AppKit's
    /// links in text answer no press.
    fn link(&self, index: usize) -> Option<Target> {
        let node = &self.nodes[index];
        let label = node.texts.first().map(|(_, text)| text)?;
        let Target::Element(view) = &self.nodes[node.parent?].target else {
            return None;
        };
        if node.role != "AXLink" || !responds(view, sel!(clickedOnLink:atIndex:)) {
            return None;
        }
        let string = text(view, sel!(string))?;
        let index = string[..string.find(label.as_str())?]
            .encode_utf16()
            .count();
        let storage: Retained<AnyObject> = unsafe { msg_send![&**view, textStorage] };
        let name = NSString::from_str("NSLink");
        let link: Option<Retained<AnyObject>> = unsafe {
            msg_send![&*storage, attribute: &*name, atIndex: index, effectiveRange: std::ptr::null_mut::<NSRange>()]
        };
        Some(Target::Link {
            view: view.clone(),
            link: link?,
            index,
        })
    }

    /// Where `index` is on the screen.
    fn frame(&self, index: usize) -> NSRect {
        match &self.nodes[index].target {
            Target::Element(object) if responds(object, sel!(accessibilityFrame)) => unsafe {
                msg_send![&**object, accessibilityFrame]
            },
            _ => NSRect::ZERO,
        }
    }

    /// `index` and its ancestors, nearest first.
    fn lineage(&self, index: usize) -> Vec<usize> {
        std::iter::successors(Some(index), |&index| self.nodes[index].parent).collect()
    }

    fn missing(&self, label: &str) -> String {
        let similar: Vec<String> = self
            .nodes
            .iter()
            .filter(|node| {
                node.texts
                    .iter()
                    .any(|(_, text)| text.to_lowercase().contains(&label.to_lowercase()))
            })
            .take(8)
            .map(Node::describe)
            .collect();
        if similar.is_empty() {
            format!("no element is labelled {label:?}")
        } else {
            format!(
                "no element is labelled {label:?}; similar: {}",
                similar.join(", ")
            )
        }
    }

    fn text(&self) -> String {
        let mut text = String::new();
        for node in &self.nodes {
            text.push_str(&"  ".repeat(node.depth));
            text.push_str(&node.role);
            for (name, value) in &node.texts {
                text.push_str(&format!(" {name}={value:?}"));
            }
            text.push('\n');
        }
        text
    }
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// An object-valued accessibility attribute as text.
fn text(object: &AnyObject, selector: Sel) -> Option<String> {
    if !responds(object, selector) {
        return None;
    }
    let value: Option<Retained<AnyObject>> =
        unsafe { msg_send![object, performSelector: selector] };
    let value = value?;
    if let Some(string) = value.downcast_ref::<NSString>() {
        Some(string.to_string())
    } else if let Some(string) = value.downcast_ref::<NSAttributedString>() {
        Some(string.string().to_string())
    } else {
        value
            .downcast_ref::<NSNumber>()
            .map(|number| number.stringValue().to_string())
    }
}

fn insert(tree: &Tree, index: usize, text: &str) -> Result<(), String> {
    let responder = editor(tree, index)?;
    let text = NSString::from_str(text);
    // As the keyboard's input context commits text, where the view takes it so
    // (a text view, SwiftTerm's terminal).
    if responds(&responder, sel!(insertText:replacementRange:)) {
        let nowhere = NSRange::new(objc2_foundation::NSNotFound as usize, 0);
        let _: () =
            unsafe { msg_send![&*responder, insertText: &*text, replacementRange: nowhere] };
    } else {
        let _: () = unsafe { msg_send![&*responder, insertText: &*text] };
    }
    Ok(())
}

/// Takes `composition` as AppKit hands an input method's work to the focused
/// text view: no input source changes, so the user's is never touched.
fn compose(tree: &Tree, index: usize, composition: &Composition) -> Result<(), String> {
    let responder = editor(tree, index)?;
    if !responds(
        &responder,
        sel!(setMarkedText:selectedRange:replacementRange:),
    ) {
        return Err(format!(
            "{} takes no composition",
            tree.nodes[index].describe()
        ));
    }
    // No replacement range: the marked text, or else the selection.
    let marked = NSRange::new(objc2_foundation::NSNotFound as usize, 0);
    match composition {
        Composition::Mark(text) => {
            let caret = NSRange::new(text.encode_utf16().count(), 0);
            let text = NSString::from_str(text);
            let _: () = unsafe {
                msg_send![&*responder, setMarkedText: &*text, selectedRange: caret, replacementRange: marked]
            };
        }
        Composition::Commit(text) => {
            let text = NSString::from_str(text);
            let _: () =
                unsafe { msg_send![&*responder, insertText: &*text, replacementRange: marked] };
        }
        Composition::Unmark => {
            let _: () = unsafe { msg_send![&*responder, unmarkText] };
        }
    }
    Ok(())
}

/// Edit > Paste into the element `index`: its editor's `paste:` (an NSH input
/// reports the paste from there).
fn paste(tree: &Tree, index: usize) -> Result<(), String> {
    let responder = editor(tree, index)?;
    let _: () = unsafe { msg_send![&*responder, paste: Option::<&AnyObject>::None] };
    Ok(())
}

/// Edit > Undo in the element `index`: its editor's undo manager undoes the
/// last edit.
fn undo(tree: &Tree, index: usize) -> Result<(), String> {
    let responder = editor(tree, index)?;
    let manager: Option<Retained<AnyObject>> = unsafe { msg_send![&*responder, undoManager] };
    let manager = manager.ok_or("the editor keeps no undo history")?;
    let can: bool = unsafe { msg_send![&*manager, canUndo] };
    if !can {
        return Err("there is no edit to undo".into());
    }
    let _: () = unsafe { msg_send![&*manager, undo] };
    Ok(())
}

/// Focuses the element `index` and returns the text editor that took the focus.
fn editor(tree: &Tree, index: usize) -> Result<Retained<objc2_app_kit::NSResponder>, String> {
    let node = &tree.nodes[index];
    let (Target::Element(object), Some(window)) = (&node.target, &node.window) else {
        return Err(format!("{} takes no text", node.describe()));
    };
    // A control's cell (a search field's) stands for it in accessibility.
    let view: Option<Retained<NSView>> = match object.downcast_ref::<NSView>() {
        Some(view) => Some(view.retain()),
        None if responds(object, sel!(controlView)) => unsafe { msg_send![&**object, controlView] },
        None => None,
    };
    // Focusing a field again would end its composition, as leaving it does;
    // otherwise focusing selects the field's text, which typing replaces.
    let focused = view.is_some_and(|view| composing(window, &view) || focus(window, &view));
    if !focused {
        return Err(format!(
            "{} could not take the keyboard focus",
            node.describe()
        ));
    }
    let responder = window
        .firstResponder()
        .ok_or("the window has no first responder")?;
    if !responds(&responder, sel!(insertText:replacementRange:))
        && !responds(&responder, sel!(insertText:))
    {
        return Err(format!("{} does not edit text", node.describe()));
    }
    Ok(responder)
}

/// Whether `view`, or a view inside it (a field's editor), has the focus
/// with an input method's composition in it.
fn composing(window: &NSWindow, view: &NSView) -> bool {
    window
        .firstResponder()
        .and_then(|responder| responder.downcast::<NSView>().ok())
        .is_some_and(|focused| {
            focused.isDescendantOf(view)
                && responds(&focused, sel!(hasMarkedText))
                && unsafe { msg_send![&*focused, hasMarkedText] }
        })
}

/// Focuses `view`, or the first view inside it that takes the focus (a field in
/// its padding).
fn focus(window: &NSWindow, view: &NSView) -> bool {
    if view.acceptsFirstResponder() && window.makeFirstResponder(Some(view)) {
        return true;
    }
    view.subviews().iter().any(|child| focus(window, &child))
}

fn post_key(key: &Key, mtm: MainThreadMarker) -> Result<(), String> {
    let app = NSApplication::sharedApplication(mtm);
    let window = app
        .keyWindow()
        .or_else(|| main_window(mtm))
        .ok_or("no window takes keys")?;
    let mut flags = flags(&key.modifiers);
    // Arrow and function keys, as AppKit reports them.
    if (123..=126).contains(&key.code) || key.code == 109 {
        flags |= NSEventModifierFlags::NumericPad | NSEventModifierFlags::Function;
    }
    let characters = NSString::from_str(&key.characters);
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::new(0.0, 0.0), flags, 0.0, window.windowNumber(), None, &characters, &characters, false, key.code,
        ).ok_or("AppKit made no key event")?;
        // Queued as typing is, so the keymap's local monitor sees it.
        app.postEvent_atStart(&event, false);
    }
    Ok(())
}

/// The flags of the modifier keys `held`, as AppKit reports them.
fn flags(held: &Modifiers) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    for (on, flag) in [
        (held.command, NSEventModifierFlags::Command),
        (held.shift, NSEventModifierFlags::Shift),
        (held.option, NSEventModifierFlags::Option),
        (held.control, NSEventModifierFlags::Control),
    ] {
        if on {
            flags |= flag;
        }
    }
    flags
}

/// A mouse wheel over the element's center, handed to its window without moving
/// the pointer: NSH routes a wheel by where the event says it is.
fn scroll(
    tree: &Tree,
    index: usize,
    lines: i32,
    held: &Modifiers,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let node = &tree.nodes[index];
    let (Target::Element(object), Some(window)) = (&node.target, &node.window) else {
        return Err(format!("{} cannot be scrolled", node.describe()));
    };
    let frame = window.convertRectFromScreen(unsafe { msg_send![&**object, accessibilityFrame] });
    let height = NSScreen::screens(mtm)
        .firstObject()
        .map(|screen| screen.frame().size.height)
        .ok_or("no screen")?;
    // Content follows the wheel, so content moving up is a negative delta.
    let event = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 1, -lines, 0, 0)
        .ok_or("CoreGraphics made no wheel event")?;
    // AppKit's modifier flags are CoreGraphics' event flags.
    CGEvent::set_flags(
        Some(&event),
        objc2_core_graphics::CGEventFlags(flags(held).0 as u64),
    );
    // A window-less event's location in its window is its screen point, which
    // the window takes as its own: place it so that they match.
    CGEvent::set_location(
        Some(&event),
        objc2_core_foundation::CGPoint::new(
            frame.origin.x + frame.size.width / 2.0,
            height - (frame.origin.y + frame.size.height / 2.0),
        ),
    );
    let event = NSEvent::eventWithCGEvent(&event).ok_or("AppKit took no wheel event")?;
    window.sendEvent(&event);
    Ok(())
}

/// A pointer move to the element's centre, handed to the view under it: NSH
/// tracks hover from the moves its views receive.
fn hover(tree: &Tree, index: usize) -> Result<(), String> {
    let node = &tree.nodes[index];
    let (Target::Element(object), Some(window)) = (&node.target, &node.window) else {
        return Err(format!("{} cannot be hovered", node.describe()));
    };
    let frame = window.convertRectFromScreen(unsafe { msg_send![&**object, accessibilityFrame] });
    let point = NSPoint::new(
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::MouseMoved, point, NSEventModifierFlags::empty(), 0.0, window.windowNumber(), None, 0, 0, 0.0,
    ).ok_or("AppKit made no pointer event")?;
    let content = window.contentView().ok_or("the window has no content")?;
    let mut view = content
        .hitTest(content.convertPoint_fromView(point, None))
        .ok_or_else(|| format!("no view is under {}", node.describe()))?;
    // A native view (message text) keeps moves to itself, and only NSH's
    // root view tracks hover: the move goes to the outermost NSH view.
    let nsh = |view: &NSView| view.class().name().to_string_lossy().starts_with("Nsh");
    // SAFETY: views of the window, on the main thread.
    while let Some(parent) = unsafe { view.superview() }
        && (!nsh(&view) || nsh(&parent))
    {
        view = parent;
    }
    view.mouseMoved(&event);
    Ok(())
}

/// A left-button drag from the element's center by `points` down, handed to
/// its window in steps as a pointer's would be, without moving the pointer.
fn drag_by(tree: &Tree, index: usize, points: i32) -> Result<(), String> {
    let node = &tree.nodes[index];
    let (Target::Element(object), Some(window)) = (&node.target, &node.window) else {
        return Err(format!("{} cannot be dragged", node.describe()));
    };
    let frame = window.convertRectFromScreen(unsafe { msg_send![&**object, accessibilityFrame] });
    let start = objc2_foundation::NSPoint::new(
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    const STEPS: i32 = 8;
    let events = std::iter::once((NSEventType::LeftMouseDown, 0))
        .chain((1..=STEPS).map(|step| (NSEventType::LeftMouseDragged, step)))
        .chain(std::iter::once((NSEventType::LeftMouseUp, STEPS)));
    for (kind, step) in events {
        // Window coordinates grow upward; a drag down lowers y.
        let point = objc2_foundation::NSPoint::new(
            start.x,
            start.y - f64::from(points) * f64::from(step) / f64::from(STEPS),
        );
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, point, NSEventModifierFlags::empty(), 0.0, window.windowNumber(), None, 0, 1, 1.0,
        )
        .ok_or("AppKit made no mouse event")?;
        window.sendEvent(&event);
    }
    Ok(())
}

fn screenshot(mtm: MainThreadMarker, output: &Path, name: &str) -> Result<String, String> {
    let view = main_window(mtm)
        .and_then(|window| window.contentView())
        .ok_or("no window to capture")?;
    let bounds = view.bounds();
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or("the window cannot be captured")?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("the capture has no PNG")?;
    let file = format!("{name}.png");
    std::fs::write(output.join(&file), data.to_vec())
        .map_err(|reason| format!("{file}: {reason}"))?;
    Ok(file)
}

fn resize(mtm: MainThreadMarker, width: f64, height: f64) -> Result<String, String> {
    let window = main_window(mtm).ok_or("no window to resize")?;
    window.setContentSize(NSSize::new(width, height));
    Ok(format!("resized the window to {width}×{height}"))
}

/// The screenshot and every view of the main window as `<name>.json` (see the
/// step's documentation in `script.rs`).
fn capture(mtm: MainThreadMarker, output: &Path, name: &str) -> Result<String, String> {
    let file = screenshot(mtm, output, name)?;
    let content = main_window(mtm)
        .and_then(|window| window.contentView())
        .ok_or("no window to capture")?;
    let mut views = Vec::new();
    describe_views(&content, &content, None, &mut views);
    let size = content.bounds().size;
    let report = json!({
        "width": size.width,
        "height": size.height,
        "scale": content.window().map(|window| window.backingScaleFactor()).unwrap_or(1.0),
        "views": views,
    });
    let json = format!("{name}.json");
    std::fs::write(
        output.join(&json),
        serde_json::to_vec_pretty(&report).unwrap_or_default(),
    )
    .map_err(|reason| format!("{json}: {reason}"))?;
    Ok(file)
}

fn describe_views(view: &NSView, content: &NSView, parent: Option<usize>, out: &mut Vec<Value>) {
    if out.len() >= MAX_NODES {
        return;
    }
    let frame = view.convertRect_toView(view.bounds(), Some(content));
    // The content view of a window is not flipped: count from its top.
    let top = if content.isFlipped() {
        frame.origin.y
    } else {
        content.bounds().size.height - frame.origin.y - frame.size.height
    };
    let object: &AnyObject = view;
    let texts: serde_json::Map<String, Value> = [
        ("label", sel!(accessibilityLabel)),
        ("title", sel!(accessibilityTitle)),
        ("value", sel!(accessibilityValue)),
        ("identifier", sel!(accessibilityIdentifier)),
        ("placeholder", sel!(accessibilityPlaceholderValue)),
    ]
    .into_iter()
    .filter_map(|(name, selector)| {
        text(object, selector)
            .filter(|text| !text.trim().is_empty())
            .map(|text| (name.to_owned(), Value::from(text)))
    })
    .collect();
    let index = out.len();
    out.push(json!({
        "parent": parent,
        "class": view.class().name().to_string_lossy(),
        "frame": [frame.origin.x, top, frame.size.width, frame.size.height],
        "hidden": view.isHiddenOrHasHiddenAncestor(),
        "alpha": view.alphaValue(),
        "role": text(object, sel!(accessibilityRole)),
        "texts": texts,
        "layer": layer(view),
    }));
    for child in view.subviews() {
        describe_views(&child, content, Some(index), out);
    }
}

/// What Core Animation draws for the view: its layer's fill, border and corner
/// radius, and the keys of the animations running in it or its sublayers.
fn layer(view: &NSView) -> Value {
    let Some(layer): Option<Retained<AnyObject>> = (unsafe { msg_send![view, layer] }) else {
        return Value::Null;
    };
    let mut animations = Vec::new();
    animation_keys(&layer, &mut animations, 0);
    let background: *const AnyObject = unsafe { msg_send![&*layer, backgroundColor] };
    let border: *const AnyObject = unsafe { msg_send![&*layer, borderColor] };
    let radius: f64 = unsafe { msg_send![&*layer, cornerRadius] };
    let border_width: f64 = unsafe { msg_send![&*layer, borderWidth] };
    let opacity: f32 = unsafe { msg_send![&*layer, opacity] };
    json!({
        "background": rgba(background),
        "border": rgba(border),
        "borderWidth": border_width,
        "radius": radius,
        "opacity": opacity,
        "animations": animations,
    })
}

fn animation_keys(layer: &AnyObject, out: &mut Vec<String>, depth: usize) {
    let keys: Option<Retained<NSArray<NSString>>> = unsafe { msg_send![layer, animationKeys] };
    out.extend(
        keys.iter()
            .flat_map(|keys| keys.iter())
            .map(|key| key.to_string()),
    );
    let sublayers: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![layer, sublayers] };
    // A view's own content and mask layers, not the layers of its subviews,
    // which are views of their own.
    if depth < 4 {
        for sublayer in sublayers.iter().flat_map(|layers| layers.iter()) {
            let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![&*sublayer, delegate] };
            if delegate.is_some_and(|delegate| delegate.downcast_ref::<NSView>().is_some()) {
                continue;
            }
            animation_keys(&sublayer, out, depth + 1);
        }
    }
}

/// A `CGColor` as sRGB components from 0 to 255 and its alpha from 0 to 1.
fn rgba(color: *const AnyObject) -> Value {
    if color.is_null() {
        return Value::Null;
    }
    let class = objc2::class!(NSColor);
    let color: Option<Retained<AnyObject>> = unsafe { msg_send![class, colorWithCGColor: color] };
    let space: Retained<AnyObject> =
        unsafe { msg_send![objc2::class!(NSColorSpace), sRGBColorSpace] };
    let Some(color): Option<Retained<AnyObject>> =
        color.and_then(|color| unsafe { msg_send![&*color, colorUsingColorSpace: &*space] })
    else {
        return Value::Null;
    };
    let (red, green, blue, alpha): (f64, f64, f64, f64) = unsafe {
        (
            msg_send![&*color, redComponent],
            msg_send![&*color, greenComponent],
            msg_send![&*color, blueComponent],
            msg_send![&*color, alphaComponent],
        )
    };
    json!([
        (red * 255.0).round(),
        (green * 255.0).round(),
        (blue * 255.0).round(),
        alpha
    ])
}

fn dump(tree: &Tree, output: &Path, name: &str) -> Result<String, String> {
    let file = format!("{name}.txt");
    std::fs::write(output.join(&file), tree.text())
        .map_err(|reason| format!("{file}: {reason}"))?;
    Ok(file)
}

/// The app's activation and its windows' states, which say why a tree came
/// out empty: a hidden app's windows are not visible, nor are minimized ones.
fn windows(mtm: MainThreadMarker, output: &Path) -> Result<String, String> {
    let app = NSApplication::sharedApplication(mtm);
    let mut text = format!(
        "active={} hidden={} key={}\n",
        app.isActive(),
        app.isHidden(),
        app.keyWindow().is_some()
    );
    for window in app.windows() {
        text.push_str(&format!(
            "{:?} visible={} miniaturized={} occlusion={:?} frame={:?}\n",
            window.title().to_string(),
            window.isVisible(),
            window.isMiniaturized(),
            window.occlusionState(),
            window.frame()
        ));
    }
    let file = "failure-windows.txt".to_owned();
    std::fs::write(output.join(&file), text).map_err(|reason| format!("{file}: {reason}"))?;
    Ok(file)
}

/// Posts `path` to the fixture Host's loopback control server.
fn post(port: u16, path: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    let failed = |reason: std::io::Error| format!("fixture control: {reason}");
    let mut stream =
        std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).map_err(failed)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(failed)?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .map_err(failed)?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(failed)?;
    let status = response.split_whitespace().nth(1).unwrap_or_default();
    if status.starts_with('2') {
        Ok(())
    } else {
        Err(format!("fixture {path} answered {status:?}"))
    }
}

/// Selects the first `text` in the element's text through its accessibility.
fn select(tree: &Tree, index: usize, text: &str) -> Result<(), String> {
    let node = &tree.nodes[index];
    let Target::Element(object) = &node.target else {
        return Err(format!("{} has no text to select", node.describe()));
    };
    // AppKit exposes links as AX elements of their text view; selection
    // belongs to that owner. Drawn-text AX spans own their range directly.
    let linked = tree.link(index);
    let object = match &linked {
        Some(Target::Link { view, .. }) => view,
        _ => object,
    };
    if !responds(object, sel!(setAccessibilitySelectedTextRange:)) {
        return Err(format!("{} takes no selection", node.describe()));
    }
    let value = self::text(object, sel!(string))
        .or_else(|| self::text(object, sel!(accessibilityValue)))
        .or_else(|| self::text(object, sel!(accessibilityLabel)))
        .unwrap_or_default();
    let start = value
        .find(text)
        .ok_or_else(|| format!("{} does not contain {text:?}", node.describe()))?;
    let range = NSRange::new(
        value[..start].encode_utf16().count(),
        text.encode_utf16().count(),
    );
    // A drag takes the keyboard focus to the text it selects, where the Edit
    // menu's Copy then acts.
    if let (Some(view), Some(window)) = (object.downcast_ref::<NSView>(), &node.window)
        && view.acceptsFirstResponder()
    {
        window.makeFirstResponder(Some(view));
    }
    // A new drag first places a caret, including when it selects the same
    // text again after a toolbar action dismissed the old selection's UI.
    let caret = NSRange::new(range.location, 0);
    let _: () = unsafe { msg_send![&**object, setAccessibilitySelectedTextRange: caret] };
    let _: () = unsafe { msg_send![&**object, setAccessibilitySelectedTextRange: range] };
    Ok(())
}

/// A mouse click on the first element `find` names, as the window takes one.
fn click(tree: &Tree, find: &Find, click: Click) -> Attempt {
    let index = match tree.labelled(find) {
        Ok(matches) => matches[0],
        Err(reason) => return Attempt::Pending(reason),
    };
    let node = &tree.nodes[index];
    let Target::Element(element) = &node.target else {
        return Attempt::Failed(format!("{} takes no click", node.describe()));
    };
    // A link in text is an element of its text view, not a view.
    let Some(view) =
        tree.lineage(index)
            .into_iter()
            .find_map(|index| match &tree.nodes[index].target {
                Target::Element(object) => object.downcast_ref::<NSView>().map(NSView::retain),
                _ => None,
            })
    else {
        return Attempt::Failed(format!("{} is in no view", node.describe()));
    };
    Attempt::Press(
        format!("clicked {}", node.describe()),
        Target::Click {
            element: element.clone(),
            view,
            click,
        },
    )
}

/// How many moves a drag takes from its start to its end.
const DRAG_STEPS: u32 = 12;

thread_local! {
    /// The window and point of the press a `drag` holds until `drop`.
    static HELD: RefCell<Option<(Retained<NSWindow>, NSPoint)>> = const { RefCell::new(None) };
}

/// A press on `from`'s element moved onto `to`'s, once both are shown.
fn drag(tree: &Tree, from: &Find, to: &Find) -> Attempt {
    let center = |find: &Find| -> Result<(Retained<NSWindow>, NSPoint), String> {
        let index = tree.labelled(find)?[0];
        let node = &tree.nodes[index];
        let (Target::Element(object), Some(window)) = (&node.target, &node.window) else {
            return Err(format!("{} cannot be dragged to or from", node.describe()));
        };
        let frame =
            window.convertRectFromScreen(unsafe { msg_send![&**object, accessibilityFrame] });
        Ok((
            window.clone(),
            NSPoint::new(
                frame.origin.x + frame.size.width / 2.0,
                frame.origin.y + frame.size.height / 2.0,
            ),
        ))
    };
    match (center(from), center(to)) {
        (Ok((window, start)), Ok((_, end))) => Attempt::Press(
            format!("dragged {:?} onto {:?}", from.text, to.text),
            Target::Drag {
                window,
                from: start,
                to: end,
            },
        ),
        (Err(reason), _) | (_, Err(reason)) => Attempt::Pending(reason),
    }
}

/// Releases the press a `drag` holds.
fn release() -> Result<String, String> {
    let (window, at) = HELD
        .with_borrow_mut(Option::take)
        .ok_or("no drag holds a press")?;
    mouse(&window, NSEventType::LeftMouseUp, at);
    Ok(format!("released at {:.0}, {:.0}", at.x, at.y))
}

/// A left button's `kind` of event at `at` of `window`, handed to it now, as
/// the system times its events.
fn mouse(window: &NSWindow, kind: NSEventType, at: NSPoint) {
    let now = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, at, NSEventModifierFlags::empty(), now, window.windowNumber(), None, 0, 1, 1.0,
    );
    if let Some(event) = event {
        window.sendEvent(&event);
    }
}

/// Which button a click presses, and the modifier keys it holds.
#[derive(Clone, Copy, PartialEq)]
enum Click {
    Primary(NSEventModifierFlags),
    /// A right click: on an element's selected text, if it has some, as the
    /// selection's own menu shows there.
    Secondary,
}

impl Click {
    /// Queues the click as a user's comes: AppKit handles it as the current
    /// event then, for which NSH's root commits the scroll offsets it shows
    /// before it hit tests (`hitTest:`), so AppKit's views inside it place
    /// the click as NSH does. The click is placed on `element` when it is
    /// taken from the queue, where the element shows then; a primary click's
    /// release follows it there, for the tracking loop of a text view's link.
    /// A secondary press opens its menu, which the next steps choose from.
    fn send(self, element: Retained<AnyObject>, view: Retained<NSView>) {
        let Some(window) = view.window() else {
            return;
        };
        let app = NSApplication::sharedApplication(MainThreadMarker::from(&*window));
        let (press, release, flags) = match self {
            Self::Primary(flags) => (
                NSEventType::LeftMouseDown,
                Some(NSEventType::LeftMouseUp),
                flags,
            ),
            Self::Secondary => (
                NSEventType::RightMouseDown,
                None,
                NSEventModifierFlags::empty(),
            ),
        };
        let number = window.windowNumber();
        let event = move |kind, point| {
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, point, flags, objc2_foundation::NSProcessInfo::processInfo().systemUptime(),
                number, None, 0, 1, 1.0,
            )
        };
        let monitor = Rc::new(RefCell::new(None::<Retained<AnyObject>>));
        let placing = monitor.clone();
        let queue = app.clone();
        let place = block2::RcBlock::new(move |queued: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a live event.
            let kind = unsafe { queued.as_ref() }.r#type();
            if kind != press {
                return queued.as_ptr();
            }
            if let Some(monitor) = placing.borrow_mut().take() {
                // SAFETY: the monitor this handler belongs to, removed once.
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
            let point = shown_point(&element, &view, &window, self);
            if let Some(release) = release.and_then(|kind| event(kind, point)) {
                queue.postEvent_atStart(&release, false);
            }
            event(press, point).map_or(queued.as_ptr(), Retained::autorelease_return)
        });
        // SAFETY: the handler returns the event to handle, as AppKit asks.
        *monitor.borrow_mut() = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
                &place,
            )
        };
        if let Some(queued) = event(press, NSPoint::new(0.0, 0.0)) {
            app.postEvent_atStart(&queued, false);
        }
    }
}

/// The middle of what shows of `element` (of its selected text, for a
/// secondary click), in `window`'s coordinates. NSH's scroll views show a new
/// offset as their containers' layer transforms until they rest, and commit it
/// to the views' bounds only then: AppKit's geometry, the accessibility frames
/// included, lags while NSH picks the view under a click by what shows. So the
/// transforms around `view` move the element as they move what shows; without
/// them a click while the conversation still scrolls lands where the element
/// was. Each container then clips it to its own bounds: the middle of an
/// element partly scrolled out of its list is not on it.
fn shown_point(element: &AnyObject, view: &NSView, window: &NSWindow, click: Click) -> NSPoint {
    let selected = (click == Click::Secondary
        && responds(element, sel!(accessibilitySelectedTextRange)))
    .then(|| -> NSRange { unsafe { msg_send![element, accessibilitySelectedTextRange] } })
    .filter(|range| range.length > 0);
    let frame: NSRect = match selected {
        Some(range) => unsafe { msg_send![element, accessibilityFrameForRange: range] },
        None => unsafe { msg_send![element, accessibilityFrame] },
    };
    let mut frame = window.convertRectFromScreen(frame);
    let mut ancestor = superview(view);
    while let Some(container) = ancestor {
        let (x, y) = sublayer_offset(&container);
        if (x, y) != (0.0, 0.0) {
            let local = container.convertPoint_fromView(frame.origin, None);
            frame.origin =
                container.convertPoint_toView(NSPoint::new(local.x + x, local.y + y), None);
        }
        ancestor = superview(&container);
    }
    let middle = |rect: NSRect| {
        NSPoint::new(
            rect.origin.x + rect.size.width / 2.0,
            rect.origin.y + rect.size.height / 2.0,
        )
    };
    let mut shown = frame;
    let mut ancestor = superview(view);
    while let Some(container) = ancestor {
        let bounds = container.convertRect_toView(container.bounds(), None);
        match intersection(shown, bounds) {
            Some(clipped) => shown = clipped,
            // Nothing of it shows: its middle, where it would.
            None => return middle(frame),
        }
        ancestor = superview(&container);
    }
    middle(shown)
}

/// Where `a` and `b` overlap, if they do.
fn intersection(a: NSRect, b: NSRect) -> Option<NSRect> {
    let left = a.origin.x.max(b.origin.x);
    let bottom = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    (left < right && bottom < top).then(|| {
        NSRect::new(
            NSPoint::new(left, bottom),
            NSSize::new(right - left, top - bottom),
        )
    })
}

/// How far `view`'s layer shows its sublayers moved: an NSH scroll view's
/// offset that its bounds do not have yet.
fn sublayer_offset(view: &NSView) -> (f64, f64) {
    let layer: Option<Retained<AnyObject>> = unsafe { msg_send![view, layer] };
    let Some(layer) = layer else {
        return (0.0, 0.0);
    };
    let translation = |axis: &str| {
        let path = NSString::from_str(&format!("sublayerTransform.translation.{axis}"));
        let value: Option<Retained<NSNumber>> =
            unsafe { msg_send![&*layer, valueForKeyPath: &*path] };
        value.map_or(0.0, |value| value.doubleValue())
    };
    (translation("x"), translation("y"))
}

fn superview(view: &NSView) -> Option<Retained<NSView>> {
    // SAFETY: a getter on the main thread, where the driver runs.
    unsafe { view.superview() }
}

thread_local! {
    /// What the open panel answers once `choose` ends it.
    static CHOSEN: RefCell<Option<Retained<NSArray<NSURL>>>> =
        const { RefCell::new(None) };
}

/// The open panel's `URLs` while the driver chooses: AppKit offers no way to
/// select a file in it, and its view runs in another process.
unsafe extern "C-unwind" fn chosen_urls(_: *mut AnyObject, _: Sel) -> *mut NSArray<NSURL> {
    Retained::autorelease_return(CHOSEN.with_borrow(|urls| urls.clone().unwrap_or_default()))
}

/// The save panel's `URL` while the driver chooses, as `chosen_urls`.
unsafe extern "C-unwind" fn chosen_url(_: *mut AnyObject, _: Sel) -> *mut NSURL {
    CHOSEN.with_borrow(|urls| {
        urls.as_ref()
            .and_then(|urls| urls.firstObject())
            .map_or(std::ptr::null_mut(), Retained::autorelease_return)
    })
}

/// A step's path, a leading `$AGENT_STATE_DIR` being the fixture Host's directory.
fn resolve(path: &str) -> Result<String, String> {
    match path.strip_prefix(super::script::STATE_DIRECTORY) {
        Some(rest) => std::env::var("AGENT_STATE_DIR")
            .map(|directory| format!("{directory}{rest}"))
            .map_err(|_| "AGENT_STATE_DIR is not set".to_owned()),
        None => Ok(path.to_owned()),
    }
}

/// Picks `path` in the app's open or save panel once one shows: the panel
/// answers it as the user's choice and closes as its Open or Save button would.
/// An open panel's file must exist; a save panel's folder must.
fn choose(path: &str, mtm: MainThreadMarker) -> Attempt {
    let path = match resolve(path) {
        Ok(path) => path,
        Err(reason) => return Attempt::Failed(reason),
    };
    let Some((panel, parent)) = NSApplication::sharedApplication(mtm)
        .windows()
        .iter()
        .filter(|window| window.isVisible())
        .filter_map(|window| window.downcast::<NSSavePanel>().ok())
        .find_map(|panel| panel.sheetParent().map(|parent| (panel, parent)))
    else {
        return Attempt::Pending("no file panel shows".into());
    };
    let opening = panel.downcast_ref::<NSOpenPanel>().is_some();
    let needed = if opening {
        Some(std::path::Path::new(&path))
    } else {
        std::path::Path::new(&path).parent()
    };
    if !needed.is_some_and(std::path::Path::exists) {
        return Attempt::Failed(format!("{path} has nothing to choose"));
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
    CHOSEN.set(Some(NSArray::from_retained_slice(&[url])));
    let (selector, answer) = if opening {
        (
            sel!(URLs),
            // SAFETY: the replacement takes and returns what `URLs` does.
            unsafe {
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut NSArray<NSURL>,
                    objc2::runtime::Imp,
                >(chosen_urls)
            },
        )
    } else {
        (
            sel!(URL),
            // SAFETY: the replacement takes and returns what `URL` does.
            unsafe {
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut NSURL,
                    objc2::runtime::Imp,
                >(chosen_url)
            },
        )
    };
    let Some(method) = panel.class().instance_method(selector) else {
        return Attempt::Failed(format!("the panel has no {selector:?}"));
    };
    // SAFETY: `answer` has the method's signature.
    unsafe { method.set_implementation(answer) };
    parent.endSheet_returnCode(&panel, NSModalResponseOK);
    Attempt::Passed(format!("chose {path}"), None)
}

/// AVAudioApplication's permission request, answered as the user declining.
unsafe extern "C-unwind" fn microphone_denied(
    _: *mut AnyObject,
    _: Sel,
    answer: &block2::DynBlock<dyn Fn(objc2::runtime::Bool)>,
) {
    answer.call((objc2::runtime::Bool::NO,));
}

/// Makes the app's microphone permission requests answer as the user declining
/// the system's prompt, which a scenario cannot reach, from now on.
fn deny_microphone() -> Result<String, String> {
    let class = objc2::runtime::AnyClass::get(c"AVAudioApplication")
        .ok_or("AVAudioApplication is not loaded")?;
    let method = class
        .class_method(sel!(requestRecordPermissionWithCompletionHandler:))
        .ok_or("AVAudioApplication has no record permission request")?;
    // SAFETY: the replacement takes what the class method does and returns nothing.
    unsafe {
        method.set_implementation(std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                *mut AnyObject,
                Sel,
                &block2::DynBlock<dyn Fn(objc2::runtime::Bool)>,
            ),
            objc2::runtime::Imp,
        >(microphone_denied));
    }
    Ok("the microphone is denied".into())
}

/// Whether the file at `path` exists and has content yet.
fn file_written(path: &str, contains: Option<&str>) -> Attempt {
    let path = match resolve(path) {
        Ok(path) => path,
        Err(reason) => return Attempt::Failed(reason),
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Attempt::Pending(format!("{path} is not written"));
    };
    let text = String::from_utf8_lossy(&bytes);
    match contains {
        _ if bytes.is_empty() => Attempt::Pending(format!("{path} is empty")),
        Some(wanted) if !text.contains(wanted) => Attempt::Pending(format!("{path} has {text:?}")),
        _ => Attempt::Passed(format!("{path} has {} bytes", bytes.len()), None),
    }
}

/// The clipboard's items, each as its data by type.
struct Clipboard {
    items: Vec<Vec<(Retained<NSString>, Retained<NSData>)>>,
    /// The clipboard's change count then: unchanged, it is left alone.
    count: isize,
}

impl Clipboard {
    fn save() -> Self {
        let pasteboard = NSPasteboard::generalPasteboard();
        let items = pasteboard
            .pasteboardItems()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.types()
                            .iter()
                            .filter_map(|kind| item.dataForType(&kind).map(|data| (kind, data)))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            items,
            count: pasteboard.changeCount(),
        }
    }

    fn restore(&self) {
        let pasteboard = NSPasteboard::generalPasteboard();
        if pasteboard.changeCount() == self.count {
            return;
        }
        pasteboard.clearContents();
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = self
            .items
            .iter()
            .map(|kinds| {
                let item = NSPasteboardItem::new();
                for (kind, data) in kinds {
                    item.setData_forType(data, kind);
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        if !items.is_empty() {
            pasteboard.writeObjects(&NSArray::from_retained_slice(&items));
        }
    }

    /// One item of `text` (none when empty), with a PNG when `image`.
    fn write(text: &str, image: bool) {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let item = NSPasteboardItem::new();
        if !text.is_empty() {
            item.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        }
        if image {
            let png = include_bytes!(
                "../../../mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
            );
            item.setData_forType(&NSData::with_bytes(png), unsafe { NSPasteboardTypePNG });
        }
        pasteboard.writeObjects(&NSArray::from_retained_slice(&[
            ProtocolObject::from_retained(item),
        ]));
    }

    fn text() -> String {
        NSPasteboard::generalPasteboard()
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|text| text.to_string())
            .unwrap_or_default()
    }
}
