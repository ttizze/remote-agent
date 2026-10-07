//! Project path rules for adding projects and browsing folders. Paths are the
//! Host's, so Windows forms are recognized even on a Unix client.
use crate::models::Project;
use crate::presentation::markdown::links::{
    is_unc_path, is_windows_absolute_path, is_windows_drive_path,
};

pub fn is_explicit_relative_path(value: &str) -> bool {
    value == "."
        || value == ".."
        || ["./", "../", ".\\", "..\\"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
}

fn is_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// `C:` is "the current directory on C:", so only `C:\` and `C:/` are roots.
fn is_root_path(value: &str) -> bool {
    value == "/"
        || value == "\\"
        || (value.len() == 3
            && is_drive_prefix(value)
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}

fn trim_trailing_path_separators(value: &str) -> String {
    if value.is_empty() || is_root_path(value) {
        return value.into();
    }
    let trimmed = if value.starts_with('/') {
        value.trim_end_matches('/')
    } else {
        value.trim_end_matches(['/', '\\'])
    };
    if trimmed.is_empty() {
        return value.into();
    }
    if trimmed.len() == 2 && is_drive_prefix(trimmed) {
        return format!("{trimmed}\\");
    }
    trimmed.into()
}

pub fn normalize_project_path_for_dispatch(value: &str) -> String {
    trim_trailing_path_separators(value.trim())
}

/// Windows paths compare case-insensitively with either separator.
pub fn normalize_project_path_for_comparison(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    if is_windows_drive_path(&normalized) || is_unc_path(&normalized) {
        return normalized.replace('/', "\\").to_lowercase();
    }
    normalized
}

/// Whether a Host platform name (`process.platform`, `navigator.platform`) is Windows.
pub fn is_windows_platform(platform: &str) -> bool {
    platform
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("win"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AbsoluteKind {
    Unix,
    Windows,
}

fn absolute_path_kind(value: &str) -> Option<AbsoluteKind> {
    if is_windows_drive_path(value) || is_unc_path(value) {
        Some(AbsoluteKind::Windows)
    } else if value.starts_with('/') {
        Some(AbsoluteKind::Unix)
    } else {
        None
    }
}

fn preferred_path_separator(value: &str) -> char {
    match absolute_path_kind(value) {
        Some(AbsoluteKind::Windows) => '\\',
        Some(AbsoluteKind::Unix) => '/',
        None if value.contains('\\') => '\\',
        None => '/',
    }
}

pub fn has_trailing_path_separator(value: &str) -> bool {
    if absolute_path_kind(value) == Some(AbsoluteKind::Unix) {
        value.ends_with('/')
    } else {
        value.ends_with(['/', '\\'])
    }
}

fn split_path_segments(value: &str, separator: char) -> Vec<String> {
    let segments: Vec<&str> = if separator == '/' {
        value.split('/').collect()
    } else {
        value.split(['/', '\\']).collect()
    };
    segments
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

fn last_path_separator_index(value: &str) -> Option<usize> {
    if absolute_path_kind(value) == Some(AbsoluteKind::Unix) {
        value.rfind('/')
    } else {
        value.rfind(['/', '\\'])
    }
}

struct AbsolutePath {
    root: String,
    separator: char,
    segments: Vec<String>,
}

fn split_absolute_path(value: &str) -> Option<AbsolutePath> {
    if is_windows_drive_path(value) {
        let root = format!("{}\\", &value[..2]);
        let segments = split_path_segments(value.get(root.len()..).unwrap_or(""), '\\');
        return Some(AbsolutePath {
            root,
            separator: '\\',
            segments,
        });
    }
    if is_unc_path(value) {
        let mut segments = split_path_segments(value, '\\').into_iter();
        let (server, share) = (segments.next()?, segments.next()?);
        return Some(AbsolutePath {
            root: format!("\\\\{server}\\{share}\\"),
            separator: '\\',
            segments: segments.collect(),
        });
    }
    value.strip_prefix('/').map(|rest| AbsolutePath {
        root: "/".into(),
        separator: '/',
        segments: split_path_segments(rest, '/'),
    })
}

/// Whether typed text browses folders instead of naming one.
pub fn is_filesystem_browse_query(value: &str, platform: &str) -> bool {
    ["./", "../", ".\\", "..\\", "/", "~/"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || (is_windows_platform(platform) && is_windows_absolute_path(value))
}

pub fn is_unsupported_windows_project_path(value: &str, platform: &str) -> bool {
    is_windows_absolute_path(value) && !is_windows_platform(platform)
}

/// `./x` and `../x` resolve against `cwd`; other paths only normalize.
pub fn resolve_project_path_for_dispatch(value: &str, cwd: Option<&str>) -> String {
    let trimmed = value.trim();
    let cwd = cwd.filter(|cwd| !cwd.is_empty());
    let base = cwd.and_then(|cwd| split_absolute_path(&normalize_project_path_for_dispatch(cwd)));
    let (true, Some(base)) = (is_explicit_relative_path(trimmed), base) else {
        return normalize_project_path_for_dispatch(trimmed);
    };
    let mut segments = base.segments;
    for segment in trimmed.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment.into()),
        }
    }
    let joined = segments.join(&base.separator.to_string());
    normalize_project_path_for_dispatch(&format!("{}{joined}", base.root))
}

/// The project with a root at this path. A project's roots all count.
pub fn find_project_by_path<'a>(projects: &'a [Project], candidate: &str) -> Option<&'a Project> {
    let candidate = normalize_project_path_for_comparison(candidate);
    if candidate.is_empty() {
        return None;
    }
    projects.iter().find(|project| {
        project
            .roots
            .iter()
            .any(|root| normalize_project_path_for_comparison(&root.path) == candidate)
    })
}

/// The folder name a path would give a new project.
pub fn infer_project_title_from_path(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    let last = match split_absolute_path(&normalized) {
        Some(path) => path.segments.last().cloned(),
        None => normalized
            .split(['/', '\\'])
            .rfind(|segment| !segment.is_empty())
            .map(str::to_owned),
    };
    last.unwrap_or(normalized)
}

pub fn append_browse_path_segment(current_path: &str, segment: &str) -> String {
    format!(
        "{}{segment}{}",
        browse_directory_path(current_path),
        preferred_path_separator(current_path)
    )
}

/// The partial name after the last separator, which filters the listing.
pub fn browse_leaf_path_segment(current_path: &str) -> &str {
    last_path_separator_index(current_path).map_or(current_path, |index| &current_path[index + 1..])
}

pub fn browse_directory_path(current_path: &str) -> &str {
    if has_trailing_path_separator(current_path) {
        return current_path;
    }
    last_path_separator_index(current_path).map_or(current_path, |index| &current_path[..=index])
}

pub fn ensure_browse_directory_path(current_path: &str) -> String {
    let trimmed = current_path.trim();
    if trimmed.is_empty() || has_trailing_path_separator(trimmed) {
        return trimmed.into();
    }
    format!("{trimmed}{}", preferred_path_separator(trimmed))
}

pub fn browse_parent_path(current_path: &str) -> Option<String> {
    let trimmed = normalize_project_path_for_dispatch(current_path);
    if let Some(path) = split_absolute_path(&trimmed) {
        return match path.segments.len() {
            0 => None,
            1 => Some(path.root),
            count => Some(format!(
                "{}{}{}",
                path.root,
                path.segments[..count - 1].join(&path.separator.to_string()),
                path.separator
            )),
        };
    }
    let separator = preferred_path_separator(current_path);
    let index = last_path_separator_index(&trimmed)?;
    if index == 2 && is_drive_prefix(&trimmed) {
        return Some(format!("{}{separator}", &trimmed[..2]));
    }
    Some(trimmed[..=index].into())
}

/// Going up is offered only once a folder has been entered.
pub fn can_navigate_up(current_path: &str) -> bool {
    has_trailing_path_separator(current_path) && browse_parent_path(current_path).is_some()
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::models::{Project, ProjectRoot};

    pub fn project(id: &str, root: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            roots: vec![ProjectRoot { path: root.into() }],
            ..Project::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::project;
    use super::*;

    #[test]
    fn detects_explicit_relative_paths() {
        assert!(is_explicit_relative_path("."));
        assert!(is_explicit_relative_path(".."));
        assert!(is_explicit_relative_path("./repo"));
        assert!(is_explicit_relative_path("..\\repo"));
        assert!(!is_explicit_relative_path("~/repo"));
        assert!(is_explicit_relative_path("./docs"));
        assert!(!is_explicit_relative_path("/repo/docs"));
    }

    #[test]
    fn normalizes_a_bare_windows_drive_root_the_same_as_one_with_a_trailing_separator() {
        assert_eq!(normalize_project_path_for_dispatch("C:"), "C:\\");
        assert_eq!(normalize_project_path_for_comparison("C:"), "c:\\");
        assert_eq!(
            normalize_project_path_for_comparison("C:"),
            normalize_project_path_for_comparison("C:\\")
        );
        assert_eq!(
            normalize_project_path_for_comparison("C:"),
            normalize_project_path_for_comparison("C:/")
        );
        assert_eq!(
            normalize_project_path_for_dispatch("C:\\repo\\"),
            "C:\\repo"
        );
    }

    #[test]
    fn normalizes_trailing_separators_for_dispatch_and_comparison() {
        assert_eq!(
            normalize_project_path_for_dispatch(" /repo/app/ "),
            "/repo/app"
        );
        assert_eq!(
            normalize_project_path_for_comparison("/repo/app/"),
            "/repo/app"
        );
    }

    #[test]
    fn normalizes_windows_style_paths_for_comparison() {
        assert_eq!(
            normalize_project_path_for_comparison("C:/Work/Repo/"),
            "c:\\work\\repo"
        );
        assert_eq!(
            normalize_project_path_for_comparison("C:\\Work\\Repo\\"),
            "c:\\work\\repo"
        );
    }

    #[test]
    fn finds_existing_projects_even_when_the_input_formatting_differs() {
        let projects = [
            project("project-1", "/repo/app"),
            project("project-2", "C:\\Work\\Repo"),
        ];
        assert_eq!(
            find_project_by_path(&projects, "C:/Work/Repo/").map(|p| p.id.as_str()),
            Some("project-2")
        );
    }

    #[test]
    fn infers_project_titles_from_normalized_paths() {
        assert_eq!(infer_project_title_from_path("/repo/app/"), "app");
        assert_eq!(infer_project_title_from_path("C:\\Work\\Repo\\"), "Repo");
        assert_eq!(
            infer_project_title_from_path("/home/user\\project/"),
            "user\\project"
        );
    }

    #[test]
    fn detects_browse_queries_across_supported_path_styles() {
        assert!(!is_filesystem_browse_query(".", ""));
        assert!(!is_filesystem_browse_query("..", ""));
        assert!(is_filesystem_browse_query("./", ""));
        assert!(is_filesystem_browse_query("../", ""));
        assert!(is_filesystem_browse_query("~/projects", ""));
        assert!(is_filesystem_browse_query("..\\docs", ""));
        assert!(!is_filesystem_browse_query("notes", ""));
    }

    #[test]
    fn only_treats_windows_style_paths_as_browse_queries_on_windows() {
        assert!(!is_filesystem_browse_query("C:\\Work\\Repo\\", "MacIntel"));
        assert!(is_filesystem_browse_query("C:\\Work\\Repo\\", "Win32"));
        assert!(is_unsupported_windows_project_path(
            "C:\\Work\\Repo\\",
            "MacIntel"
        ));
        assert!(!is_unsupported_windows_project_path(
            "C:\\Work\\Repo\\",
            "Win32"
        ));
    }

    #[test]
    fn resolves_explicit_relative_paths_against_the_current_project() {
        assert_eq!(
            resolve_project_path_for_dispatch(".", Some("/repo/app")),
            "/repo/app"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("..", Some("/repo/app")),
            "/repo"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("./docs", Some("/repo/app")),
            "/repo/app/docs"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("../docs", Some("/repo/app")),
            "/repo/docs"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("./Repo", Some("C:\\Work")),
            "C:\\Work\\Repo"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("./docs", Some("/home/user\\project")),
            "/home/user\\project/docs"
        );
    }

    #[test]
    fn navigates_browse_paths_with_matching_separators() {
        assert_eq!(append_browse_path_segment("/repo/", "src"), "/repo/src/");
        assert_eq!(
            append_browse_path_segment("C:\\Work\\", "Repo"),
            "C:\\Work\\Repo\\"
        );
        assert_eq!(
            append_browse_path_segment("/home/user\\project/", "docs"),
            "/home/user\\project/docs/"
        );
        assert_eq!(browse_parent_path("/repo/src/").as_deref(), Some("/repo/"));
        assert_eq!(
            browse_parent_path("C:\\Work\\Repo\\").as_deref(),
            Some("C:\\Work\\")
        );
        assert_eq!(browse_parent_path("\\\\server\\share\\"), None);
        assert_eq!(
            browse_parent_path("\\\\server\\share\\repo\\").as_deref(),
            Some("\\\\server\\share\\")
        );
        assert_eq!(browse_parent_path("C:\\"), None);
        assert_eq!(
            browse_parent_path("/home/user\\project/docs/").as_deref(),
            Some("/home/user\\project/")
        );
    }

    #[test]
    fn detects_browse_path_boundaries() {
        assert!(has_trailing_path_separator("/repo/src/"));
        assert!(!has_trailing_path_separator("/repo/src"));
        assert_eq!(browse_directory_path("/repo/src"), "/repo/");
        assert_eq!(browse_directory_path("/repo/src/"), "/repo/src/");
        assert_eq!(browse_leaf_path_segment("/repo/src"), "src");
        assert_eq!(browse_leaf_path_segment("C:\\Work\\Repo\\Docs"), "Docs");
        assert_eq!(
            browse_directory_path("/home/user\\project/docs"),
            "/home/user\\project/"
        );
        assert_eq!(browse_leaf_path_segment("/home/user\\project/docs"), "docs");
    }

    #[test]
    fn only_allows_browse_up_after_entering_a_directory() {
        assert!(!can_navigate_up("~/repo"));
        assert!(!can_navigate_up("~/a"));
        assert!(can_navigate_up("~/repo/"));
        assert!(!can_navigate_up("\\\\server\\share\\"));
        assert!(can_navigate_up("\\\\server\\share\\repo\\"));
    }

    proptest::proptest! {
        #[test]
        fn normalizing_twice_changes_nothing(path in "[a-zA-Z:/\\\\.]{0,12}") {
            let once = normalize_project_path_for_comparison(&path);
            proptest::prop_assert_eq!(normalize_project_path_for_comparison(&once), once);
        }
    }
}
