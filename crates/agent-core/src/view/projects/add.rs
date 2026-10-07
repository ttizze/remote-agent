//! Adding a local folder as a project: the browse field and the path check
//! before `RegisterProject`.
use super::paths::{
    browse_directory_path, browse_leaf_path_segment, browse_parent_path, can_navigate_up,
    ensure_browse_directory_path, find_project_by_path, has_trailing_path_separator,
    is_explicit_relative_path, is_filesystem_browse_query, is_unsupported_windows_project_path,
    resolve_project_path_for_dispatch,
};
use crate::models::{FileEntry, Project};
use crate::view::collation::locale_compare;

/// Projects are added only while the Host is connected.
pub fn can_add_project(connected: bool) -> bool {
    connected
}

/// The browse field starts at the configured folder, or the home folder.
pub fn add_project_initial_query(base_directory: Option<&str>) -> String {
    match base_directory.map(str::trim).unwrap_or("") {
        "" => "~/".into(),
        base => ensure_browse_directory_path(base),
    }
}

/// What the browse field lists for the typed text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FilesystemBrowsePath {
    pub is_browsing: bool,
    /// The folder to list.
    pub directory_path: String,
    /// The partial name after the folder that filters the listing.
    pub filter_query: String,
    pub parent_path: Option<String>,
    pub can_browse_up: bool,
}

pub fn filesystem_browse_path(query: &str, platform: &str, enabled: bool) -> FilesystemBrowsePath {
    let is_browsing = enabled && is_filesystem_browse_query(query, platform);
    let directory_path = if is_browsing {
        browse_directory_path(query).to_owned()
    } else {
        String::new()
    };
    FilesystemBrowsePath {
        is_browsing,
        filter_query: if is_browsing && !has_trailing_path_separator(query) {
            browse_leaf_path_segment(query).into()
        } else {
            String::new()
        },
        parent_path: is_browsing
            .then(|| browse_parent_path(&directory_path))
            .flatten(),
        can_browse_up: is_browsing && can_navigate_up(&directory_path),
        directory_path,
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FilesystemBrowseEntries {
    pub visible: Vec<FileEntry>,
    /// The folder whose name is exactly the query.
    pub exact: Option<FileEntry>,
}

/// The folders of a listing that match the typed name, sorted by name. Hidden
/// folders appear once the name starts with a dot.
pub fn filter_filesystem_browse_entries(
    entries: &[FileEntry],
    query: &str,
) -> FilesystemBrowseEntries {
    let lower_query = query.to_lowercase();
    let show_hidden = query.starts_with('.');
    let mut visible: Vec<FileEntry> = entries
        .iter()
        .filter(|entry| {
            entry.directory
                && entry.name.to_lowercase().starts_with(&lower_query)
                && (show_hidden || !entry.name.starts_with('.'))
        })
        .cloned()
        .collect();
    visible.sort_by(|left, right| locale_compare(&left.name, &right.name));
    let exact = (!query.is_empty())
        .then(|| visible.iter().find(|entry| entry.name == query).cloned())
        .flatten();
    FilesystemBrowseEntries { visible, exact }
}

/// The absolute path to register, or why the typed path cannot be one.
pub fn resolve_add_project_path(
    raw_path: &str,
    current_project_cwd: Option<&str>,
    platform: &str,
) -> Result<String, String> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err("Enter a project path.".into());
    }
    if is_unsupported_windows_project_path(raw_path, platform) {
        return Err("Windows-style paths are only supported on Windows environments.".into());
    }
    let current_project_cwd = current_project_cwd.filter(|cwd| !cwd.is_empty());
    if is_explicit_relative_path(raw_path) && current_project_cwd.is_none() {
        return Err("Relative paths require an active project in this environment.".into());
    }
    match resolve_project_path_for_dispatch(raw_path, current_project_cwd) {
        path if path.is_empty() => Err("Enter a project path.".into()),
        path => Ok(path),
    }
}

/// The project already registered at `path`.
pub fn find_existing_add_project<'a>(projects: &'a [Project], path: &str) -> Option<&'a Project> {
    find_project_by_path(projects, path)
}

/// A listed folder; choosing it puts `query` in the path field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderBrowserEntry {
    pub name: String,
    pub query: String,
}

/// The "Browse folders" list under the mobile path field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderBrowserView {
    pub is_browsing: bool,
    /// The folder to list with `Intent::ListFiles`.
    pub directory_path: String,
    /// The Host has listed `directory_path`.
    pub listed: bool,
    /// The ".." row's query.
    pub parent_query: Option<String>,
    pub entries: Vec<FolderBrowserEntry>,
}

/// The folders under the typed path that match its last segment, once the
/// Host has listed that folder.
pub fn folder_browser(
    query: &str,
    connected: bool,
    listed: Option<(&str, &[FileEntry])>,
) -> FolderBrowserView {
    let browse = filesystem_browse_path(query, "", connected);
    let listing = listed
        .filter(|(path, _)| browse.is_browsing && *path == browse.directory_path)
        .map(|(_, entries)| entries);
    FolderBrowserView {
        entries: listing
            .map(|entries| {
                filter_filesystem_browse_entries(entries, &browse.filter_query)
                    .visible
                    .into_iter()
                    .map(|entry| FolderBrowserEntry {
                        query: super::paths::append_browse_path_segment(
                            &browse.directory_path,
                            &entry.name,
                        ),
                        name: entry.name,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        listed: listing.is_some(),
        parent_query: browse.parent_path.filter(|_| browse.can_browse_up),
        is_browsing: browse.is_browsing,
        directory_path: browse.directory_path,
    }
}

/// What "Add project" does with the typed path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AddProjectTarget {
    /// Register the folder with `Intent::AddProject`.
    Add {
        path: String,
    },
    /// The folder is already this project: "Project already exists".
    Existing {
        project_id: String,
        title: String,
    },
    Invalid {
        message: String,
    },
}

/// The target of the mobile "Local folder" screen, which has no current
/// project to resolve relative paths against.
pub fn add_project_target(projects: &[Project], raw_path: &str) -> AddProjectTarget {
    match resolve_add_project_path(raw_path, None, "") {
        Err(message) => AddProjectTarget::Invalid { message },
        Ok(path) => match find_existing_add_project(projects, &path) {
            Some(project) => AddProjectTarget::Existing {
                project_id: project.id.clone(),
                title: project.name.clone(),
            },
            None => AddProjectTarget::Add { path },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::paths::fixtures::project;
    use super::*;

    fn entry(name: &str, directory: bool) -> FileEntry {
        FileEntry {
            name: name.into(),
            path: format!("/Users/test/{name}"),
            directory,
            size: 0,
        }
    }

    #[test]
    fn only_allows_project_creation_in_connected_environments() {
        assert!(can_add_project(true));
        assert!(!can_add_project(false));
    }

    #[test]
    fn resolves_initial_browse_paths_from_settings() {
        assert_eq!(add_project_initial_query(Some("")), "~/");
        assert_eq!(add_project_initial_query(None), "~/");
        assert_eq!(add_project_initial_query(Some("/work")), "/work/");
        assert_eq!(add_project_initial_query(Some("C:\\work")), "C:\\work\\");
    }

    #[test]
    fn rejects_unsupported_windows_paths_on_non_windows_environments() {
        assert_eq!(
            resolve_add_project_path("C:\\repo", None, "MacIntel"),
            Err("Windows-style paths are only supported on Windows environments.".into())
        );
    }

    #[test]
    fn resolves_relative_paths_from_the_active_project_cwd() {
        assert_eq!(
            resolve_add_project_path("../next", Some("/work/current"), "Linux"),
            Ok("/work/next".into())
        );
    }

    #[test]
    fn needs_a_path_and_an_active_project_for_relative_paths() {
        assert_eq!(
            resolve_add_project_path("  ", None, "Linux"),
            Err("Enter a project path.".into())
        );
        assert_eq!(
            resolve_add_project_path("./repo", None, "Linux"),
            Err("Relative paths require an active project in this environment.".into())
        );
    }

    #[test]
    fn finds_existing_projects_by_normalized_path() {
        let projects = [project("other", "/elsewhere"), project("project", "/repo/")];
        assert_eq!(
            find_existing_add_project(&projects, "/repo").map(|p| p.id.as_str()),
            Some("project")
        );
    }

    #[test]
    fn derives_the_browse_target_and_navigation_state() {
        assert_eq!(
            filesystem_browse_path("~/projects/app", "", true),
            FilesystemBrowsePath {
                is_browsing: true,
                directory_path: "~/projects/".into(),
                filter_query: "app".into(),
                parent_path: Some("~/".into()),
                can_browse_up: true,
            }
        );
        assert!(!filesystem_browse_path("C:\\Users\\test", "MacIntel", true).is_browsing);
        assert!(!filesystem_browse_path("~/projects/", "", false).is_browsing);
    }

    #[test]
    fn filters_names_hidden_directories_and_exact_matches_consistently() {
        let entries = [
            entry(".config", true),
            entry("Code", true),
            entry("codething", true),
        ];
        let names = |query: &str| {
            filter_filesystem_browse_entries(&entries, query)
                .visible
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("co"), ["Code", "codething"]);
        assert_eq!(filter_filesystem_browse_entries(&entries, "co").exact, None);
        assert_eq!(names(""), ["Code", "codething"]);
        assert_eq!(names("."), [".config"]);
        assert_eq!(
            filter_filesystem_browse_entries(&entries, "Code").exact,
            Some(entries[1].clone())
        );
    }

    #[test]
    fn the_listing_offers_only_folders_in_name_order() {
        let entries = [
            entry("zeta", true),
            entry("notes.txt", false),
            entry("Alpha", true),
            entry("alpha", true),
        ];
        assert_eq!(
            filter_filesystem_browse_entries(&entries, "")
                .visible
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            ["alpha", "Alpha", "zeta"]
        );
    }

    #[test]
    fn the_browser_lists_matching_folders_once_their_parent_is_listed() {
        let entries = [entry("app", true), entry("api", true), entry("docs", true)];
        let waiting = folder_browser("~/projects/a", true, Some(("~/", &entries)));
        assert!(waiting.is_browsing && !waiting.listed);
        assert_eq!(waiting.directory_path, "~/projects/");
        assert_eq!(waiting.parent_query.as_deref(), Some("~/"));
        let listed = folder_browser("~/projects/a", true, Some(("~/projects/", &entries)));
        assert_eq!(
            listed.entries,
            [
                FolderBrowserEntry {
                    name: "api".into(),
                    query: "~/projects/api/".into()
                },
                FolderBrowserEntry {
                    name: "app".into(),
                    query: "~/projects/app/".into()
                },
            ]
        );
        assert!(!folder_browser("~/projects/", false, None).is_browsing);
    }

    #[test]
    fn adding_a_registered_folder_opens_its_project() {
        let projects = [project("app", "/work/app")];
        assert_eq!(
            add_project_target(&projects, "/work/app/"),
            AddProjectTarget::Existing {
                project_id: "app".into(),
                title: "app".into()
            }
        );
        assert_eq!(
            add_project_target(&projects, " /work/new "),
            AddProjectTarget::Add {
                path: "/work/new".into()
            }
        );
        assert_eq!(
            add_project_target(&projects, "./new"),
            AddProjectTarget::Invalid {
                message: "Relative paths require an active project in this environment.".into()
            }
        );
    }
}
