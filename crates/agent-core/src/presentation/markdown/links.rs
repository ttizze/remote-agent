//! Markdown link destinations and inline code that name local files, and how native
//! clients present links (external host, file chip with icon, or plain link).
use crate::js_text::{decode_uri_component, is_js_space, js_trim, utf16_len, utf16_skip};
use regex::Regex;
use std::sync::LazyLock;

pub fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'/' | b'\\'))
}

pub fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

pub fn is_windows_absolute_path(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value)
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

static RELATIVE_FILE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"^(?:[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*/)+[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*(?::[0-9]+){0,2}$",
    )
});
static RELATIVE_FILE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"^[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*\.[A-Za-z0-9_-]+(?::[0-9]+){0,2}$")
});
static POSITION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| regex(r":([0-9]+)(?::([0-9]+))?$"));
static POSITION_HASH: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^#[Ll]([0-9]+)(?:[Cc]([0-9]+))?$"));
static FILE_EXTENSION: LazyLock<Regex> = LazyLock::new(|| regex(r"\.[A-Za-z0-9_-]+$"));
// A final dot between digits marks a version or model id (`glm-5.3`,
// `Qwen2.5-Coder`), not an extension. `ls.1` and `libfoo.so.1` stay files.
static VERSION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| regex(r"[0-9]\.[0-9][^.]*$"));
static NUMERIC_DOTTED: LazyLock<Regex> = LazyLock::new(|| regex(r"^[0-9]+(?:\.[0-9]+)+$"));

// Standard OS and dev-container roots; deliberately excludes app-route-ish
// prefixes like /app/ or /chat/ so SPA routes never read as files.
const POSIX_FILE_ROOT_PREFIXES: [&str; 24] = [
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/",
    "/etc/",
    "/opt/",
    "/mnt/",
    "/Volumes/",
    "/private/",
    "/root/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/lib/",
    "/lib64/",
    "/srv/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/run/",
    "/boot/",
    "/media/",
    "/workspace/",
    "/workspaces/",
];
// `Name:digits` also matches `error:1`, `port:3000`, and `TODO:12`.
const EXTENSIONLESS_FILE_NAMES: [&str; 27] = [
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Procfile",
    "Brewfile",
    "Caddyfile",
    "Vagrantfile",
    "Jenkinsfile",
    "Podfile",
    "Fastfile",
    "BUILD",
    "WORKSPACE",
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTORS",
    "CHANGELOG",
    "README",
    "CODEOWNERS",
];
// These allowlists avoid classifying dotted directories such as `conf.d/`
// or filenames such as `Makefile.in:12` as hosts.
const GENERIC_HOSTNAME_TLDS: [&str; 25] = [
    "com", "net", "org", "io", "dev", "app", "ai", "co", "edu", "gov", "mil", "info", "biz", "xyz",
    "me", "tv", "cc", "gg", "chat", "cloud", "site", "online", "tech", "store", "link",
];
// Country codes also name file extensions. A :line suffix makes `.pl`
// and `.pt` files more likely than hostnames.
const COUNTRY_HOSTNAME_TLDS: [&str; 31] = [
    "uk", "de", "fr", "nl", "se", "no", "fi", "dk", "pl", "ch", "at", "be", "es", "it", "pt", "eu",
    "us", "ca", "au", "nz", "jp", "kr", "cn", "br", "ru", "mx", "ie", "cz", "tr", "sg", "hk",
];

fn has_position_suffix(path: &str) -> bool {
    POSITION_SUFFIX.is_match(path)
}

fn without_position_suffix(path: &str) -> &str {
    POSITION_SUFFIX
        .find(path)
        .map_or(path, |found| &path[..found.start()])
}

fn has_relative_path_prefix(path: &str) -> bool {
    path.starts_with("~/") || path.starts_with("./") || path.starts_with("../")
}

fn trim_trailing_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

fn looks_like_hostname(segment: &str, has_position: bool) -> bool {
    if segment.starts_with('.') {
        return false;
    }
    let lowered = segment.to_lowercase();
    if lowered == "localhost" || NUMERIC_DOTTED.is_match(segment) {
        return true;
    }
    let labels: Vec<&str> = lowered.split('.').collect();
    let last = labels[labels.len() - 1];
    if labels.len() < 2 {
        return false;
    }
    GENERIC_HOSTNAME_TLDS.contains(&last)
        || (!has_position && COUNTRY_HOSTNAME_TLDS.contains(&last))
}

/// Picks path-shaped inline code for the client's markdown file-link resolver.
/// It does not resolve paths or turn plain prose and fenced code into links.
pub fn inline_code_file_path_candidate(code_text: &str) -> Option<String> {
    let trimmed = js_trim(code_text);
    if trimmed.is_empty() || trimmed.chars().any(|c| is_js_space(c) || c == '`') {
        return None;
    }
    let candidate = if is_windows_absolute_path(trimmed) {
        trimmed.to_owned()
    } else {
        trimmed.replace('\\', "/")
    };
    let has_position = has_position_suffix(&candidate);
    if !has_position && !candidate.contains(['/', '\\']) {
        return None;
    }
    let has_explicit_path_shape = has_relative_path_prefix(&candidate)
        || candidate.starts_with('/')
        || is_windows_absolute_path(&candidate);
    if !has_explicit_path_shape {
        let without_position = without_position_suffix(&candidate);
        let first_segment = without_position
            .split('/')
            .next()
            .unwrap_or(without_position);
        if looks_like_hostname(first_segment, has_position) {
            return None;
        }
        let basename = trim_trailing_separators(without_position)
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        if VERSION_SUFFIX.is_match(basename)
            || (!has_position && !FILE_EXTENSION.is_match(basename))
        {
            return None;
        }
    }
    Some(candidate)
}

pub fn safe_decode_uri_component(value: &str) -> String {
    decode_uri_component(value).unwrap_or_else(|| value.to_owned())
}

pub fn normalize_markdown_link_destination(value: &str) -> String {
    let trimmed = js_trim(value);
    trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(trimmed)
        .to_owned()
}

/// Browser URL parsers write `C:/foo` as `/C:/foo` for file URLs.
pub fn strip_slash_prefixed_windows_drive(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\')
    {
        path[1..].to_owned()
    } else {
        path.to_owned()
    }
}

/// A link destination's path (without the query) and its `#` fragment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkPathAndHash {
    pub path: String,
    pub hash: String,
}

pub fn split_markdown_link_search_and_hash(value: &str) -> LinkPathAndHash {
    let (path_with_search, hash) = value.split_at(value.find('#').unwrap_or(value.len()));
    let path = &path_with_search[..path_with_search.find('?').unwrap_or(path_with_search.len())];
    LinkPathAndHash {
        path: path.to_owned(),
        hash: hash.to_owned(),
    }
}

/// Turns a `file:` URL into a host path, still percent-encoded so callers that
/// decode every destination in one place do not decode file URLs twice. A
/// non-localhost authority becomes a UNC share.
pub fn parse_file_url_href(href: &str) -> Option<LinkPathAndHash> {
    let parsed = url::Url::parse(href).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    let host = parsed
        .host_str()
        .filter(|host| !host.eq_ignore_ascii_case("localhost"))
        .unwrap_or("");
    let path = if host.is_empty() {
        parsed.path().to_owned()
    } else {
        format!("\\\\{host}{}", parsed.path().replace('/', "\\"))
    };
    if path.is_empty() {
        return None;
    }
    Some(LinkPathAndHash {
        path: strip_slash_prefixed_windows_drive(&path),
        hash: url_hash(&parsed),
    })
}

/// JavaScript `URL.hash`: empty when the fragment is absent or empty.
fn url_hash(url: &url::Url) -> String {
    url.fragment()
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| format!("#{fragment}"))
        .unwrap_or_default()
}

/// A file path with the 1-based line and column a link points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

fn positive(digits: Option<regex::Match<'_>>) -> Option<u64> {
    // The digits always parse; JavaScript keeps an oversized number positive.
    let value = digits?.as_str().parse::<u64>().unwrap_or(u64::MAX);
    (value > 0).then_some(value)
}

/// Returns the one-based line target only when it fits the rendered file.
/// Keeping this check in core lets native clients handle oversized Markdown
/// line numbers without converting them to a platform integer first.
pub fn markdown_line_target(line: u64, line_count: u64) -> Option<u64> {
    (line > 0 && line <= line_count).then_some(line)
}

/// Recognizes a PDF resource by its path, ignoring a query or fragment.
/// Resource routes need this decision before a native client asks the Host
/// for text, so all clients use the same case-insensitive extension rule.
pub fn is_pdf_file(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.get(path.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".pdf"))
}

pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    if let Some(suffix) = POSITION_SUFFIX.captures(path) {
        let start = suffix.get(0).expect("whole match").start();
        return FilePathPosition {
            path: path[..start].to_owned(),
            line: positive(suffix.get(1)),
            column: positive(suffix.get(2)),
        };
    }
    match POSITION_HASH.captures(hash) {
        Some(anchor) => FilePathPosition {
            path: path.to_owned(),
            line: positive(anchor.get(1)),
            column: positive(anchor.get(2)),
        },
        None => FilePathPosition {
            path: path.to_owned(),
            ..FilePathPosition::default()
        },
    }
}

pub fn format_file_path_position(position: &FilePathPosition) -> String {
    match (position.line, position.column) {
        (Some(line), Some(column)) => format!("{}:{line}:{column}", position.path),
        (Some(line), None) => format!("{}:{line}", position.path),
        (None, _) => position.path.clone(),
    }
}

/// Keeps filename and destination-path labels compact without discarding prose.
pub fn is_markdown_file_link_label(label: &str, href: &str) -> bool {
    let Some(destination) = parse_markdown_file_link(href) else {
        return false;
    };
    fn normalize(path: &str) -> String {
        let path = path.replace('\\', "/");
        let path = path.strip_prefix("./").unwrap_or(&path);
        path.trim_end_matches('/').to_owned()
    }
    let label_position = split_file_path_position(js_trim(label), "");
    if label_position
        .line
        .is_some_and(|line| Some(line) != destination.line)
        || label_position
            .column
            .is_some_and(|column| Some(column) != destination.column)
    {
        return false;
    }
    let mut label_path = normalize(&label_position.path);
    let mut destination_path = normalize(&destination.path);
    if label_path.is_empty() {
        return true;
    }
    if is_windows_absolute_path(&destination.path) {
        label_path = label_path.to_lowercase();
        destination_path = destination_path.to_lowercase();
    }
    destination_path == label_path || destination_path.ends_with(&format!("/{label_path}"))
}

fn looks_like_posix_filesystem_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if POSIX_FILE_ROOT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || has_position_suffix(path)
    {
        return true;
    }
    let basename = &path[path.rfind('/').map_or(0, |index| index + 1)..];
    EXTENSIONLESS_FILE_NAMES.contains(&basename) || FILE_EXTENSION.is_match(basename)
}

/// Decides whether a decoded link destination is a file path rather than a route
/// or prose. Only a `:line` suffix the author wrote counts as evidence; a `#L`
/// anchor never turns `/chat/settings` into a file.
fn looks_like_file_path(path: &str, authored_path: &str) -> bool {
    if is_windows_absolute_path(path) || has_relative_path_prefix(path) {
        return true;
    }
    if path.starts_with('/') {
        return looks_like_posix_filesystem_path(authored_path);
    }
    EXTENSIONLESS_FILE_NAMES.contains(&path)
        || RELATIVE_FILE_PATH.is_match(authored_path)
        || RELATIVE_FILE_NAME.is_match(path)
}

/// `^([A-Za-z][A-Za-z0-9+.-]*):(.*)$` where `.` excludes JavaScript line terminators.
fn external_scheme_rest(path: &str) -> Option<&str> {
    let colon = path.find(':')?;
    let (scheme, rest) = (&path[..colon], &path[colon + 1..]);
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    let single_line = !rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']);
    (valid_scheme && single_line).then_some(rest)
}

fn has_external_scheme(path: &str) -> bool {
    if is_windows_absolute_path(path) {
        return false;
    }
    let Some(rest) = external_scheme_rest(path) else {
        return false;
    };
    if rest.starts_with("//") {
        return true;
    }
    // `name:12` and `name:12:3` are positions, not schemes.
    let position_only = rest
        .split_once(':')
        .map_or((rest, None), |(line, column)| (line, Some(column)));
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    !(digits(position_only.0) && position_only.1.is_none_or(digits))
}

pub fn parse_markdown_file_link(href: &str) -> Option<FilePathPosition> {
    let normalized = normalize_markdown_link_destination(href);
    if normalized.is_empty() || normalized.starts_with('#') || normalized.starts_with("//") {
        return None;
    }
    let is_file_url = normalized
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"));
    let source = is_file_url
        .then(|| parse_file_url_href(&normalized))
        .flatten()
        .unwrap_or_else(|| split_markdown_link_search_and_hash(&normalized));
    // A percent-encoded drive colon (`/c%3A/`) only becomes strippable once decoded.
    let path =
        strip_slash_prefixed_windows_drive(&safe_decode_uri_component(js_trim(&source.path)));
    let hash = safe_decode_uri_component(js_trim(&source.hash));
    if path.is_empty() || has_external_scheme(&path) {
        return None;
    }
    let position = split_file_path_position(&path, &hash);
    looks_like_file_path(&position.path, &path).then_some(position)
}

pub fn file_basename(path: &str) -> String {
    // A trailing separator is a valid way to write a directory. Trim it before
    // taking the final segment so the label is never empty.
    let trimmed = trim_trailing_separators(path);
    if trimmed.is_empty() {
        return path.to_owned();
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_owned()
}

pub fn workspace_relative_file_path(path: &str, workspace_root: Option<&str>) -> Option<String> {
    let workspace_root = workspace_root.filter(|root| !root.is_empty())?;
    let normalized_path = strip_slash_prefixed_windows_drive(&path.replace('\\', "/"));
    let normalized_root = strip_slash_prefixed_windows_drive(&workspace_root.replace('\\', "/"));
    let normalized_root = normalized_root.trim_end_matches('/');
    let case_insensitive =
        is_windows_absolute_path(&strip_slash_prefixed_windows_drive(workspace_root));
    let (path_for_compare, root_for_compare) = if case_insensitive {
        (
            normalized_path.to_lowercase(),
            normalized_root.to_lowercase(),
        )
    } else {
        (normalized_path.clone(), normalized_root.to_owned())
    };
    if path_for_compare.trim_end_matches('/') == root_for_compare {
        return Some(".".into());
    }
    if !path_for_compare.starts_with(&format!("{root_for_compare}/")) {
        return None;
    }
    Some(utf16_skip(&normalized_path, utf16_len(normalized_root) + 1).to_owned())
}

/// How a native client presents a Markdown link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownLinkPresentation {
    External {
        href: String,
        host: String,
    },
    File {
        link: MarkdownFileLink,
    },
    /// Only `mailto:` and `tel:` destinations stay openable.
    Link {
        href: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownFileLink {
    pub href: String,
    pub icon: MarkdownFileIcon,
    pub label: String,
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// Sites whose brand mark replaces the generic external-link glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownLinkIcon {
    Github,
}

/// The brand marks are monochrome and tinted with the link color, so they follow the theme.
pub fn markdown_link_icon(host: &str) -> Option<MarkdownLinkIcon> {
    let hostname = host.to_lowercase();
    (hostname == "github.com" || hostname.ends_with(".github.com"))
        .then_some(MarkdownLinkIcon::Github)
}

/// Native link and media APIs have no document scheme to inherit from protocol-relative URLs.
pub fn native_markdown_url(value: &str) -> String {
    if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    }
}

macro_rules! file_icons {
    ($($variant:ident => $name:literal,)*) => {
        /// File chip icons, named after the Pierre icon set's assets.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
        pub enum MarkdownFileIcon { $($variant,)* }

        impl MarkdownFileIcon {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }
        }
    };
}

file_icons! {
    Agents => "agents", Astro => "astro", Babel => "babel", Bash => "bash", Biome => "biome",
    Browserslist => "browserslist", Bun => "bun", C => "c", Claude => "claude", Cpp => "cpp",
    Css => "css", Database => "database", Default => "default", Docker => "docker",
    Eslint => "eslint", Font => "font", Git => "git", Go => "go", Graphql => "graphql",
    Html => "html", Image => "image", Javascript => "javascript", Json => "json",
    Markdown => "markdown", Nextjs => "nextjs", Npm => "npm", Oxc => "oxc", Pnpm => "pnpm",
    Postcss => "postcss", Prettier => "prettier", Python => "python", React => "react",
    Ruby => "ruby", Rust => "rust", Sass => "sass", Stylelint => "stylelint", Svelte => "svelte",
    Svg => "svg", Svgo => "svgo", Swift => "swift", Table => "table", Tailwind => "tailwind",
    Terraform => "terraform", Text => "text", Typescript => "typescript", Video => "video",
    Vite => "vite", Vscode => "vscode", Vue => "vue", Wasm => "wasm", Webpack => "webpack",
    Yml => "yml", Zig => "zig", Zip => "zip",
}

fn file_icon_by_name(name: &str) -> Option<MarkdownFileIcon> {
    use MarkdownFileIcon::*;
    Some(match name {
        ".babelrc" | ".babelrc.json" | "babel.config.js" | "babel.config.cjs"
        | "babel.config.json" | "babel.config.mjs" => Babel,
        ".bash_profile" | ".bashrc" | ".zprofile" | ".zshenv" | ".zshrc" => Bash,
        ".browserslistrc" => Browserslist,
        ".dockerignore"
        | "compose.yaml"
        | "compose.yml"
        | "docker-compose.yaml"
        | "docker-compose.yml"
        | "docker-compose.override.yml"
        | "dockerfile" => Docker,
        ".eslintignore" | ".eslintrc" | ".eslintrc.cjs" | ".eslintrc.js" | ".eslintrc.json"
        | ".eslintrc.yaml" | ".eslintrc.yml" | "eslint.config.js" | "eslint.config.cjs"
        | "eslint.config.mjs" | "eslint.config.mts" | "eslint.config.ts" => Eslint,
        ".gitattributes" | ".gitignore" | ".gitkeep" | ".gitmodules" => Git,
        ".oxlintrc.json" => Oxc,
        ".postcssrc" | ".postcssrc.json" | ".postcssrc.yaml" | ".postcssrc.yml"
        | "postcss.config.js" | "postcss.config.cjs" | "postcss.config.mjs"
        | "postcss.config.ts" => Postcss,
        ".prettierignore"
        | ".prettierrc"
        | ".prettierrc.json"
        | ".prettierrc.cjs"
        | ".prettierrc.js"
        | ".prettierrc.mjs"
        | ".prettierrc.toml"
        | ".prettierrc.yaml"
        | ".prettierrc.yml"
        | "prettier.config.js"
        | "prettier.config.cjs"
        | "prettier.config.mjs" => Prettier,
        ".stylelintignore"
        | ".stylelintrc"
        | ".stylelintrc.cjs"
        | ".stylelintrc.js"
        | ".stylelintrc.json"
        | ".stylelintrc.mjs"
        | ".stylelintrc.yaml"
        | ".stylelintrc.yml"
        | "stylelint.config.js"
        | "stylelint.config.cjs"
        | "stylelint.config.mjs" => Stylelint,
        ".terraform.lock.hcl" => Terraform,
        "agents.md" => Agents,
        "biome.json" | "biome.jsonc" => Biome,
        "bun.lock" | "bun.lockb" | "bunfig.toml" => Bun,
        "claude.md" => Claude,
        "gemfile" | "rakefile" => Ruby,
        "next.config.js" | "next.config.mjs" | "next.config.mts" | "next.config.ts" => Nextjs,
        "package.json" => Npm,
        "pnpm-lock.yaml" | "pnpm-workspace.yaml" => Pnpm,
        "readme.md" => Markdown,
        "svgo.config.js" | "svgo.config.cjs" | "svgo.config.mjs" | "svgo.config.ts" => Svgo,
        "tailwind.config.js"
        | "tailwind.config.cjs"
        | "tailwind.config.mjs"
        | "tailwind.config.ts" => Tailwind,
        "tsconfig.json" => Typescript,
        "vite.config.js" | "vite.config.mjs" | "vite.config.mts" | "vite.config.ts" => Vite,
        "webpack.config.js"
        | "webpack.config.babel.js"
        | "webpack.config.cjs"
        | "webpack.config.mjs"
        | "webpack.config.ts" => Webpack,
        _ => return None,
    })
}

fn file_icon_by_extension(extension: &str) -> Option<MarkdownFileIcon> {
    use MarkdownFileIcon::*;
    Some(match extension {
        "7z" | "bz2" | "gz" | "jar" | "rar" | "tar" | "tgz" | "zip" => Zip,
        "astro" => Astro,
        "avif" | "bmp" | "gif" | "ico" | "icns" | "jpeg" | "jpg" | "png" | "webp" => Image,
        "code-workspace" => Vscode,
        "bash" | "fish" | "sh" | "zsh" => Bash,
        "c" | "h" => C,
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "inl" => Cpp,
        "css" | "less" | "postcss" => Css,
        "csv" | "tsv" => Table,
        "cts" | "mts" | "ts" => Typescript,
        "db" | "sql" | "sqlite" | "sqlite3" => Database,
        "env" | "env.development" | "env.local" | "env.production" | "ini" | "txt" => Text,
        "eot" | "woff" | "woff2" => Font,
        "erb" | "rake" | "rb" => Ruby,
        "go" => Go,
        "gql" | "graphql" => Graphql,
        "htm" | "html" => Html,
        "js" | "mjs" => Javascript,
        "jsx" | "tsx" => React,
        "json" | "jsonc" => Json,
        "md" | "mdx" | "mdx.tsx" => Markdown,
        "py" | "pyi" | "pyw" | "pyx" => Python,
        "rs" => Rust,
        "sass" | "scss" => Sass,
        "svelte" => Svelte,
        "svg" => Svg,
        "swift" => Swift,
        "tf" | "tfstate" | "tfvars" => Terraform,
        "vue" => Vue,
        "wasm" => Wasm,
        "yml" | "yaml" => Yml,
        "zig" => Zig,
        _ => return None,
    })
}

/// Recognizes videos by extension when nothing recorded the file's MIME type.
pub(crate) fn is_video_file_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_lowercase().as_str(),
            "avi" | "m4v" | "mkv" | "mov" | "mp4" | "ogv" | "webm"
        )
    })
}

pub fn markdown_file_icon(value: &str) -> MarkdownFileIcon {
    let basename = file_basename(value);
    let basename = without_position_suffix(&basename).to_lowercase();
    if is_video_file_name(&basename) {
        return MarkdownFileIcon::Video;
    }
    if let Some(icon) = file_icon_by_name(&basename) {
        return icon;
    }
    if basename.starts_with("tsconfig.") && basename.ends_with(".json") {
        return MarkdownFileIcon::Typescript;
    }
    basename
        .match_indices('.')
        .find_map(|(index, _)| file_icon_by_extension(&basename[index + 1..]))
        .unwrap_or(MarkdownFileIcon::Default)
}

pub fn markdown_link_presentation(href: &str) -> MarkdownLinkPresentation {
    let normalized = normalize_markdown_link_destination(href);
    if let Ok(parsed) = url::Url::parse(&native_markdown_url(&normalized))
        && matches!(parsed.scheme(), "http" | "https")
    {
        return MarkdownLinkPresentation::External {
            host: parsed.host_str().unwrap_or("").to_owned(),
            href: parsed.into(),
        };
    }
    if let Some(target) = parse_markdown_file_link(&normalized) {
        return MarkdownLinkPresentation::File {
            link: MarkdownFileLink {
                href: normalized,
                icon: markdown_file_icon(&target.path),
                label: file_basename(&format_file_path_position(&target)),
                line: target.line,
                column: target.column,
                path: target.path,
            },
        };
    }
    let openable = ["mailto:", "tel:"].iter().any(|scheme| {
        normalized
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    });
    MarkdownLinkPresentation::Link {
        href: openable.then_some(normalized),
    }
}

/// Backticks become file references only when the shared path heuristic recognizes the whole span.
pub fn markdown_inline_code_presentation(content: &str) -> Option<MarkdownFileLink> {
    match markdown_link_presentation(&inline_code_file_path_candidate(content)?) {
        MarkdownLinkPresentation::File { link } => Some(link),
        _ => None,
    }
}

/// A Host path, as the mobile file screen tells workspace files from others.
pub fn is_absolute_file_path(value: &str) -> bool {
    value.starts_with('/') || is_windows_absolute_path(value)
}

fn normalize_relative_path(value: &str) -> Option<String> {
    let mut segments: Vec<&str> = vec![];
    for segment in value.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            _ => segments.push(segment),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

/// A link target as a path inside the workspace: relative paths normalized,
/// absolute ones under `workspace_root` made relative; `None` outside it.
pub fn workspace_file_path(workspace_root: Option<&str>, target: &str) -> Option<String> {
    if !is_absolute_file_path(target) {
        if target.starts_with("~/") || target.starts_with("~\\") {
            return None;
        }
        return normalize_relative_path(target);
    }
    let root = workspace_root.filter(|root| !root.is_empty())?;
    let normalized_target = target.replace('\\', "/");
    let normalized_root = root.replace('\\', "/");
    let normalized_root = normalized_root.trim_end_matches('/');
    let case_insensitive = is_windows_absolute_path(target) || is_windows_absolute_path(root);
    let (comparable_target, comparable_root) = if case_insensitive {
        (
            normalized_target.to_lowercase(),
            normalized_root.to_lowercase(),
        )
    } else {
        (normalized_target.clone(), normalized_root.to_owned())
    };
    if !comparable_target.starts_with(&format!("{comparable_root}/")) {
        return None;
    }
    let relative = utf16_skip(&normalized_target, utf16_len(normalized_root) + 1);
    // `/repo/../x` starts with the root but escapes it.
    if relative.split('/').any(|segment| segment == "..") {
        return None;
    }
    normalize_relative_path(relative)
}

/// The file screen's subtitle: the project and the file's folder, or only the
/// folder of a Host file outside the workspace.
pub fn file_header_subtitle(project_name: &str, path: &str) -> String {
    let parent = &path[..path.rfind(['/', '\\']).unwrap_or(0)];
    if is_absolute_file_path(path) {
        parent.to_owned()
    } else {
        [project_name, parent]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// What tapping a link in a conversation does.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MarkdownLinkAction {
    /// A file of the thread's workspace, by its workspace-relative path.
    WorkspaceFile { path: String, line: Option<u64> },
    /// A Host file outside the workspace, such as a report in a temp directory.
    HostFile { path: String, line: Option<u64> },
    /// Web, mail and phone links open outside the app.
    External { url: String },
    /// Nothing the app can open.
    Nothing,
}

pub fn markdown_link_action(href: &str, workspace_root: Option<&str>) -> MarkdownLinkAction {
    match markdown_link_presentation(href) {
        MarkdownLinkPresentation::File { link } => {
            match workspace_file_path(workspace_root, &link.path) {
                Some(path) => MarkdownLinkAction::WorkspaceFile {
                    path,
                    line: link.line,
                },
                None if is_absolute_file_path(&link.path) => MarkdownLinkAction::HostFile {
                    path: link.path,
                    line: link.line,
                },
                None => MarkdownLinkAction::Nothing,
            }
        }
        MarkdownLinkPresentation::External { href, .. }
        | MarkdownLinkPresentation::Link { href: Some(href) } => {
            MarkdownLinkAction::External { url: href }
        }
        MarkdownLinkPresentation::Link { href: None } => MarkdownLinkAction::Nothing,
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
