//! File renderings use the transcript's native text, links and media controls.
use super::*;

pub(crate) type TaskPresses = (Memo<bool>, std::rc::Rc<dyn Fn(u32, bool)>);

pub(crate) fn view(
    surroundings: crate::rich_text::Surroundings,
    path: String,
    source: impl Fn() -> Option<String> + 'static,
    blocks: impl Fn(
        String,
        String,
        Option<String>,
    ) -> Vec<agent_core::presentation::markdown::MarkdownBlock>
    + 'static,
    maximum: f32,
    tasks: Option<TaskPresses>,
) -> impl IntoElement {
    let (tasks_enabled, press_task) = tasks.unzip();
    let connection = surroundings.connection;
    let target = crate::rich_text::target(connection);
    let scope = computed(move || json!([path.as_str(), target.get()]).to_string());
    let chips = computed(move || crate::file_actions::Chips::read(connection));
    let projected = computed(move || {
        let text = source().unwrap_or_default();
        let (cwd, home) = connection
            .publication
            .with(|p| (p.context.navigation().cwd, p.context.home()));
        let blocks = blocks(text.clone(), cwd, home);
        let blocks = if tasks_enabled.is_some_and(|enabled| enabled.get()) {
            link_tasks(blocks)
        } else {
            blocks
        };
        let paths = crate::rich_text::file_paths(&blocks);
        let parts = chips.with(|chips| {
            crate::rich_text::indexed(crate::rich_text::parts(
                blocks,
                &crate::rich_text::diagrams(&text),
                &|file| Some(chips.chip(file)),
            ))
        });
        vec![
            json!({"id":"document","user":false,"gap":0,"top":20,"buttons":[],"parts":parts,"images":[],"media":{},"filePaths":paths,"time":null,"queued":false,"steer":false,"document":document_metadata(&target.get(), connection.pane.is_none(), false, false, None)}),
        ]
    });
    effect(move || {
        let paths = projected.with(|rows| {
            rows[0]["filePaths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        crate::file_actions::read_metadata(connection, paths);
    });
    let retry = signal(0_u64);
    let media = image_resources(connection, scope, projected, retry);
    native("bex.conversation")
        .prop("data", move || {
            let mut rows = projected.get();
            media.with(|media| attach_images(&mut rows[0], media));
            json!({"scope":scope.get(),"rows":rows,"following":false,"inset":0,"history":null,"notice":null,"opening":null,"latest":"","lane":maximum,"padding":24})
        })
        .on_native_event(move |event: &NativeEvent| {
            if event.data["scope"].as_str() != Some(scope.get_untracked().as_str()) {
                return;
            }
            if event.data["action"] == "link"
                && let Some(link) = event.data["text"].as_str().filter(|link| link.starts_with("bex-task:")) {
                if tasks_enabled.is_some_and(|enabled| enabled.get_untracked())
                    && let Some((index, checked)) = task_request(link)
                    && let Some(press) = &press_task
                {
                    press(index, checked);
                }
                return;
            }
            if event.data["action"] == "image" {
                if let Some(source) = event.data["text"].as_str() {
                    surroundings.preview.load_image(connection, source.to_owned(), None);
                }
            } else if !media_action(&event.data, media, retry) {
                crate::rich_text::action(surroundings, &event.data);
            }
        })
        .flex_1().min_h_0()
}

// Native text already reports links on every platform. Only file documents
// receive these marker links; conversation checkboxes remain plain text.
fn link_tasks(
    blocks: Vec<agent_core::presentation::markdown::MarkdownBlock>,
) -> Vec<agent_core::presentation::markdown::MarkdownBlock> {
    use agent_core::presentation::markdown::{MarkdownBlock, MarkdownRun};

    fn visit(blocks: Vec<MarkdownBlock>, next: &mut u32) -> Vec<MarkdownBlock> {
        let mut linked = Vec::new();
        for mut block in blocks {
            let style = match &mut block {
                MarkdownBlock::Paragraph { style, .. }
                | MarkdownBlock::Table { style, .. }
                | MarkdownBlock::Visualization { style, .. }
                | MarkdownBlock::Disclosure { style, .. }
                | MarkdownBlock::Callout { style, .. } => style,
            };
            if let Some(checked) = match style.marker.as_deref() {
                Some("☐") => Some(true),
                Some("☑") => Some(false),
                _ => None,
            } {
                let run = MarkdownRun {
                    text: format!("{} ", style.marker.take().unwrap()),
                    link: Some(format!("bex-task:{next}/{checked}")),
                    ..Default::default()
                };
                let style = style.clone();
                if let MarkdownBlock::Paragraph { runs, .. } = &mut block {
                    runs.insert(0, run);
                } else {
                    // A visualization has no text run to hold its box's link.
                    linked.push(MarkdownBlock::Paragraph {
                        runs: vec![run],
                        style,
                    });
                }
                *next += 1;
            }
            match &mut block {
                MarkdownBlock::Disclosure { blocks, .. }
                | MarkdownBlock::Callout { blocks, .. } => {
                    *blocks = visit(std::mem::take(blocks), next);
                }
                _ => {}
            }
            linked.push(block);
        }
        linked
    }
    visit(blocks, &mut 0)
}

fn task_request(link: &str) -> Option<(u32, bool)> {
    let (index, checked) = link.strip_prefix("bex-task:")?.split_once('/')?;
    Some((index.parse().ok()?, checked.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::presentation::{files::file_markdown_blocks, markdown::markdown_runs};

    #[test]
    fn file_markers_link_to_the_same_tasks_the_source_edits() {
        let source = "- [ ] first\n  - [x] nested\n\n> - [ ] quoted\n\n> [!NOTE]\n> - [ ] note\n\n<details>\n<summary>more</summary>\n\n- [x] hidden\n\n</details>\n";
        let blocks = link_tasks(file_markdown_blocks(source, "/a.md", "/".into(), None));
        let links: Vec<_> = markdown_runs(&blocks)
            .into_iter()
            .filter_map(|run| {
                run.link
                    .as_deref()
                    .and_then(task_request)
                    .map(|request| (run.text.as_str(), request))
            })
            .collect();
        assert_eq!(
            links,
            [
                ("☐ ", (0, true)),
                ("☑ ", (1, false)),
                ("☐ ", (2, true)),
                ("☐ ", (3, true)),
                ("☑ ", (4, false))
            ]
        );
    }

    #[test]
    fn a_visualizations_box_keeps_later_task_links_on_their_own_items() {
        use agent_core::presentation::{
            files::file_markdown_task_checked, markdown::MarkdownBlock,
        };

        let source = "- [ ] visualize{\"path\":\"/plot.html\"}\n- [ ] ordinary\n";
        let blocks = link_tasks(file_markdown_blocks(source, "/a.md", "/".into(), None));
        let links: Vec<_> = markdown_runs(&blocks)
            .into_iter()
            .filter_map(|run| run.link.as_deref().and_then(task_request))
            .collect();
        assert_eq!(links, [(0, true), (1, true)]);
        assert!(
            matches!(&blocks[1], MarkdownBlock::Visualization { path, style }
            if path == "/plot.html" && style.marker.is_none())
        );
        let (index, checked) = links[1];
        assert_eq!(
            file_markdown_task_checked(source, index, checked),
            Some(source.replace("- [ ] ordinary", "- [x] ordinary"))
        );
    }

    #[test]
    fn task_links_require_an_index_and_boolean_state() {
        for link in [
            "a.md",
            "bex-task:",
            "bex-task:-1/true",
            "bex-task:0/yes",
            "bex-task:0/true/extra",
        ] {
            assert_eq!(task_request(link), None);
        }
        assert_eq!(task_request("bex-task:12/false"), Some((12, false)));
    }
}
