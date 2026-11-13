//! The step language of an acceptance scenario: one step per line, `#` starts a
//! comment, and an argument with spaces is quoted (`"..."`, with `\"`, `\\` and
//! `\n` for a line break).
//!
//! - `fixture <path>`: posts `<path>` to the fixture Host's control server
//!   (`crates/host-fixture/src/pairing.rs`). The runner posts leading ones before
//!   the app starts; the driver posts later ones when it reaches them
//!   (`/release-inputs` while a turn waits).
//! - `first-launch`: the app starts as on its first launch, showing its setup.
//!   Without it the runner marks the setup done, as a returning user's.
//! - `host without-codex`: the runner starts the fixture Host without Codex, as
//!   on a PC where only Claude is installed.
//!
//!   Both come before the other steps, beside the leading `fixture` lines.
//! - `press <label> [after <anchor>] [timeout-ms]`: presses the first element
//!   whose label, title, value, identifier or placeholder is `<label>`, or an item
//!   of an open menu; a link in text is clicked. A `*` at either end of `<label>` matches any text there
//!   (`"*作業しました"`). With `after`, only elements after the first one labelled
//!   `<anchor>` in reading order count (a file row below a panel's header, not the
//!   same name in the conversation).
//! - `type <label> <text> [after <anchor>] [timeout-ms]`: focuses that element
//!   and inserts `<text>`, where `$AGENT_FIXTURE_ORIGIN` is the fixture Host's
//!   loopback server (its `/browser-page` is a page for the browser).
//! - `key <name>`: a key with optional modifiers, as `return`, `escape`, `cmd+,`
//!   or `shift+f10`.
//! - `click <modifiers> <label> [after <anchor>] [timeout-ms]`: a mouse click
//!   on the middle of the first element `<label>` names (a link in text
//!   included), with `<modifiers>` held: `none`, or `cmd`, `shift`, `option`
//!   and `ctrl` joined by `+`.
//! - `mark <label> <text> [after <anchor>] [timeout-ms]`: focuses that element
//!   and makes `<text>` its input method's marked text, replacing the marked
//!   text before it, as a Japanese input method does while converting.
//! - `commit <label> <text> ...` / `unmark <label> ...`: ends that composition
//!   as an input method does, inserting `<text>` in place of the marked text or
//!   accepting the marked text as it is.
//! - `expect <text> [after <anchor>] [timeout-ms]` / `expect_absent ...`: waits
//!   until some element's text contains `<text>`, or none does.
//! - `expect_scrolled <label> <from> <to> [after <anchor>] [timeout-ms]`: waits
//!   until that element is a scroll area whose vertical scroll bar says it is
//!   scrolled between `<from>` and `<to>` (0 at the content's start, 1 at its
//!   end), as VoiceOver reads its position.
//! - `expect_reading_order <label> [after <anchor>] [timeout-ms]`: waits until
//!   the texts, folds and buttons inside that element read, in accessibility
//!   order, from the top of the screen down, as VoiceOver reads them.
//! - `expect_menu_bar <text> [after <anchor>] [timeout-ms]`: waits until an
//!   item of the app's menu bar, or of one of its menus, is titled `<text>`;
//!   `after` names the menu it is in (`expect_menu_bar 取り消す after 編集`).
//! - `expect_count <text> <n> [after <anchor>] [timeout-ms]`: waits until
//!   exactly `<n>` elements' text contains `<text>`, whatever their order (a
//!   conversation's rows read in their list's order, not the screen's).
//! - `expect_enabled <label> [after <anchor>] [timeout-ms]` / `expect_disabled
//!   ...`: waits until the element `press` would find takes presses, or until it
//!   does not.
//! - `expect_height <label> above|below <points> [after <anchor>] [timeout-ms]`:
//!   waits until that element is taller, or shorter, than `<points>`.
//! - `scroll <label> <lines> [held <modifiers>] [after <anchor>] [timeout-ms]`:
//!   a mouse wheel over that element, with `<modifiers>` held as `click` names
//!   them; positive lines move the content up, toward its end.
//! - `expect_apart <label> <other> [after <anchor>] [timeout-ms]`: waits until
//!   elements labelled `<label>` (after the anchor) and `<other>` both show and
//!   none of the first overlaps one of the others, as minimap marks and message
//!   text must not.
//! - `expect_inside <label> [after <anchor>] [timeout-ms]`: waits until some
//!   element is labelled `<label>` and every one lies whole inside its window.
//! - `window <width> <height>`: resizes the main window's content to that many
//!   points.
//! - `drag_by <label> <points> [after <anchor>] [timeout-ms]`: drags that
//!   element with the left button by `points` down (negative: up), in steps,
//!   as a pointer would, and releases it.
//! - `select <label> <text> [after <anchor>] [timeout-ms]`: selects the first
//!   `<text>` inside that element's text, as a drag over it would.
//! - `menu <label> [after <anchor>] [timeout-ms]`: opens that element's context
//!   menu, as a right click on it; `press` then chooses its items.
//! - `drag <label> <target> [after <anchor>] [timeout-ms]`: presses the middle
//!   of that element, moves onto the middle of the first element labelled
//!   `<target>` and holds the button there, so `expect` and `screenshot` see the
//!   drag; `drop` releases it. `after` and the timeout find the dragged element.
//! - `hover <label> [after <anchor>] [timeout-ms]`: moves the pointer over that
//!   element's centre, as hover-only controls need.
//! - `choose <path>`: answers the open or save panel the app shows with
//!   `<path>`, waiting for one to appear. A leading `$AGENT_STATE_DIR` is the
//!   fixture Host's directory, whose `project` folder is the fixture's project.
//! - `expect_file <path> [contains <text>] [timeout-ms]`: waits until the file
//!   at `<path>` (as `choose` reads it) exists and is not empty, or contains
//!   `<text>`.
//! - `expect_clipboard <text> [timeout-ms]`: waits until the clipboard's text
//!   contains `<text>`. The driver restores the clipboard when the scenario ends.
//! - `copy <text>` / `copy_image [text]`: puts `<text>` on the clipboard, or an
//!   image with `[text]` beside it, as a screenshot tool or a page copies one.
//! - `paste <label> [after <anchor>] [timeout-ms]`: focuses that element and
//!   pastes the clipboard into it, as Edit > Paste does (a posted ⌘V reaches
//!   no field while the app is in the background).
//! - `undo <label> [after <anchor>] [timeout-ms]`: focuses that element and
//!   undoes its last edit, as Edit > Undo does.
//! - `activate`: brings the app to the front through Launch Services, as the
//!   user clicking it in the Dock: AppKit lets an app activate itself only
//!   while the user is in it. It takes the focus from the user briefly.
//! - `microphone deny`: answers the app's microphone permission requests from
//!   then on as the user declining the system's prompt.
//! - `relaunch`: quits the app as the user would; the runner starts it again on
//!   the same Host and state, and the next steps run in the new process.
//! - `wait <ms>`, `screenshot <name>`, `dump <name>` (the accessibility tree as text).
//! - `capture <name>`: `<name>.png` as `screenshot` writes it, and `<name>.json`: every
//!   view of the main window, parents first, with its frame in the content's points
//!   (origin top left), accessibility role and texts, its layer's background, border
//!   and corner radius, and the animations running in its layers. `cargo xtask
//!   nsh-parity` compares it with T3's rendering of the same state.
// Off the Mac only the tests read it.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use std::time::Duration;

/// What a `choose` path may begin with: the fixture Host's directory.
pub(crate) const STATE_DIRECTORY: &str = "$AGENT_STATE_DIR";
/// What `type` replaces with the fixture Host's loopback server, as
/// `http://127.0.0.1:<port>`: it serves `/browser-page` to the browser.
pub(crate) const FIXTURE_ORIGIN: &str = "$AGENT_FIXTURE_ORIGIN";

/// How long a step waits for its element or text unless the line says otherwise.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// A control path the runner posted before the app started.
    Fixture(String),
    /// The runner left the setup to show, as on a first launch.
    FirstLaunch,
    /// A control path the driver posts when it reaches the step.
    Control(String),
    /// The runner started the fixture Host without Codex.
    WithoutCodex,
    Press(Find),
    Type(Find, String),
    /// An input method's composition in the element: what it marks or commits.
    Compose(Find, Composition),
    Key(Key),
    Click(Modifiers, Find),
    Expect(Find),
    ExpectAbsent(Find),
    /// The element's texts and buttons read in their order on the screen.
    ExpectReadingOrder(Find),
    /// The element is a scroll area scrolled between the two fractions.
    ExpectScrolled(Find, f64, f64),
    /// An item of the menu bar shows the text.
    ExpectMenuBar(Find),
    /// This many elements show the text.
    ExpectCount(Find, usize),
    /// The element takes presses, or not.
    ExpectEnabled(Find, bool),
    /// The element is taller than this many points, or shorter when `false`.
    ExpectHeight(Find, f64, bool),
    /// A wheel of this many lines over the element, with the keys held.
    Scroll(Find, i32, Modifiers),
    /// No element `Find` names overlaps one labelled the text.
    ExpectApart(Find, String),
    /// Every element `Find` names lies inside its window.
    ExpectInside(Find),
    /// The main window's content size in points.
    Window(f64, f64),
    /// A left-button drag of the element by this many points down.
    DragBy(Find, i32),
    Select(Find, String),
    Menu(Find),
    Hover(Find),
    /// A press on the first element, moved onto the second and held there.
    /// The first is found after its anchor, if it names one.
    Drag(Find, Find),
    /// The held press of a drag, released where it is.
    Drop,
    Choose(String),
    /// The file is written, with this text in it if one is named.
    ExpectFile(String, Option<String>, Duration),
    ExpectClipboard(String, Duration),
    /// Text for the clipboard, with an image when `image`.
    Copy {
        text: String,
        image: bool,
    },
    Paste(Find),
    Undo(Find),
    Activate,
    DenyMicrophone,
    Relaunch,
    Wait(Duration),
    Screenshot(String),
    Dump(String),
    Capture(String),
}

/// A step of an input method's composition, as AppKit's `NSTextInputClient`
/// receives it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Composition {
    /// `setMarkedText:selectedRange:replacementRange:` with this text.
    Mark(String),
    /// `insertText:replacementRange:` with this text, replacing the marked text.
    Commit(String),
    /// `unmarkText`: the marked text stays as it is.
    Unmark,
}

/// What a step looks for, and how long it waits for it.
impl Step {
    /// Whether the runner takes the step before the app starts, as a scenario's
    /// leading lines.
    pub(crate) fn prepares(&self) -> bool {
        matches!(
            self,
            Self::Fixture(_) | Self::FirstLaunch | Self::WithoutCodex
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Find {
    pub text: String,
    /// Only elements after the first one labelled this count.
    pub after: Option<String>,
    pub timeout: Duration,
}

/// A key as AppKit reports it: its characters and virtual key code.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Key {
    pub characters: String,
    pub code: u16,
    pub modifiers: Modifiers,
}

/// The modifier keys a key or a click holds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Modifiers {
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line {
    pub number: usize,
    pub source: String,
    pub step: Step,
}

pub(crate) fn parse(script: &str) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    for (index, source) in script.lines().enumerate() {
        let number = index + 1;
        let source = source.trim();
        if source.is_empty() || source.starts_with('#') {
            continue;
        }
        let mut step = words(source)
            .and_then(|words| step(&words))
            .map_err(|reason| format!("line {number}: {reason}"))?;
        // Leading lines prepare the run before the app starts.
        let leading = lines.iter().all(|line: &Line| line.step.prepares());
        match &step {
            Step::Fixture(path) if !leading => step = Step::Control(path.clone()),
            Step::FirstLaunch | Step::WithoutCodex if !leading => {
                return Err(format!(
                    "line {number}: the run is prepared only by leading lines"
                ));
            }
            _ => {}
        }
        lines.push(Line {
            number,
            source: source.to_owned(),
            step,
        });
    }
    if lines.is_empty() {
        return Err("the scenario has no steps".into());
    }
    Ok(lines)
}

fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut characters = line.chars().peekable();
    while let Some(&first) = characters.peek() {
        if first.is_whitespace() {
            characters.next();
            continue;
        }
        let mut word = String::new();
        if first == '"' {
            characters.next();
            loop {
                match characters.next() {
                    Some('"') => break,
                    Some('\\') => match characters.next().ok_or("unfinished escape")? {
                        'n' => word.push('\n'),
                        escaped => word.push(escaped),
                    },
                    Some(character) => word.push(character),
                    None => return Err("unclosed quote".into()),
                }
            }
        } else {
            while let Some(&character) = characters.peek() {
                if character.is_whitespace() {
                    break;
                }
                word.push(character);
                characters.next();
            }
        }
        words.push(word);
    }
    Ok(words)
}

fn step(words: &[String]) -> Result<Step, String> {
    let name = words[0].as_str();
    let arguments: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    if name == "expect" || name.starts_with("expect_") {
        return expectation(name, &arguments);
    }
    Ok(match (name, arguments.as_slice()) {
        ("fixture", [path]) if path.starts_with('/') => Step::Fixture((*path).to_owned()),
        ("first-launch", []) => Step::FirstLaunch,
        ("host", ["without-codex"]) => Step::WithoutCodex,
        ("press", [target, rest @ ..]) => Step::Press(find(target, rest)?),
        ("type", [target, text, rest @ ..]) => Step::Type(find(target, rest)?, (*text).to_owned()),
        ("key", [name]) => Step::Key(key(name)?),
        ("click", [held, target, rest @ ..]) => Step::Click(
            if *held == "none" {
                Modifiers::default()
            } else {
                modifiers(held.split('+'))?
            },
            find(target, rest)?,
        ),
        ("mark", [target, text, rest @ ..]) => {
            Step::Compose(find(target, rest)?, Composition::Mark((*text).to_owned()))
        }
        ("commit", [target, text, rest @ ..]) => {
            Step::Compose(find(target, rest)?, Composition::Commit((*text).to_owned()))
        }
        ("unmark", [target, rest @ ..]) => Step::Compose(find(target, rest)?, Composition::Unmark),
        ("window", [width, height]) => Step::Window(points(width)?, points(height)?),
        ("scroll", [target, lines, rest @ ..]) => wheel(target, lines, rest)?,
        ("drag_by", [target, points, rest @ ..]) => Step::DragBy(
            find(target, rest)?,
            points
                .parse()
                .map_err(|_| format!("{points:?} is not a distance in points"))?,
        ),
        ("select", [target, text, rest @ ..]) => {
            Step::Select(find(target, rest)?, (*text).to_owned())
        }
        ("menu", [target, rest @ ..]) => Step::Menu(find(target, rest)?),
        ("hover", [target, rest @ ..]) => Step::Hover(find(target, rest)?),
        // `after` and a timeout name where the drag starts.
        ("drag", [from, to, rest @ ..]) => Step::Drag(find(from, rest)?, find(to, &[])?),
        ("drop", []) => Step::Drop,
        ("choose", [path]) if absolute(path) => Step::Choose((*path).to_owned()),
        ("copy", [text]) => Step::Copy {
            text: (*text).to_owned(),
            image: false,
        },
        ("copy_image", []) => Step::Copy {
            text: String::new(),
            image: true,
        },
        ("copy_image", [text]) => Step::Copy {
            text: (*text).to_owned(),
            image: true,
        },
        ("paste", [target, rest @ ..]) => Step::Paste(find(target, rest)?),
        ("undo", [target, rest @ ..]) => Step::Undo(find(target, rest)?),
        ("activate", []) => Step::Activate,
        ("microphone", ["deny"]) => Step::DenyMicrophone,
        ("relaunch", []) => Step::Relaunch,
        ("wait", [milliseconds]) => Step::Wait(self::milliseconds(milliseconds)?),
        ("screenshot", [name]) => Step::Screenshot(artifact(name)?),
        ("dump", [name]) => Step::Dump(artifact(name)?),
        ("capture", [name]) => Step::Capture(artifact(name)?),
        (name, _) => return Err(format!("unknown step or wrong arguments: {name}")),
    })
}

/// A step that waits until something is so.
fn expectation(name: &str, arguments: &[&str]) -> Result<Step, String> {
    Ok(match (name, arguments) {
        ("expect", [text, rest @ ..]) => Step::Expect(find(text, rest)?),
        ("expect_absent", [text, rest @ ..]) => Step::ExpectAbsent(find(text, rest)?),
        ("expect_menu_bar", [text, rest @ ..]) => Step::ExpectMenuBar(find(text, rest)?),
        ("expect_reading_order", [label, rest @ ..]) => {
            Step::ExpectReadingOrder(find(label, rest)?)
        }
        ("expect_scrolled", [label, from, to, rest @ ..]) => {
            Step::ExpectScrolled(find(label, rest)?, fraction(from)?, fraction(to)?)
        }
        ("expect_apart", [text, other, rest @ ..]) => {
            Step::ExpectApart(find(text, rest)?, (*other).to_owned())
        }
        ("expect_inside", [text, rest @ ..]) => Step::ExpectInside(find(text, rest)?),
        ("expect_count", [text, count, rest @ ..]) => Step::ExpectCount(
            find(text, rest)?,
            count
                .parse()
                .map_err(|_| format!("{count:?} is not a count"))?,
        ),
        ("expect_enabled", [label, rest @ ..]) => Step::ExpectEnabled(find(label, rest)?, true),
        ("expect_disabled", [label, rest @ ..]) => Step::ExpectEnabled(find(label, rest)?, false),
        ("expect_height", [target, side @ ("above" | "below"), points, rest @ ..]) => {
            Step::ExpectHeight(
                find(target, rest)?,
                points
                    .parse()
                    .map_err(|_| format!("{points:?} is not a number of points"))?,
                *side == "above",
            )
        }
        ("expect_file", [path, rest @ ..]) if absolute(path) => written(path, rest)?,
        ("expect_clipboard", [text]) => Step::ExpectClipboard((*text).to_owned(), DEFAULT_TIMEOUT),
        ("expect_clipboard", [text, timeout]) => {
            Step::ExpectClipboard((*text).to_owned(), milliseconds(timeout)?)
        }
        (name, _) => return Err(format!("unknown step or wrong arguments: {name}")),
    })
}

/// Whether a step's `path` is absolute or in the fixture Host's directory.
fn absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with(STATE_DIRECTORY)
}

/// Whether an element's `text` is the `label` a step names, `*` at either end of
/// it matching any text there.
pub(crate) fn labels(text: &str, label: &str) -> bool {
    let text = text.trim();
    match (label.strip_prefix('*'), label.strip_suffix('*')) {
        (Some(rest), Some(_)) if !rest.is_empty() => text.contains(&rest[..rest.len() - 1]),
        (Some(rest), _) => text.ends_with(rest),
        (None, Some(rest)) => text.starts_with(rest),
        (None, None) => text == label,
    }
}

fn find(text: &str, rest: &[&str]) -> Result<Find, String> {
    let (after, rest) = match rest {
        ["after", anchor, rest @ ..] => (Some((*anchor).to_owned()), rest),
        _ => (None, rest),
    };
    let timeout = match rest {
        [] => DEFAULT_TIMEOUT,
        [value] => milliseconds(value)?,
        _ => return Err("too many arguments".into()),
    };
    Ok(Find {
        text: text.to_owned(),
        after,
        timeout,
    })
}

/// An `expect_file` step: `contains <text>` comes before its timeout.
fn written(path: &str, rest: &[&str]) -> Result<Step, String> {
    let (contains, rest) = match rest {
        ["contains", text, rest @ ..] => (Some((*text).to_owned()), rest),
        _ => (None, rest),
    };
    let timeout = match rest {
        [] => DEFAULT_TIMEOUT,
        [value] => milliseconds(value)?,
        _ => return Err("too many arguments".into()),
    };
    Ok(Step::ExpectFile(path.to_owned(), contains, timeout))
}

/// A `scroll` step: `held <modifiers>` comes before where it finds its element.
fn wheel(target: &str, lines: &str, rest: &[&str]) -> Result<Step, String> {
    let (held, rest) = match rest {
        ["held", held, rest @ ..] => (modifiers(held.split('+'))?, rest),
        _ => (Modifiers::default(), rest),
    };
    let lines = lines
        .parse()
        .map_err(|_| format!("{lines:?} is not a line count"))?;
    Ok(Step::Scroll(find(target, rest)?, lines, held))
}

fn fraction(value: &str) -> Result<f64, String> {
    value
        .parse()
        .ok()
        .filter(|fraction: &f64| (0.0..=1.0).contains(fraction))
        .ok_or_else(|| format!("{value:?} is not a fraction from 0 to 1"))
}

fn points(value: &str) -> Result<f64, String> {
    value
        .parse()
        .ok()
        .filter(|points: &f64| points.is_finite() && *points > 0.0)
        .ok_or_else(|| format!("{value:?} is not a size in points"))
}

fn milliseconds(value: &str) -> Result<Duration, String> {
    value
        .parse()
        .map(Duration::from_millis)
        .map_err(|_| format!("{value:?} is not a number of milliseconds"))
}

/// A file name of the scenario's output directory.
fn artifact(name: &str) -> Result<String, String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
    {
        Ok(name.to_owned())
    } else {
        Err(format!(
            "{name:?} must be letters, digits, '-' or '_' to name a file"
        ))
    }
}

fn key(spec: &str) -> Result<Key, String> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    let name = parts
        .pop()
        .filter(|name| !name.is_empty())
        .ok_or("no key")?;
    let mut key = named(name).ok_or_else(|| format!("unknown key {name:?}"))?;
    key.modifiers = modifiers(parts)?;
    Ok(key)
}

fn modifiers<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Modifiers, String> {
    let mut held = Modifiers::default();
    for name in names {
        let flag = match name {
            "cmd" => &mut held.command,
            "shift" => &mut held.shift,
            "option" | "alt" => &mut held.option,
            "ctrl" => &mut held.control,
            other => return Err(format!("unknown modifier {other:?}")),
        };
        *flag = true;
    }
    Ok(held)
}

/// ANSI virtual key codes of the keys scenarios name.
fn named(name: &str) -> Option<Key> {
    let function = |code: u16, unicode: u32| (char::from_u32(unicode).unwrap().to_string(), code);
    let (characters, code) = match name {
        "return" => ("\r".to_owned(), 36),
        "tab" => ("\t".to_owned(), 48),
        "space" => (" ".to_owned(), 49),
        "delete" => ("\u{7f}".to_owned(), 51),
        "escape" => ("\u{1b}".to_owned(), 53),
        "left" => function(123, 0xF702),
        "right" => function(124, 0xF703),
        "down" => function(125, 0xF701),
        "up" => function(126, 0xF700),
        "f10" => function(109, 0xF70D),
        _ => {
            let mut characters = name.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            const CODES: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.";
            let code = CODES.find(character.to_ascii_lowercase())?;
            (character.to_ascii_lowercase().to_string(), code as u16)
        }
    };
    Some(Key {
        characters,
        code,
        modifiers: Modifiers::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_take_quoted_labels_and_optional_timeouts() {
        let lines = parse(
            "# Comment\nfixture /external-conversation\nfirst-launch\nhost without-codex\n\npress \"新しい チャット\"\n\
             type メッセージ \"say \\\"hi\\\"\" 500\nexpect done after 見出し 20000\nexpect_absent x\nexpect_menu_bar 取り消す after 編集\nexpect_reading_order この会話\nexpect_scrolled この会話 0.5 1 after 見出し\nexpect_count 回答 2\nexpect_disabled 送信 2000\nexpect_height 入力 above 60\n\
             scroll 会話 -3\nscroll 画像 2 held cmd after 見出し\nexpect_apart \"移動:*\" \"message.*\" 500\nwindow 660 700\nexpect_inside 送信 1000\nselect 回答 Needle\nmenu 回答 after 見出し\nhover 回答\ndrag nested \"AI に依頼する\" after 見出し\ndrop\nchoose /tmp/a.txt\nchoose $AGENT_STATE_DIR/project\n\
             expect_file $AGENT_STATE_DIR/saved.png 500\nexpect_file /tmp/b.png contains \"a.txt:2\"\n\
             expect_clipboard \"a b\" 500\ncopy \"a\\nb\"\ncopy_image\ncopy_image 説明\npaste 入力欄 after 見出し\nundo 入力欄 500\n\
             fixture /release-inputs\nactivate\nmicrophone deny\nrelaunch\nwait 250\nscreenshot after-send\ndump tree_1\nwindow 1280 800\ncapture state_1\n",
        )
        .unwrap();
        let steps: Vec<_> = lines.iter().map(|line| line.step.clone()).collect();
        assert_eq!(
            steps,
            [
                Step::Fixture("/external-conversation".into()),
                Step::FirstLaunch,
                Step::WithoutCodex,
                Step::Press(Find {
                    text: "新しい チャット".into(),
                    after: None,
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::Type(
                    Find {
                        text: "メッセージ".into(),
                        after: None,
                        timeout: Duration::from_millis(500)
                    },
                    "say \"hi\"".into()
                ),
                Step::Expect(Find {
                    text: "done".into(),
                    after: Some("見出し".into()),
                    timeout: Duration::from_secs(20)
                }),
                Step::ExpectAbsent(Find {
                    text: "x".into(),
                    after: None,
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::ExpectMenuBar(Find {
                    text: "取り消す".into(),
                    after: Some("編集".into()),
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::ExpectReadingOrder(Find {
                    text: "この会話".into(),
                    after: None,
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::ExpectScrolled(
                    Find {
                        text: "この会話".into(),
                        after: Some("見出し".into()),
                        timeout: DEFAULT_TIMEOUT
                    },
                    0.5,
                    1.0
                ),
                Step::ExpectCount(
                    Find {
                        text: "回答".into(),
                        after: None,
                        timeout: DEFAULT_TIMEOUT
                    },
                    2
                ),
                Step::ExpectEnabled(
                    Find {
                        text: "送信".into(),
                        after: None,
                        timeout: Duration::from_secs(2)
                    },
                    false
                ),
                Step::ExpectHeight(
                    Find {
                        text: "入力".into(),
                        after: None,
                        timeout: DEFAULT_TIMEOUT
                    },
                    60.0,
                    true
                ),
                Step::Scroll(
                    Find {
                        text: "会話".into(),
                        after: None,
                        timeout: DEFAULT_TIMEOUT
                    },
                    -3,
                    Modifiers::default()
                ),
                Step::Scroll(
                    Find {
                        text: "画像".into(),
                        after: Some("見出し".into()),
                        timeout: DEFAULT_TIMEOUT
                    },
                    2,
                    Modifiers {
                        command: true,
                        ..Modifiers::default()
                    }
                ),
                Step::ExpectApart(
                    Find {
                        text: "移動:*".into(),
                        after: None,
                        timeout: Duration::from_millis(500)
                    },
                    "message.*".into()
                ),
                Step::Window(660.0, 700.0),
                Step::ExpectInside(Find {
                    text: "送信".into(),
                    after: None,
                    timeout: Duration::from_millis(1000)
                }),
                Step::Select(
                    Find {
                        text: "回答".into(),
                        after: None,
                        timeout: DEFAULT_TIMEOUT
                    },
                    "Needle".into()
                ),
                Step::Menu(Find {
                    text: "回答".into(),
                    after: Some("見出し".into()),
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::Hover(Find {
                    text: "回答".into(),
                    after: None,
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::Drag(
                    Find {
                        text: "nested".into(),
                        after: Some("見出し".into()),
                        timeout: DEFAULT_TIMEOUT
                    },
                    Find {
                        text: "AI に依頼する".into(),
                        after: None,
                        timeout: DEFAULT_TIMEOUT
                    }
                ),
                Step::Drop,
                Step::Choose("/tmp/a.txt".into()),
                Step::Choose("$AGENT_STATE_DIR/project".into()),
                Step::ExpectFile(
                    "$AGENT_STATE_DIR/saved.png".into(),
                    None,
                    Duration::from_millis(500)
                ),
                Step::ExpectFile("/tmp/b.png".into(), Some("a.txt:2".into()), DEFAULT_TIMEOUT),
                Step::ExpectClipboard("a b".into(), Duration::from_millis(500)),
                Step::Copy {
                    text: "a\nb".into(),
                    image: false
                },
                Step::Copy {
                    text: String::new(),
                    image: true
                },
                Step::Copy {
                    text: "説明".into(),
                    image: true
                },
                Step::Paste(Find {
                    text: "入力欄".into(),
                    after: Some("見出し".into()),
                    timeout: DEFAULT_TIMEOUT
                }),
                Step::Undo(Find {
                    text: "入力欄".into(),
                    after: None,
                    timeout: Duration::from_millis(500)
                }),
                Step::Control("/release-inputs".into()),
                Step::Activate,
                Step::DenyMicrophone,
                Step::Relaunch,
                Step::Wait(Duration::from_millis(250)),
                Step::Screenshot("after-send".into()),
                Step::Dump("tree_1".into()),
                Step::Window(1280.0, 800.0),
                Step::Capture("state_1".into()),
            ]
        );
        assert_eq!(lines[0].number, 2);
        assert_eq!(lines[3].source, "press \"新しい チャット\"");
    }

    #[test]
    fn compositions_mark_commit_or_unmark_an_element() {
        let steps: Vec<_> = parse("mark 入力 か\ncommit 入力 会話 after 見出し\nunmark 入力 500\n")
            .unwrap()
            .into_iter()
            .map(|line| line.step)
            .collect();
        let field = |after: Option<&str>, timeout| Find {
            text: "入力".into(),
            after: after.map(Into::into),
            timeout,
        };
        assert_eq!(
            steps,
            [
                Step::Compose(field(None, DEFAULT_TIMEOUT), Composition::Mark("か".into())),
                Step::Compose(
                    field(Some("見出し"), DEFAULT_TIMEOUT),
                    Composition::Commit("会話".into())
                ),
                Step::Compose(field(None, Duration::from_millis(500)), Composition::Unmark),
            ]
        );
    }

    #[test]
    fn keys_carry_modifiers_and_appkit_codes() {
        let Step::Key(key) = step(&["key".into(), "cmd+shift+n".into()]).unwrap() else {
            panic!("not a key");
        };
        assert_eq!((key.characters.as_str(), key.code), ("n", 45));
        assert_eq!(
            key.modifiers,
            Modifiers {
                command: true,
                shift: true,
                ..Modifiers::default()
            }
        );
        assert_eq!(
            step(&["click".into(), "cmd".into(), "Webサイトを開く".into()]).unwrap(),
            Step::Click(
                Modifiers {
                    command: true,
                    ..Modifiers::default()
                },
                Find {
                    text: "Webサイトを開く".into(),
                    after: None,
                    timeout: DEFAULT_TIMEOUT
                }
            )
        );
        assert_eq!(
            step(&["click".into(), "none".into(), "a".into(), "500".into()]).unwrap(),
            Step::Click(
                Modifiers::default(),
                Find {
                    text: "a".into(),
                    after: None,
                    timeout: std::time::Duration::from_millis(500)
                }
            )
        );
        assert!(step(&["click".into(), "hyper".into(), "a".into()]).is_err());
        for (name, characters, code) in [
            ("return", "\r", 36),
            ("escape", "\u{1b}", 53),
            ("a", "a", 0),
            (",", ",", 43),
            ("down", "\u{F701}", 125),
            ("f10", "\u{F70D}", 109),
        ] {
            let key = super::key(name).unwrap();
            assert_eq!((key.characters.as_str(), key.code), (characters, code));
        }
        assert!(super::key("cmd+").is_err());
        assert!(super::key("hyper+a").is_err());
        assert!(super::key("f13").is_err());
    }

    #[test]
    fn labels_match_whole_text_unless_starred() {
        for (text, label, expected) in [
            ("送信", "送信", true),
            (" 送信 ", "送信", true),
            ("送信中", "送信", false),
            ("3秒 作業しました", "*作業しました", true),
            ("3秒 作業しました", "3秒*", true),
            ("3秒 作業しました", "*秒 作*", true),
            ("3秒 作業しました", "*作業中*", false),
            ("作業", "*", true),
            ("x", "**", true),
        ] {
            assert_eq!(labels(text, label), expected, "{text:?} {label:?}");
        }
    }

    #[test]
    fn malformed_scenarios_name_the_line() {
        for (script, reason) in [
            ("", "the scenario has no steps"),
            ("press", "line 1: unknown step or wrong arguments: press"),
            ("tap x", "line 1: unknown step or wrong arguments: tap"),
            ("window 0 700", "line 1: \"0\" is not a size in points"),
            (
                "expect_scrolled この会話 0 2",
                "line 1: \"2\" is not a fraction from 0 to 1",
            ),
            (
                "window wide 700",
                "line 1: \"wide\" is not a size in points",
            ),
            ("expect \"open", "line 1: unclosed quote"),
            (
                "wait soon",
                "line 1: \"soon\" is not a number of milliseconds",
            ),
            ("expect a 1 2", "line 1: too many arguments"),
            (
                "press a after",
                "line 1: \"after\" is not a number of milliseconds",
            ),
            (
                "screenshot ../escape",
                "line 1: \"../escape\" must be letters, digits, '-' or '_' to name a file",
            ),
            (
                "choose a.txt",
                "line 1: unknown step or wrong arguments: choose",
            ),
            (
                "expect_file saved.png",
                "line 1: unknown step or wrong arguments: expect_file",
            ),
            (
                "expect_clipboard a b c",
                "line 1: unknown step or wrong arguments: expect_clipboard",
            ),
            ("copy", "line 1: unknown step or wrong arguments: copy"),
            (
                "copy_image a b",
                "line 1: unknown step or wrong arguments: copy_image",
            ),
            (
                "window 1280",
                "line 1: unknown step or wrong arguments: window",
            ),
            (
                "fixture release-inputs",
                "line 1: unknown step or wrong arguments: fixture",
            ),
            (
                "expect x\nfirst-launch",
                "line 2: the run is prepared only by leading lines",
            ),
            (
                "wait 1\nhost without-codex",
                "line 2: the run is prepared only by leading lines",
            ),
            (
                "host with-codex",
                "line 1: unknown step or wrong arguments: host",
            ),
        ] {
            assert_eq!(parse(script).unwrap_err(), reason, "{script}");
        }
    }
}
