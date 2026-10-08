use super::*;

fn hello_world() -> ArtifactTemplate {
    ArtifactTemplate {
        artifact_kind: ArtifactTemplateKind::Document,
        display_name: "Hello World".into(),
        gallery_kind: None,
        skill_directory: "/Users/test/.codex/skills/artifact-template-hello-world".into(),
        skill_name: "artifact-template-hello-world".into(),
    }
}

fn attributes(pairs: &[(&str, &str)]) -> DirectiveAttributes {
    pairs
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect()
}

#[test]
fn shares_labels_and_copy_text_across_clients() {
    assert_eq!(
        artifact_template_presentation_label(ArtifactTemplateKind::Document),
        "Document template"
    );
}

#[test]
fn accepts_the_template_metadata_emitted_by_codex() {
    assert_eq!(
        artifact_template(&attributes(&[
            ("artifact_kind", "document"),
            ("display_name", "  Hello World  "),
            (
                "skill_directory",
                "/Users/test/.codex/skills/artifact-template-hello-world"
            ),
            ("skill_name", "artifact-template-hello-world"),
        ])),
        Some(hello_world())
    );
}

#[test]
fn accepts_absolute_windows_skill_directories() {
    for skill_directory in [
        r"C:\Users\test\.codex\skills\artifact-template-hello-world",
        r"\\server\share\artifact-template-hello-world",
        "//server/share/artifact-template-hello-world",
    ] {
        let template = artifact_template(&attributes(&[
            ("artifact_kind", "image"),
            ("display_name", "Reference image"),
            ("gallery_kind", "product-design"),
            ("skill_directory", skill_directory),
            ("skill_name", "artifact-template-reference-image"),
        ]))
        .unwrap_or_else(|| panic!("{skill_directory}"));
        assert_eq!(template.artifact_kind, ArtifactTemplateKind::Image);
        assert_eq!(
            template.gallery_kind,
            Some(ArtifactTemplateGalleryKind::ProductDesign)
        );
        assert_eq!(template.skill_directory, skill_directory);
    }
}

#[test]
fn rejects_malformed_template_metadata() {
    // A directive attribute without a value reads as an empty string, the closest
    // counterpart of the reference's `gallery_kind: null`.
    for (name, value) in [
        ("artifact_kind", "unknown"),
        ("display_name", " "),
        ("skill_directory", "relative/template"),
        ("skill_name", "hello-world"),
        ("gallery_kind", ""),
        ("gallery_kind", "unknown"),
    ] {
        let mut metadata = attributes(&[
            ("artifact_kind", "document"),
            ("display_name", "Hello World"),
            ("skill_directory", "/templates/hello-world"),
            ("skill_name", "artifact-template-hello-world"),
        ]);
        metadata.insert(name.into(), value.into());
        assert_eq!(artifact_template(&metadata), None, "{name}={value:?}");
    }
}

#[test]
fn builds_the_same_document_follow_up_shape_as_codex() {
    assert_eq!(
        artifact_template_use_prompt(&hello_world()),
        "Create a document using this $artifact-template-hello-world about…"
    );
}

#[test]
fn uses_the_artifact_specific_image_wording() {
    assert_eq!(
        artifact_template_use_prompt(&ArtifactTemplate {
            artifact_kind: ArtifactTemplateKind::Image,
            ..hello_world()
        }),
        "Create an image using this $artifact-template-hello-world of…"
    );
}

const PROMPT: &str = "Create a document using this $artifact-template-hello-world about…";

#[test]
fn adds_the_prompt_to_an_empty_draft() {
    assert_eq!(
        append_artifact_template_use_prompt(String::new(), hello_world()),
        PROMPT
    );
}

#[test]
fn preserves_existing_draft_text() {
    assert_eq!(
        append_artifact_template_use_prompt("Write about otters".into(), hello_world()),
        format!("Write about otters {PROMPT}")
    );
}

#[test]
fn does_not_append_the_same_final_prompt_twice() {
    for draft in [
        PROMPT.to_owned(),
        format!("{PROMPT}\n"),
        format!("Notes\n\n{PROMPT}"),
    ] {
        assert_eq!(
            append_artifact_template_use_prompt(draft.clone(), hello_world()),
            draft
        );
    }
}

#[test]
fn does_not_mistake_text_containing_the_prompt_for_a_final_prompt() {
    let draft = format!("{PROMPT}\nAdditional instructions");
    assert_eq!(
        append_artifact_template_use_prompt(draft.clone(), hello_world()),
        format!("{draft} {PROMPT}")
    );
}
