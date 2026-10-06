//! The program a shell command line runs, read without executing it, and the
//! script a plain shell wrapper carries.
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandWrapper {
    Env,
    Sudo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramContext {
    Exec,
    Shell,
}

const MAX_COMMAND_SEGMENTS: i64 = 64;

const SHELL_PROGRAMS: &[&str] = &["sh", "bash", "zsh", "dash", "ash", "ksh", "fish"];
const WINDOWS_SHELL_PROGRAMS: &[&str] = &[
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
];
const SHELL_OPTIONS_WITH_VALUE: &[&str] = &["-o", "-O", "--rcfile", "--init-file"];
const SHELL_COMMAND_WRAPPERS: &[&str] = &["builtin", "command", "exec"];
const SHELL_PRECOMMAND_MODIFIERS: &[&str] = &["nocorrect", "noglob", "time"];
const POWERSHELL_SETUP_PROGRAMS: &[&str] = &["pop-location", "push-location", "set-location"];
const POWERSHELL_FLAGS: &[&str] = &["-mta", "-nologo", "-noninteractive", "-noprofile", "-sta"];
const POWERSHELL_OPTIONS_WITH_VALUE: &[&str] = &[
    "-configurationname",
    "-executionpolicy",
    "-inputformat",
    "-outputformat",
    "-version",
    "-windowstyle",
    "-workingdirectory",
];
const START_PROCESS_FLAGS: &[&str] = &[
    "-confirm",
    "-debug",
    "-loaduserprofile",
    "-nonewwindow",
    "-passthru",
    "-usenewenvironment",
    "-verbose",
    "-wait",
    "-whatif",
];
const START_PROCESS_OPTIONS_WITH_VALUE: &[&str] = &[
    "-argumentlist",
    "-credential",
    "-environment",
    "-erroraction",
    "-errorvariable",
    "-informationaction",
    "-informationvariable",
    "-outbuffer",
    "-outvariable",
    "-pipelinevariable",
    "-progressaction",
    "-redirectstandarderror",
    "-redirectstandardinput",
    "-redirectstandardoutput",
    "-verb",
    "-warningaction",
    "-warningvariable",
    "-windowstyle",
    "-workingdirectory",
];
const SKIPPABLE_SUDO_PROBES: &[&str] = &["[", "[[", "test", "true"];
const NON_PROGRAM_PREFIX_CHARACTERS: &str = "<>(){}[];|&$`#!%@:";
const NON_PROGRAM_SUFFIX_CHARACTERS: &str = "){]}`";

// Shell syntax and shell-local control flow, not a useful executable name.
// Falling back to "command" is less misleading than "Ran if" or "Ran [".
const NON_DESCRIPTIVE_SHELL_PROGRAMS: &[&str] = &[
    "!",
    "#",
    ".",
    ":",
    "[",
    "[[",
    "alias",
    "and",
    "autoload",
    "begin",
    "bg",
    "bind",
    "bindkey",
    "break",
    "builtin",
    "caller",
    "case",
    "catch",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "continue",
    "coproc",
    "declare",
    "dirs",
    "disown",
    "do",
    "done",
    "elif",
    "else",
    "enable",
    "end",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "fi",
    "finally",
    "for",
    "foreach",
    "function",
    "getopts",
    "history",
    "if",
    "in",
    "jobs",
    "let",
    "local",
    "logout",
    "mapfile",
    "nocorrect",
    "noglob",
    "not",
    "or",
    "popd",
    "pushd",
    "read",
    "readarray",
    "readonly",
    "repeat",
    "return",
    "select",
    "set",
    "setopt",
    "shift",
    "shopt",
    "source",
    "switch",
    "suspend",
    "test",
    "then",
    "time",
    "times",
    "trap",
    "try",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "until",
    "unset",
    "unsetopt",
    "wait",
    "while",
];

// Unlike setup builtins, these can make later segments part of control flow or
// unreachable, so a later program must not label the command.
const TERMINAL_SHELL_PROGRAMS: &[&str] = &[
    "and", "begin", "break", "case", "catch", "continue", "coproc", "do", "done", "elif", "else",
    "end", "esac", "eval", "exec", "exit", "false", "fi", "finally", "for", "foreach", "function",
    "if", "in", "not", "or", "repeat", "return", "select", "switch", "then", "try", "until",
    "while",
];

const ENV_OPTIONS_WITH_VALUE: &[&str] = &["-C", "--chdir", "-S", "--split-string", "-u", "--unset"];
const SUDO_OPTIONS_WITH_VALUE: &[&str] = &[
    "-C",
    "--close-from",
    "-D",
    "--chdir",
    "-g",
    "--group",
    "-u",
    "--user",
];
const ENV_FLAGS: &[&str] = &[
    "-0",
    "--null",
    "-i",
    "--ignore-environment",
    "--debug",
    "-v",
];
const SUDO_FLAGS: &[&str] = &[
    "-A",
    "--askpass",
    "-b",
    "--background",
    "-E",
    "-H",
    "-i",
    "-n",
    "-S",
];

impl CommandWrapper {
    fn options_with_value(self) -> &'static [&'static str] {
        match self {
            Self::Env => ENV_OPTIONS_WITH_VALUE,
            Self::Sudo => SUDO_OPTIONS_WITH_VALUE,
        }
    }
    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Env => ENV_FLAGS,
            Self::Sudo => SUDO_FLAGS,
        }
    }
}

/// JavaScript `\s`: Unicode white space plus U+FEFF, without U+0085.
pub(crate) fn js_space(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

/// JavaScript `String.prototype.trim`.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(js_space)
}

fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(js_space)
}

/// Builds a regex whose `\s` and `\S` mean the JavaScript classes.
fn js_regex(pattern: &str) -> Regex {
    const SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";
    let pattern = pattern
        .replace(r"\S", &format!("[^{SPACE}]"))
        .replace(r"\s", &format!("[{SPACE}]"));
    Regex::new(&pattern).expect("pattern compiles")
}

macro_rules! js_regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| js_regex($pattern));
    };
}

/// JavaScript `.` without the `s` flag.
const DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

js_regex!(SHELL_COMMAND_OPTION, r"^-[a-zA-Z]*c[a-zA-Z]*$");
js_regex!(WINDOWS_PATH_PREFIX, r"^(?:[A-Za-z]:|\.{1,2})(?:\\[^\s]*)?$");
js_regex!(PROCESS_SUBSTITUTION, r"^[<>]\(");
static REDIRECTION: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(&format!(
        r"^(?:(?:(?:[0-9]+|\*|\{{[A-Za-z_][A-Za-z0-9_]*\}})?(?:<<<|<<-|<<|<>|>>|>\||<&|>&|<|>))|&>>|&>)({DOT}*)$"
    ))
});
js_regex!(SCRIPT_OPTIONS, r"^-[adkpqr]+$");
js_regex!(ARCH_OPTION, r"^-(?:arm64|arm64e|i386|x86_64)$");
js_regex!(
    TIMEOUT_DURATION,
    r"^(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)[smhd]?$"
);
js_regex!(
    POWERSHELL_ASSIGNMENT,
    r"(?i)^\s*\$(?:(env|global|local|script):)?[A-Za-z_][A-Za-z0-9_]*\s*=\s*([\s\S]*)$"
);
js_regex!(POWERSHELL_COLLECTION, r"(?i)^(?:\[ordered\]\s*)?@\s*[{(]");
js_regex!(
    POWERSHELL_DIRECT_COMMAND,
    r"^(?:@?\(\s*)?([A-Za-z][A-Za-z0-9_.-]*)(?-u:\b)"
);
js_regex!(LITERAL_ASSIGNMENT, r"(?s)^([A-Za-z_][A-Za-z0-9_]*)=(.*)$");
js_regex!(
    ALIAS_REFERENCE,
    r"^\$(?:([A-Za-z_][A-Za-z0-9_]*)|\{([A-Za-z_][A-Za-z0-9_]*)(?:\[@\])?\})$"
);
js_regex!(ASSIGNMENT_WORD, r"^[A-Za-z_][A-Za-z0-9_]*\+?=");
js_regex!(EXEC_CLEAR_OR_LOGIN, r"^-[cl]+$");
js_regex!(
    CONTROL_BLOCK,
    r"(?i)^(?:catch|finally|for|foreach|function|if|param|switch|try|while)\s*[{(]"
);
js_regex!(
    FUNCTION_DEFINITION,
    r"^[A-Za-z_][A-Za-z0-9_]*\s*\(\s*\)\s*\{"
);
js_regex!(POWERSHELL_HERE_STRING, r#"^@["'](?:\r?\n)"#);
js_regex!(
    WINDOWS_PROGRAM_PATH,
    r"(?i)^\s*((?:\.{1,2}|%[A-Za-z_][A-Za-z0-9_]*%|\$env:[A-Za-z_][A-Za-z0-9_]*)\\\S+)"
);
js_regex!(FUNCTION_OPENING, r"^[A-Za-z_][A-Za-z0-9_]*\(\)\{$");

fn has(set: &[&str], value: &str) -> bool {
    set.contains(&value)
}

fn line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The text after a two-character prefix has at least one character `.` matches.
fn continues_after_prefix(token: &str, prefix_len: usize) -> bool {
    token
        .chars()
        .nth(prefix_len)
        .is_some_and(|c| !line_terminator(c))
}

/// The last path segment, split on either slash.
fn last_path_segment(value: &str) -> &str {
    value.rsplit(['\\', '/']).next().unwrap_or("")
}

fn without_exe_suffix(value: &str) -> &str {
    let split = value.len().wrapping_sub(4);
    if value.len() >= 4
        && value.is_char_boundary(split)
        && value[split..].eq_ignore_ascii_case(".exe")
    {
        &value[..split]
    } else {
        value
    }
}

fn starts_with_any(value: &str, characters: &str) -> bool {
    value.chars().next().is_some_and(|c| characters.contains(c))
}

fn ends_with_any(value: &str, characters: &str) -> bool {
    value.chars().last().is_some_and(|c| characters.contains(c))
}

fn shell_command_argument_index(tokens: &[String], start: usize) -> Option<usize> {
    let mut index = start;
    while index < tokens.len() {
        let option = tokens[index].as_str();
        if option == "--" || !option.starts_with('-') {
            return None;
        }
        if has(SHELL_OPTIONS_WITH_VALUE, option) {
            index += 2;
            continue;
        }
        if option == "--command" || SHELL_COMMAND_OPTION.is_match(option) {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn tokenize_shell_command(command: &str) -> Option<Vec<String>> {
    let input: Vec<char> = js_trim(command).chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let mut in_backticks = false;
    let mut substitution_depth = 0u32;
    let mut parameter_expansion_depth = 0u32;
    let mut token_started = false;

    let mut index = 0;
    while index < input.len() {
        let i = index;
        index += 1;
        let character = input[i];
        let next = input.get(i + 1).copied();
        if escaping {
            current.push(character);
            escaping = false;
            token_started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            let windows_path = quote.is_none() && WINDOWS_PATH_PREFIX.is_match(&current);
            if (quote == Some('"') || windows_path)
                && next.is_some_and(|next| !matches!(next, '"' | '\\' | '$' | '`' | '\n'))
            {
                current.push(character);
                token_started = true;
                continue;
            }
            escaping = true;
            token_started = true;
            continue;
        }
        if in_backticks {
            current.push(character);
            if character == '`' {
                in_backticks = false;
            }
            token_started = true;
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else {
                current.push(character);
            }
            token_started = true;
            continue;
        }
        if character == '`' {
            current.push(character);
            in_backticks = true;
            token_started = true;
            continue;
        }
        if character == '$' && next == Some('{') {
            current.push_str("${");
            parameter_expansion_depth += 1;
            token_started = true;
            index += 1;
            continue;
        }
        if character == '{' && parameter_expansion_depth > 0 {
            current.push(character);
            parameter_expansion_depth += 1;
            token_started = true;
            continue;
        }
        if character == '}' && parameter_expansion_depth > 0 {
            current.push(character);
            parameter_expansion_depth -= 1;
            token_started = true;
            continue;
        }
        if character == '$' && next == Some('(') {
            current.push_str("$(");
            substitution_depth += 1;
            token_started = true;
            index += 1;
            continue;
        }
        if character == '(' {
            current.push(character);
            substitution_depth += 1;
            token_started = true;
            continue;
        }
        if character == ')' && substitution_depth > 0 {
            current.push(character);
            substitution_depth -= 1;
            token_started = true;
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            token_started = true;
            continue;
        }
        if js_space(character) {
            if substitution_depth > 0 || parameter_expansion_depth > 0 {
                current.push(character);
                token_started = true;
                continue;
            }
            if token_started {
                tokens.push(std::mem::take(&mut current));
                token_started = false;
            }
            continue;
        }
        current.push(character);
        token_started = true;
    }

    if quote.is_some()
        || escaping
        || in_backticks
        || substitution_depth > 0
        || parameter_expansion_depth > 0
    {
        return None;
    }
    if token_started {
        tokens.push(current);
    }
    Some(tokens)
}

struct ShellCommandSplit {
    first_command: String,
    /// Never empty.
    remaining_command: Option<String>,
    separator: Option<String>,
}

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
}

/// The character starting at byte `at`.
fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..)?.chars().next()
}

/// The command up to byte `end` without its comments, given as byte ranges.
fn command_without_shell_comments(
    command: &str,
    end: usize,
    comments: &[(usize, usize)],
) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    for &(start, comment_end) in comments {
        if start >= end {
            break;
        }
        result.push_str(&command[cursor.min(start)..start]);
        cursor = comment_end.min(end);
    }
    result.push_str(&command[cursor.min(end)..end]);
    result
}

/// The heredoc delimiter starting at byte `start` and the byte after it.
fn read_heredoc_delimiter(
    command: &str,
    start: usize,
    strip_tabs: bool,
) -> Option<(Heredoc, usize)> {
    let mut index = start;
    while matches!(char_at(command, index), Some(' ' | '\t')) {
        index += 1;
    }
    let mut delimiter = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;
    while let Some(character) = char_at(command, index) {
        if escaping {
            delimiter.push(character);
            escaping = false;
        } else if character == '\\' && quote != Some('\'') {
            escaping = true;
        } else if let Some(open) = quote {
            if character == open {
                quote = None;
            } else {
                delimiter.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if js_space(character) || ";&|<>()".contains(character) {
            break;
        } else {
            delimiter.push(character);
        }
        index += character.len_utf8();
    }
    if delimiter.is_empty() || quote.is_some() || escaping {
        return None;
    }
    Some((
        Heredoc {
            delimiter,
            strip_tabs,
        },
        index,
    ))
}

fn command_after_heredocs(command: &str, start: usize, heredocs: &[Heredoc]) -> Option<String> {
    let length = command.len();
    let mut cursor = start;
    for heredoc in heredocs {
        let mut found_delimiter = false;
        while cursor <= length {
            let newline = command[cursor..].find('\n').map(|offset| cursor + offset);
            let line_end = newline.unwrap_or(length);
            let line = &command[cursor..line_end];
            let line = line.strip_suffix('\r').unwrap_or(line);
            let comparable = if heredoc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            cursor = newline.map_or(length, |newline| newline + 1);
            if comparable == heredoc.delimiter {
                found_delimiter = true;
                break;
            }
            if newline.is_none() {
                break;
            }
        }
        if !found_delimiter {
            return None;
        }
    }
    let rest = js_trim(&command[cursor.min(length)..]);
    (!rest.is_empty()).then(|| rest.to_owned())
}

fn split_first_shell_command(command: &str) -> ShellCommandSplit {
    let at = |index: usize| char_at(command, index);
    let mut quote: Option<char> = None;
    let mut here_string_quote: Option<char> = None;
    let mut escaping = false;
    let mut in_backticks = false;
    let mut in_comment = false;
    let mut substitution_depth = 0u32;
    let mut parameter_expansion_depth = 0u32;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    let mut comments: Vec<(usize, usize)> = Vec::new();
    let mut comment_start = 0;
    // (byte, length) of the first separator on a line that opened heredocs.
    let mut separator_before_heredocs: Option<(usize, usize)> = None;
    let mut previous: Option<char> = None;

    // Byte offsets: every character compared or skipped ahead is ASCII.
    let mut index = 0;
    while let Some(character) = at(index) {
        let i = index;
        let before = previous;
        index += character.len_utf8();
        previous = Some(character);
        if let Some(open) = here_string_quote {
            if character == open && at(i + 1) == Some('@') && (i == 0 || before == Some('\n')) {
                here_string_quote = None;
                index += 1;
                previous = Some('@');
            }
            continue;
        }
        if in_comment {
            if character != '\n' {
                continue;
            }
            in_comment = false;
            comments.push((comment_start, i));
        }
        if escaping {
            escaping = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaping = true;
            continue;
        }
        if in_backticks {
            if character == '`' {
                in_backticks = false;
            }
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
            continue;
        }
        if character == '@'
            && matches!(at(i + 1), Some('"' | '\''))
            && (at(i + 2) == Some('\n') || (at(i + 2) == Some('\r') && at(i + 3) == Some('\n')))
        {
            here_string_quote = at(i + 1);
            previous = here_string_quote;
            index += 1;
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            continue;
        }
        if character == '`' {
            in_backticks = true;
            continue;
        }
        if character == '#'
            && before.is_none_or(|before| js_space(before) || ";&|(".contains(before))
        {
            in_comment = true;
            comment_start = i;
            continue;
        }
        if character == '$' && at(i + 1) == Some('{') {
            parameter_expansion_depth += 1;
            index += 1;
            previous = Some('{');
            continue;
        }
        if character == '{' && parameter_expansion_depth > 0 {
            parameter_expansion_depth += 1;
            continue;
        }
        if character == '}' && parameter_expansion_depth > 0 {
            parameter_expansion_depth -= 1;
            continue;
        }
        if character == '(' {
            substitution_depth += 1;
            continue;
        }
        if character == ')' && substitution_depth > 0 {
            substitution_depth -= 1;
            continue;
        }
        if substitution_depth > 0 || parameter_expansion_depth > 0 {
            continue;
        }

        if character == '<' && at(i + 1) == Some('<') && at(i + 2) != Some('<') {
            let strip_tabs = at(i + 2) == Some('-');
            let Some((heredoc, end)) =
                read_heredoc_delimiter(command, i + if strip_tabs { 3 } else { 2 }, strip_tabs)
            else {
                return ShellCommandSplit {
                    first_command: js_trim(command).to_owned(),
                    remaining_command: None,
                    separator: None,
                };
            };
            heredocs.push(heredoc);
            previous = command[..end].chars().next_back();
            index = end;
            continue;
        }

        let double_operator = (character == '&' && at(i + 1) == Some('&'))
            || (character == '|' && matches!(at(i + 1), Some('|' | '&')));
        let redirection_ampersand =
            character == '&' && (matches!(before, Some('>' | '<')) || at(i + 1) == Some('>'));
        if (!double_operator && !";&|\n".contains(character)) || redirection_ampersand {
            continue;
        }

        if character == '\n' && !heredocs.is_empty() {
            let separator = separator_before_heredocs;
            let first_command = js_trim_start(&command_without_shell_comments(
                command,
                separator.map_or(i, |(start, _)| start),
                &comments,
            ))
            .to_owned();
            let command_before_heredocs = separator
                .map(|(start, length)| js_trim(&command[(start + length).min(i)..i]).to_owned())
                .unwrap_or_default();
            let command_following_heredocs = command_after_heredocs(command, i + 1, &heredocs);
            let remaining_command = [Some(command_before_heredocs), command_following_heredocs]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            return ShellCommandSplit {
                first_command,
                remaining_command: (!remaining_command.is_empty()).then_some(remaining_command),
                separator: Some(separator.map_or_else(
                    || "\n".to_owned(),
                    |(start, length)| command[start..start + length].to_owned(),
                )),
            };
        }
        if !heredocs.is_empty() {
            separator_before_heredocs.get_or_insert((i, if double_operator { 2 } else { 1 }));
            if double_operator {
                index += 1;
                previous = at(i + 1);
            }
            continue;
        }

        let first_command =
            js_trim_start(&command_without_shell_comments(command, i, &comments)).to_owned();
        let mut next_command_index = i + if double_operator { 2 } else { 1 };
        while let Some(space) = at(next_command_index).filter(|&c| js_space(c)) {
            next_command_index += space.len_utf8();
        }
        let next_command = js_trim(&command[next_command_index.min(command.len())..]);
        return ShellCommandSplit {
            first_command,
            remaining_command: (!next_command.is_empty()).then(|| next_command.to_owned()),
            separator: Some(if double_operator {
                command[i..i + 2].to_owned()
            } else {
                character.to_string()
            }),
        };
    }

    if in_comment {
        comments.push((comment_start, command.len()));
    }
    ShellCommandSplit {
        first_command: js_trim(&command_without_shell_comments(
            command,
            command.len(),
            &comments,
        ))
        .to_owned(),
        remaining_command: None,
        separator: None,
    }
}

fn command_without_leading_shell_comments(command: &str) -> Option<String> {
    let mut remaining = js_trim_start(command);
    while remaining.starts_with('#') {
        let newline = remaining.find('\n')?;
        remaining = js_trim_start(&remaining[newline + 1..]);
    }
    (!remaining.is_empty()).then(|| remaining.to_owned())
}

fn without_shell_line_continuations(command: &str) -> String {
    let c: Vec<char> = command.chars().collect();
    let mut normalized = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let mut index = 0;
    while index < c.len() {
        let i = index;
        index += 1;
        let character = c[i];
        if escaping {
            normalized.push(character);
            escaping = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            if c.get(i + 1) == Some(&'\n') {
                index += 1;
                continue;
            }
            if c.get(i + 1) == Some(&'\r') && c.get(i + 2) == Some(&'\n') {
                index += 2;
                continue;
            }
            normalized.push(character);
            escaping = true;
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        }
        normalized.push(character);
    }
    normalized
}

/// The token index after a redirection at `index`; past the end when its
/// target is missing.
fn index_after_shell_redirection(tokens: &[String], index: usize) -> Option<usize> {
    let token = tokens.get(index)?;
    if token.is_empty() || PROCESS_SUBSTITUTION.is_match(token) {
        return None;
    }
    let captures = REDIRECTION.captures(token)?;
    if !captures[1].is_empty() {
        return Some(index + 1);
    }
    Some(if tokens.get(index + 1).is_none() {
        tokens.len() + 1
    } else {
        index + 2
    })
}

fn serialize_shell_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| format!("'{}'", token.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn transparent_wrapper_command_index(
    wrapper: &str,
    tokens: &[String],
    index: usize,
) -> Option<usize> {
    let at = |offset: usize| tokens.get(index + offset).map(String::as_str);
    match wrapper {
        "bundle" => (at(1) == Some("exec") && at(2).is_some()).then_some(index + 2),
        "nohup" => {
            let mut target = index + 1;
            if tokens.get(target).map(String::as_str) == Some("--") {
                target += 1;
            }
            tokens
                .get(target)
                .is_some_and(|token| !token.is_empty() && !token.starts_with('-'))
                .then_some(target)
        }
        // BSD `script` takes an output file before the optional command.
        // Requiring an option and both operands avoids guessing about `script file`.
        "script" => {
            (SCRIPT_OPTIONS.is_match(at(1).unwrap_or("")) && at(3).is_some()).then_some(index + 3)
        }
        "arch" => {
            if ARCH_OPTION.is_match(at(1).unwrap_or("")) {
                return at(2).is_some().then_some(index + 2);
            }
            (at(1) == Some("-arch") && at(3).is_some()).then_some(index + 3)
        }
        "timeout" | "gtimeout" => {
            (TIMEOUT_DURATION.is_match(at(1).unwrap_or("")) && at(2).is_some()).then_some(index + 2)
        }
        _ => None,
    }
}

/// `scheme:` not followed by a slash, as in a URL or a drive-less URI.
fn has_uri_scheme(value: &str) -> bool {
    if !value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    let rest = &value[1..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
        .unwrap_or(rest.len());
    let after = &rest[end..];
    after.starts_with(':') && !matches!(after[1..].chars().next(), Some('\\' | '/'))
}

fn static_program_name(value: &str) -> Option<String> {
    let trimmed = js_trim(value);
    if trimmed.is_empty() || has_uri_scheme(trimmed) {
        return None;
    }
    let program = last_path_segment(trimmed);
    if program.is_empty()
        || (program.chars().any(js_space) && !trimmed.contains(['\\', '/']))
        || starts_with_any(program, NON_PROGRAM_PREFIX_CHARACTERS)
        || ends_with_any(program, NON_PROGRAM_SUFFIX_CHARACTERS)
    {
        return None;
    }
    Some(program.to_owned())
}

fn leading_powershell_literal(command: &str) -> Option<String> {
    let input: Vec<char> = js_trim_start(command).chars().collect();
    let quote = match input.first() {
        Some(&quote @ ('"' | '\'')) => quote,
        _ => {
            let word: String = input.iter().take_while(|&&c| !js_space(c)).collect();
            return static_program_name(&word);
        }
    };
    let mut value = String::new();
    let mut index = 1;
    while index < input.len() {
        let character = input[index];
        if character == '`'
            && let Some(&next) = input.get(index + 1)
        {
            value.push(next);
            index += 2;
            continue;
        }
        if character == quote {
            if quote == '\'' && input.get(index + 1) == Some(&'\'') {
                value.push('\'');
                index += 2;
                continue;
            }
            return static_program_name(&value);
        }
        value.push(character);
        index += 1;
    }
    None
}

/// `None` when the command is not a call operator; `Some(None)` when it calls
/// something that is not a literal program.
fn powershell_call_operator_program_name(command: &str) -> Option<Option<String>> {
    let rest = js_trim_start(command).strip_prefix('&')?;
    if !rest.starts_with(js_space) {
        return None;
    }
    Some(leading_powershell_literal(rest))
}

/// `None` when the command is not a PowerShell assignment.
fn powershell_assignment_program_name(
    command: &str,
    depth: u32,
    remaining_command: Option<&str>,
    segments_remaining: i64,
) -> Option<Option<String>> {
    let assignment = POWERSHELL_ASSIGNMENT.captures(command)?;
    let next_segment = || {
        remaining_command.and_then(|remaining| {
            command_program_name_internal(
                remaining,
                depth,
                ProgramContext::Shell,
                segments_remaining - 1,
            )
        })
    };

    // Environment assignments are setup. Their right-hand side is a value, not
    // a command, so prefer the next top-level segment when one exists.
    if assignment
        .get(1)
        .is_some_and(|scope| scope.as_str().to_lowercase() == "env")
    {
        return Some(next_segment());
    }

    let value = js_trim(&assignment[2]);
    // The segment splitter does not balance PowerShell arrays or hashtables;
    // a key after an internal semicolon is not a command.
    if POWERSHELL_COLLECTION.is_match(value) {
        return Some(None);
    }
    if let Some(called) = powershell_call_operator_program_name(value) {
        return Some(called);
    }
    if let Some(direct) = POWERSHELL_DIRECT_COMMAND
        .captures(value)
        .map(|captures| captures[1].to_owned())
        && !has(NON_DESCRIPTIVE_SHELL_PROGRAMS, &direct.to_lowercase())
    {
        let parsed = command_program_name_internal(
            value,
            depth + 1,
            ProgramContext::Shell,
            segments_remaining,
        );
        return Some(parsed.or(Some(direct)));
    }
    Some(next_segment())
}

/// `None` when the launcher carries no payload option.
fn windows_shell_payload_program_name(
    shell: &str,
    tokens: &[String],
    start: usize,
    depth: u32,
    remaining_command: Option<&str>,
    separator: Option<&str>,
    segments_remaining: i64,
) -> Option<Option<String>> {
    let parse_payload = |payload: Option<&String>| {
        let payload = payload.filter(|payload| !payload.is_empty())?;
        let command = match (remaining_command, separator) {
            (Some(remaining), Some(separator)) => format!("{payload} {separator} {remaining}"),
            _ => payload.clone(),
        };
        command_program_name_internal(
            &command,
            depth + 1,
            ProgramContext::Shell,
            segments_remaining,
        )
    };

    if shell == "cmd" || shell == "cmd.exe" {
        for index in start..tokens.len() {
            let option = tokens[index].to_lowercase();
            if option != "/c" && option != "/k" {
                continue;
            }
            return Some(parse_payload(tokens.get(index + 1)));
        }
        return None;
    }

    let mut index = start;
    while index < tokens.len() {
        let option = tokens[index].to_lowercase();
        if option == "-command" || option == "-c" {
            return Some(parse_payload(tokens.get(index + 1)));
        }
        if option == "-file" || option == "-f" {
            return Some(static_program_name(
                tokens.get(index + 1).map_or("", String::as_str),
            ));
        }
        if option == "-encodedcommand" || option == "-enc" || option == "-e" {
            return Some(None);
        }
        if has(POWERSHELL_OPTIONS_WITH_VALUE, &option) {
            index += 2;
            continue;
        }
        if has(POWERSHELL_FLAGS, &option) {
            index += 1;
            continue;
        }
        if !option.starts_with('-') {
            return Some(static_program_name(&tokens[index]));
        }
        index += 1;
    }
    None
}

fn start_process_program_name(tokens: &[String], start: usize) -> Option<String> {
    let mut index = start;
    while index < tokens.len() {
        let token = &tokens[index];
        let option = token.to_lowercase();
        if option == "-filepath" {
            return static_program_name(tokens.get(index + 1).map_or("", String::as_str));
        }
        if has(START_PROCESS_FLAGS, &option) {
            index += 1;
            continue;
        }
        if has(START_PROCESS_OPTIONS_WITH_VALUE, &option) {
            tokens.get(index + 1)?;
            index += 2;
            continue;
        }
        if token.starts_with('-') {
            return None;
        }
        return static_program_name(token);
    }
    None
}

/// A `NAME=value` word: the name and the literal program it names, if any.
fn literal_assignment_program(token: &str) -> Option<(String, Option<String>)> {
    let assignment = LITERAL_ASSIGNMENT.captures(token)?;
    let name = assignment[1].to_owned();
    let mut value = js_trim(&assignment[2]).to_owned();
    if value.is_empty() || value.contains(['$', '`']) {
        return Some((name, None));
    }
    if value.starts_with('(') && value.ends_with(')') {
        value = tokenize_shell_command(&value[1..value.len() - 1])
            .and_then(|tokens| tokens.into_iter().next())
            .unwrap_or_default();
    } else if value.chars().any(js_space) && !value.contains(['\\', '/']) {
        return Some((name, None));
    }
    let program = static_program_name(&value);
    Some((name, program))
}

fn referenced_command_alias(token: &str) -> Option<String> {
    let reference = ALIAS_REFERENCE.captures(token)?;
    reference
        .get(1)
        .or_else(|| reference.get(2))
        .map(|name| name.as_str().to_owned())
}

/// Recovers only literal aliases declared by an earlier top-level segment, such
/// as `SSH=(ssh ...)` or `TOOL=/path/to/tool`, without evaluating expansions or
/// modelling general shell state.
fn literal_command_alias_program_name(command: &str) -> Option<String> {
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut remaining_command = Some(command.to_owned());
    let mut control_flow_depth = 0u32;

    for _ in 0..64 {
        let Some(remaining) = remaining_command.take().filter(|r| !r.is_empty()) else {
            break;
        };
        let command_without_comments = command_without_leading_shell_comments(&remaining)?;
        let split = split_first_shell_command(&command_without_comments);
        let tokens =
            tokenize_shell_command(&without_shell_line_continuations(&split.first_command))?;

        let leading = tokens.first().map(String::as_str);
        let mut command_index = usize::from(matches!(leading, Some("do" | "then")));
        while command_index < tokens.len() {
            if let Some(after) = index_after_shell_redirection(&tokens, command_index)
                && after <= tokens.len()
            {
                command_index = after;
                continue;
            }
            if ASSIGNMENT_WORD.is_match(&tokens[command_index]) {
                command_index += 1;
                continue;
            }
            break;
        }

        if let Some(alias) =
            referenced_command_alias(tokens.get(command_index).map_or("", String::as_str))
            && let Some(program) = aliases.get(&alias)
        {
            return Some(program.clone());
        }

        if matches!(leading, Some("fi" | "done" | "esac")) {
            control_flow_depth = control_flow_depth.saturating_sub(1);
        }
        if matches!(
            leading,
            Some("if" | "for" | "while" | "until" | "select" | "case")
        ) {
            control_flow_depth += 1;
        }

        if control_flow_depth == 0 && leading == Some("unset") {
            for name in &tokens[1..] {
                aliases.remove(name);
            }
        }

        let assignment_start = usize::from(leading == Some("export"));
        let assignments: Vec<_> = tokens[assignment_start..]
            .iter()
            .map(|token| literal_assignment_program(token))
            .collect();
        if control_flow_depth == 0
            && !assignments.is_empty()
            && assignments.iter().all(Option::is_some)
        {
            for (name, program) in assignments.into_iter().flatten() {
                match program {
                    Some(program) => {
                        aliases.insert(name, program);
                    }
                    None => {
                        aliases.remove(&name);
                    }
                }
            }
        }

        remaining_command = split.remaining_command;
    }
    None
}

fn wrapped_shell_command_program_name(
    wrapper: &str,
    tokens: &[String],
    start: usize,
    depth: u32,
    remaining_command: Option<&str>,
    segments_remaining: i64,
) -> Option<String> {
    let mut index = start;
    match wrapper {
        "command" => {
            while index < tokens.len() {
                let option = tokens[index].as_str();
                if option == "--" {
                    index += 1;
                    break;
                }
                if !option.starts_with('-') || option == "-" {
                    break;
                }
                if option != "-p" {
                    return None;
                }
                index += 1;
            }
        }
        "builtin" => {
            if tokens.get(index).map(String::as_str) == Some("--") {
                index += 1;
            } else if tokens
                .get(index)
                .is_some_and(|token| token.starts_with('-'))
            {
                return None;
            }
        }
        "exec" => {
            while index < tokens.len() {
                let option = tokens[index].as_str();
                if option == "--" {
                    index += 1;
                    break;
                }
                if option == "-a" {
                    tokens.get(index + 1)?;
                    index += 2;
                    continue;
                }
                if (option.starts_with("-a") && continues_after_prefix(option, 2))
                    || EXEC_CLEAR_OR_LOGIN.is_match(option)
                {
                    index += 1;
                    continue;
                }
                if option.starts_with('-') && option != "-" {
                    return None;
                }
                break;
            }
        }
        _ => {}
    }

    let wrapped = &tokens[index.min(tokens.len())..];
    if wrapped.is_empty() {
        return None;
    }
    let mut target_index = 0;
    while target_index < wrapped.len() {
        match index_after_shell_redirection(wrapped, target_index) {
            Some(after) if after <= wrapped.len() => target_index = after,
            _ => break,
        }
    }
    let target = wrapped
        .get(target_index)
        .map(String::as_str)
        .filter(|target| !target.is_empty());
    if target.is_some_and(|target| ASSIGNMENT_WORD.is_match(target)) {
        return None;
    }

    let wrapped_program = command_program_name_internal(
        &serialize_shell_tokens(wrapped),
        depth + 1,
        if wrapper == "exec" {
            ProgramContext::Exec
        } else {
            ProgramContext::Shell
        },
        segments_remaining,
    );
    if wrapped_program.is_some() {
        return wrapped_program;
    }

    if wrapper != "exec"
        && let Some(target) = target
        && target == target.to_lowercase()
        && has(NON_DESCRIPTIVE_SHELL_PROGRAMS, target)
        && !has(TERMINAL_SHELL_PROGRAMS, target)
        && let Some(remaining) = remaining_command
    {
        return command_program_name_internal(
            remaining,
            depth,
            ProgramContext::Shell,
            segments_remaining - 1,
        );
    }
    None
}

fn parse_command_program_name(
    command: &str,
    depth: u32,
    context: ProgramContext,
    segments_remaining: i64,
) -> Option<String> {
    if depth >= 8 || segments_remaining <= 0 {
        return None;
    }
    let command_without_comments = command_without_leading_shell_comments(command)?;
    if CONTROL_BLOCK.is_match(&command_without_comments)
        || FUNCTION_DEFINITION.is_match(&command_without_comments)
    {
        return None;
    }
    // `&&` and `||` inside `[[ ... ]]` are not top-level separators. Keep the
    // label conservative instead of scanning the test body.
    if command_without_comments.starts_with("[[") {
        return None;
    }
    let split = split_first_shell_command(&command_without_comments);
    let remaining = split.remaining_command.as_deref();
    let next_segment = |context: ProgramContext| {
        remaining.and_then(|remaining| {
            command_program_name_internal(remaining, depth, context, segments_remaining - 1)
        })
    };
    if POWERSHELL_HERE_STRING.is_match(js_trim_start(&split.first_command)) {
        return next_segment(ProgramContext::Shell);
    }
    if let Some(program) = powershell_assignment_program_name(
        &split.first_command,
        depth,
        remaining,
        segments_remaining,
    ) {
        return program;
    }
    if let Some(path) = WINDOWS_PROGRAM_PATH.captures(&split.first_command) {
        return static_program_name(&path[1]);
    }
    let tokens = tokenize_shell_command(&without_shell_line_continuations(&split.first_command))?;
    let first_character = js_trim_start(&split.first_command).chars().next();
    if tokens.len() == 1
        && matches!(first_character, Some('"' | '\''))
        && tokens[0].contains(|c: char| js_space(c) || matches!(c, '(' | ')' | '='))
        && !tokens[0].contains(['\\', '/'])
    {
        return next_segment(context);
    }

    let mut index = 0;
    let mut wrapper: Option<CommandWrapper> = None;
    let mut execution_context = context;
    let mut saw_assignment = false;
    let mut saw_redirection = false;

    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token.is_empty() {
            return None;
        }
        if let Some(after) = index_after_shell_redirection(&tokens, index) {
            if after > tokens.len() {
                return None;
            }
            saw_redirection = true;
            index = after;
            continue;
        }
        if ASSIGNMENT_WORD.is_match(token) {
            saw_assignment = true;
            index += 1;
            continue;
        }
        if execution_context == ProgramContext::Shell && token == ":" {
            return next_segment(execution_context);
        }
        if starts_with_any(token, NON_PROGRAM_PREFIX_CHARACTERS)
            && !(token.starts_with('$') && token.contains('/'))
        {
            return if execution_context == ProgramContext::Shell && token.starts_with('[') {
                next_segment(execution_context)
            } else {
                None
            };
        }
        let program = last_path_segment(token);
        let unqualified = token == program;
        if program == "env" || program == "sudo" {
            wrapper = Some(if program == "env" {
                CommandWrapper::Env
            } else {
                CommandWrapper::Sudo
            });
            execution_context = ProgramContext::Exec;
            index += 1;
            continue;
        }
        if wrapper.is_some() && token == "--" {
            wrapper = None;
            index += 1;
            continue;
        }
        if let Some(wrapper) = wrapper
            && token.starts_with('-')
        {
            if wrapper == CommandWrapper::Env && (token == "-S" || token == "--split-string") {
                return tokens
                    .get(index + 1)
                    .filter(|split| !split.is_empty())
                    .and_then(|split| {
                        command_program_name_internal(
                            split,
                            depth + 1,
                            execution_context,
                            segments_remaining,
                        )
                    });
            }
            if wrapper == CommandWrapper::Env
                && let Some(split) = token.strip_prefix("--split-string=")
            {
                return command_program_name_internal(
                    split,
                    depth + 1,
                    execution_context,
                    segments_remaining,
                );
            }
            if has(wrapper.options_with_value(), token) {
                tokens.get(index + 1)?;
                index += 2;
                continue;
            }
            if has(wrapper.flags(), token) {
                index += 1;
                continue;
            }
            if token.starts_with("--")
                && let Some(equals) = token.find('=').filter(|&equals| equals > 2)
            {
                if !has(wrapper.options_with_value(), &token[..equals]) {
                    return None;
                }
                index += 1;
                continue;
            }
            if token
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphabetic())
                && continues_after_prefix(token, 2)
                && !token.starts_with("--")
            {
                let options: Vec<char> = token[1..].chars().collect();
                let mut consumes_next_token = false;
                for (option_index, option) in options.iter().enumerate() {
                    let short_option = format!("-{option}");
                    if has(wrapper.options_with_value(), &short_option) {
                        consumes_next_token = option_index == options.len() - 1;
                        break;
                    }
                    if !has(wrapper.flags(), &short_option) {
                        return None;
                    }
                }
                if consumes_next_token {
                    tokens.get(index + 1)?;
                }
                index += if consumes_next_token { 2 } else { 1 };
                continue;
            }
            return None;
        }
        if !program.is_empty()
            && has(SHELL_PROGRAMS, &without_exe_suffix(program).to_lowercase())
            && let Some(script_index) = shell_command_argument_index(&tokens, index + 1)
        {
            return tokens
                .get(script_index)
                .filter(|script| !script.is_empty())
                .and_then(|script| {
                    command_program_name_internal(
                        script,
                        depth + 1,
                        ProgramContext::Shell,
                        segments_remaining,
                    )
                });
        }
        let lower_program = program.to_lowercase();
        if has(WINDOWS_SHELL_PROGRAMS, &lower_program)
            && let Some(payload) = windows_shell_payload_program_name(
                &lower_program,
                &tokens,
                index + 1,
                depth,
                remaining,
                split.separator.as_deref(),
                segments_remaining,
            )
        {
            return payload;
        }
        if lower_program == "start-process"
            && let Some(started) = start_process_program_name(&tokens, index + 1)
        {
            return Some(started);
        }
        if execution_context == ProgramContext::Shell
            && unqualified
            && has(SHELL_PRECOMMAND_MODIFIERS, program)
        {
            index += 1;
            if program == "time" && tokens.get(index).map(String::as_str) == Some("-p") {
                index += 1;
            }
            if tokens
                .get(index)
                .is_some_and(|token| token.starts_with('-'))
            {
                return None;
            }
            continue;
        }
        if unqualified
            && !program.is_empty()
            && let Some(target_index) = transparent_wrapper_command_index(program, &tokens, index)
            && let Some(wrapped) = command_program_name_internal(
                &serialize_shell_tokens(&tokens[target_index..]),
                depth + 1,
                ProgramContext::Exec,
                segments_remaining,
            )
        {
            return Some(wrapped);
        }
        if unqualified && has(SHELL_COMMAND_WRAPPERS, program) {
            return wrapped_shell_command_program_name(
                program,
                &tokens,
                index + 1,
                depth,
                remaining,
                segments_remaining,
            );
        }
        if (execution_context == ProgramContext::Shell
            || (wrapper == Some(CommandWrapper::Sudo) && has(SKIPPABLE_SUDO_PROBES, program)))
            && unqualified
            && has(NON_DESCRIPTIVE_SHELL_PROGRAMS, program)
            && (!has(TERMINAL_SHELL_PROGRAMS, program)
                || (program == "false" && split.separator.as_deref() != Some("&&")))
            && remaining.is_some()
        {
            return next_segment(ProgramContext::Shell);
        }
        if execution_context == ProgramContext::Shell
            && unqualified
            && has(POWERSHELL_SETUP_PROGRAMS, &lower_program)
            && remaining.is_some()
        {
            return next_segment(ProgramContext::Shell);
        }
        if program.is_empty()
            || (unqualified && has(NON_DESCRIPTIVE_SHELL_PROGRAMS, program))
            || starts_with_any(program, NON_PROGRAM_PREFIX_CHARACTERS)
            || ends_with_any(program, NON_PROGRAM_SUFFIX_CHARACTERS)
            || program.ends_with("()")
            || FUNCTION_OPENING.is_match(program)
        {
            return None;
        }
        return Some(program.to_owned());
    }

    if (saw_assignment || saw_redirection) && wrapper.is_none() {
        return next_segment(execution_context);
    }
    None
}

fn command_program_name_internal(
    command: &str,
    depth: u32,
    context: ProgramContext,
    segments_remaining: i64,
) -> Option<String> {
    if segments_remaining <= 0 {
        return None;
    }
    if let Some(called) = powershell_call_operator_program_name(command) {
        return called;
    }
    parse_command_program_name(command, depth, context, segments_remaining).or_else(|| {
        if context == ProgramContext::Shell {
            literal_command_alias_program_name(command)
        } else {
            None
        }
    })
}

/// The program a command line runs, or `None` when only shell syntax, setup
/// or an expansion would name it.
pub fn command_program_name(command: &str) -> Option<String> {
    command_program_name_internal(command, 0, ProgramContext::Shell, MAX_COMMAND_SEGMENTS)
}

/// Removes a plain shell `-c` wrapper for display; callers keep the original
/// for details.
pub fn command_display_text(command: &str) -> String {
    let trimmed = js_trim(command);
    if split_first_shell_command(trimmed)
        .remaining_command
        .is_some()
    {
        return trimmed.to_owned();
    }
    let Some(tokens) = tokenize_shell_command(trimmed) else {
        return trimmed.to_owned();
    };
    let program = tokens
        .first()
        .map(|token| without_exe_suffix(last_path_segment(token)));
    if !program.is_some_and(|program| !program.is_empty() && has(SHELL_PROGRAMS, program)) {
        return trimmed.to_owned();
    }
    // Positional arguments can affect the script; keep those invocations intact.
    match shell_command_argument_index(&tokens, 1) {
        Some(script_index) if script_index == tokens.len() - 1 => {
            let script = js_trim(&tokens[script_index]);
            if script.is_empty() {
                trimmed.to_owned()
            } else {
                script.to_owned()
            }
        }
        _ => trimmed.to_owned(),
    }
}

#[cfg(test)]
#[path = "command_label_tests.rs"]
mod tests;
