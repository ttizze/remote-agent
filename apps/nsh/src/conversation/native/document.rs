//! File renderings use the transcript's native text, links and media controls.
use super::*;

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
) -> impl IntoElement {
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
