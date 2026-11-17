//! The provider input boundary shared by submission, recovery and queued edits.
use crate::state::{Attachment, Draft, append_transcript};
use agent_protocol::{
    composer::{Invocation, InvocationKind},
    operations::Input,
};

/// New references follow the selected provider/text. A queued message's
/// existing parts remain selected until the editor explicitly removes them.
pub(crate) fn selected_invocations<'a>(
    invocations: &'a [Invocation],
    text: &'a str,
    provider: Option<crate::session::ProviderKind>,
    retained: &'a [Invocation],
) -> impl Iterator<Item = &'a Invocation> {
    invocations.iter().filter(move |item| {
        retained.contains(item) || Some(item.provider) == provider && item.is_in(text)
    })
}

/// Text edits prune new references; persisted queued parts need an explicit
/// removal. Insertion replaces the same token without consuming other parts.
pub(crate) fn edited_invocations(
    current: &[Invocation],
    text: &str,
    retained: &[Invocation],
    insert: Option<Invocation>,
) -> Vec<Invocation> {
    let token = insert.as_ref().map(Invocation::token);
    current
        .iter()
        .filter(|item| {
            token.as_ref().is_none_or(|token| &item.token() != token)
                && (retained.contains(item) || item.is_in(text))
        })
        .cloned()
        .chain(insert)
        .collect()
}

pub(crate) fn to_input<'a>(
    text: &str,
    attachments: &[Attachment],
    invocations: impl Iterator<Item = &'a Invocation>,
    contexts: &[agent_protocol::context::MessageContext],
) -> Vec<Input> {
    let mut input = Vec::with_capacity(attachments.len() + usize::from(!text.is_empty()));
    if !text.is_empty() {
        input.push(Input::Text { text: text.into() });
    }
    input.extend(invocations.map(Invocation::input));
    input.extend(attachments.iter().map(|attachment| {
        if attachment.is_image {
            Input::LocalImage {
                path: attachment.path.clone(),
            }
        } else {
            Input::Mention {
                path: attachment.path.clone(),
                name: attachment.name.clone(),
            }
        }
    }));
    input.extend(contexts.iter().cloned().map(|context| Input::Context {
        context: Box::new(context),
    }));
    input
}

pub(crate) fn from_input(
    input: &[Input],
    provider: Option<crate::session::ProviderKind>,
    fresh_context: bool,
) -> Option<Draft> {
    let mut draft = Draft {
        fresh_context,
        ..Default::default()
    };
    for part in input {
        match part {
            Input::Context { context } => draft.contexts.push(context.as_ref().clone()),
            Input::Text { text } => append_transcript(&mut draft.text, text),
            Input::Skill { name, path } => draft.invocations.push(Invocation {
                provider: provider?,
                kind: InvocationKind::Skill,
                name: name.clone(),
                path: path.clone(),
            }),
            Input::Mention { name, path } if agent_protocol::composer::is_plugin_path(path) => {
                draft.invocations.push(Invocation {
                    provider: provider?,
                    kind: InvocationKind::Plugin,
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            Input::Mention { name, path } => draft.attachments.push(Attachment {
                path: path.clone(),
                name: name.clone(),
                is_image: false,
            }),
            Input::LocalImage { path } => draft.attachments.push(Attachment {
                name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned(),
                path: path.clone(),
                is_image: true,
            }),
        }
    }
    Some(draft)
}

/// Restore canonical input using the same attachment/context decoding as the
/// conversation view. Original attachment names survive a retry or reconnect.
pub(crate) fn from_body(
    body: &agent_protocol::orchestration::TurnItemBody,
    provider: crate::session::ProviderKind,
    fresh_context: bool,
) -> Result<Draft, String> {
    let agent_protocol::orchestration::TurnItemBody::UserMessage {
        text,
        attachments,
        context,
        ..
    } = body
    else {
        return Err("saved input is not a user message".into());
    };
    let context = crate::orchestration::context_input(context.as_ref());
    let mut draft = from_input(&context, Some(provider), fresh_context)
        .expect("the provider was resolved before restoring input");
    draft.text = text.clone();
    let mut attachments = crate::orchestration::attachments(attachments)?;
    attachments.append(&mut draft.attachments);
    draft.attachments = attachments;
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_recovery_keeps_original_names_and_typed_context() {
        let context = agent_protocol::context::MessageContext {
            id: "reference".into(),
            label: "Source".into(),
            content: agent_protocol::context::ContextContent::File {
                path: "/project/source.rs".into(),
            },
        };
        let body = agent_protocol::orchestration::TurnItemBody::UserMessage {
            created_by: agent_protocol::orchestration::Actor::User,
            creation_source: agent_protocol::orchestration::CreationSource::Mobile,
            scheduled_task_id: None,
            message_id: "message".into(),
            input_intent: "queue".into(),
            text: "Restore exactly".into(),
            attachments: vec![
                serde_json::json!({"id":"image-id", "type":"image", "name":"Original photo.png", "mimeType":"image/png"}),
            ],
            context: Some(serde_json::json!({"version":1,"records":[context.record()]})),
        };
        let draft = from_body(&body, crate::session::ProviderKind::Codex, true).unwrap();
        assert_eq!(draft.text, "Restore exactly");
        assert!(draft.fresh_context);
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.attachments[0].name, "Original photo.png");
        assert!(draft.attachments[0].is_image);
        assert_eq!(draft.contexts, [context]);
    }
}
