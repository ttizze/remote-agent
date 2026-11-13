//! Project operations and labels are shared; clients present native menus and editors.
use super::list::ProjectSummary;
use crate::state::Intent;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ProjectAction {
    Dispatch(Box<Intent>),
    Archive(String),
    Edit(String),
    OpenFolder(String),
}

impl ProjectAction {
    /// Archive at the user's action time; menu data remains stable while the list updates.
    pub fn into_intent(self, now: i64) -> Result<Intent, Self> {
        match self {
            Self::Dispatch(intent) => Ok(*intent),
            Self::Archive(project_id) => Ok(Intent::ArchiveProjectChats {
                project_id,
                before: now,
            }),
            action => Err(action),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ProjectMenuItem {
    Action {
        title: String,
        action: ProjectAction,
        checked: bool,
        enabled: bool,
    },
    Submenu {
        title: String,
        items: Vec<ProjectMenuItem>,
    },
    Separator,
}

fn item(title: impl Into<String>, action: ProjectAction) -> ProjectMenuItem {
    ProjectMenuItem::Action {
        title: title.into(),
        action,
        checked: false,
        enabled: true,
    }
}

/// Inputs are the visible project projection, known sections and local-folder availability.
pub fn project_menu(
    project: &ProjectSummary,
    sections: &[String],
    local: bool,
    folder_label: &str,
) -> Vec<ProjectMenuItem> {
    let id = &project.id;
    let mut section_items = vec![ProjectMenuItem::Action {
        title: "未分類".into(),
        checked: project.section.is_none(),
        enabled: true,
        action: ProjectAction::Dispatch(Box::new(Intent::SetProjectSection {
            project_id: id.clone(),
            section: String::new(),
        })),
    }];
    for section in sections.iter().collect::<std::collections::BTreeSet<_>>() {
        section_items.push(ProjectMenuItem::Action {
            title: section.clone(),
            checked: project.section.as_ref() == Some(section),
            enabled: true,
            action: ProjectAction::Dispatch(Box::new(Intent::SetProjectSection {
                project_id: id.clone(),
                section: section.clone(),
            })),
        });
    }
    section_items.push(item("新しいセクション…", ProjectAction::Edit(id.clone())));
    let folders = project
        .roots
        .iter()
        .map(|root| ProjectMenuItem::Action {
            title: root.path.clone(),
            checked: false,
            enabled: local,
            action: ProjectAction::OpenFolder(root.path.clone()),
        })
        .collect::<Vec<_>>();
    let folder_menu = match &folders[..] {
        [ProjectMenuItem::Action { action, .. }] => ProjectMenuItem::Action {
            title: folder_label.into(),
            action: action.clone(),
            checked: false,
            enabled: local,
        },
        _ => ProjectMenuItem::Submenu {
            title: folder_label.into(),
            items: folders,
        },
    };
    let mut items = vec![
        item(
            if project.pinned {
                "ピン留めを解除"
            } else {
                "ピン留め"
            },
            ProjectAction::Dispatch(Box::new(Intent::SetProjectPinned {
                project_id: id.clone(),
                pinned: !project.pinned,
            })),
        ),
        item("編集", ProjectAction::Edit(id.clone())),
        ProjectMenuItem::Separator,
        ProjectMenuItem::Submenu {
            title: "セクション".into(),
            items: section_items,
        },
        folder_menu,
        ProjectMenuItem::Separator,
        item("チャットをアーカイブ", ProjectAction::Archive(id.clone())),
    ];
    if project.chats_archived {
        items.push(item(
            "アーカイブしたチャットを戻す",
            ProjectAction::Dispatch(Box::new(Intent::RestoreProjectChats {
                project_id: id.clone(),
            })),
        ));
    }
    items.push(ProjectMenuItem::Separator);
    items.push(item(
        "プロジェクトを一覧から削除",
        ProjectAction::Dispatch(Box::new(Intent::SetProjectHidden {
            project_id: id.clone(),
            hidden: true,
        })),
    ));
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectSummary {
        ProjectSummary {
            host_id: "host".into(),
            id: "project".into(),
            name: "日本語のプロジェクト".into(),
            coordinator_url: None,
            roots: vec![agent_protocol::models::ProjectRoot {
                path: "/fixture/日本語".into(),
            }],
            icon: None,
            monogram: "PR".into(),
            icon_color: 0,
            pinned: false,
            section: Some("B".into()),
            chats_archived: false,
            expanded: true,
            error: None,
            threads: Vec::new(),
            has_more: false,
        }
    }

    #[test]
    fn project_actions_preserve_sections_local_folder_restrictions_and_reversible_archive() {
        let mut project = project();
        let sections = ["B".into(), "A".into(), "B".into()];
        let menu = project_menu(&project, &sections, false, "フォルダを開く");
        assert!(
            matches!(&menu[0], ProjectMenuItem::Action { title, action: ProjectAction::Dispatch(intent), .. }
            if title == "ピン留め" && matches!(**intent, Intent::SetProjectPinned { pinned: true, .. }))
        );
        let ProjectMenuItem::Submenu { title, items } = &menu[3] else {
            panic!("section menu");
        };
        assert_eq!(title, "セクション");
        let titles = items
            .iter()
            .map(|entry| match entry {
                ProjectMenuItem::Action { title, .. } => title.as_str(),
                _ => panic!("section action"),
            })
            .collect::<Vec<_>>();
        assert_eq!(titles, ["未分類", "A", "B", "新しいセクション…"]);
        assert!(matches!(
            &items[2],
            ProjectMenuItem::Action { checked: true, .. }
        ));
        assert!(matches!(
            &items[0],
            ProjectMenuItem::Action { checked: false, .. }
        ));
        assert!(
            matches!(&menu[4], ProjectMenuItem::Action { action: ProjectAction::OpenFolder(path), enabled: false, .. } if path == "/fixture/日本語")
        );
        assert!(!menu.iter().any(|item| matches!(item, ProjectMenuItem::Action { title, .. } if title == "アーカイブしたチャットを戻す")));
        project.pinned = true;
        project.chats_archived = true;
        project.roots.push(agent_protocol::models::ProjectRoot {
            path: "/fixture/other".into(),
        });
        let menu = project_menu(&project, &sections, true, "フォルダを開く");
        assert!(
            matches!(&menu[0], ProjectMenuItem::Action { title, action: ProjectAction::Dispatch(intent), .. }
            if title == "ピン留めを解除" && matches!(**intent, Intent::SetProjectPinned { pinned: false, .. }))
        );
        assert!(menu.iter().any(|item| matches!(item, ProjectMenuItem::Action { action: ProjectAction::Dispatch(intent), .. } if matches!(&**intent, Intent::RestoreProjectChats { project_id } if project_id == "project"))));
        let ProjectMenuItem::Submenu { items, .. } = &menu[4] else {
            panic!("folders keep separate actions");
        };
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|entry| matches!(entry, ProjectMenuItem::Action { enabled: true, .. }))
        );
    }

    #[test]
    fn archive_uses_action_time_and_editor_and_folder_actions_stay_native() {
        for now in [10, 90] {
            assert!(
                matches!(ProjectAction::Archive("project".into()).into_intent(now), Ok(Intent::ArchiveProjectChats { project_id, before }) if project_id == "project" && before == now)
            );
        }
        assert!(
            matches!(ProjectAction::Edit("project".into()).into_intent(10), Err(ProjectAction::Edit(id)) if id == "project")
        );
        assert!(
            matches!(ProjectAction::OpenFolder("/fixture".into()).into_intent(10), Err(ProjectAction::OpenFolder(path)) if path == "/fixture")
        );
    }
}
