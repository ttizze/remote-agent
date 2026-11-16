use super::*;

/// The Host projects committed image results, including results that arrive
/// after the foreground ends. No new native message is sent.
pub fn project_generated_images(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    provider_turn_id: &str,
    image_ids: &[String],
    now: &str,
) -> Result<Decision, String> {
    let Some(attempt) = state
        .owned_attempt(attempt_id, session_id)
        .filter(|attempt| {
            matches!(
                attempt.status,
                RunAttemptStatus::Running | RunAttemptStatus::Completed
            )
        })
    else {
        return Ok(Decision::default());
    };
    let belongs = |item: &&TurnItem| {
        item.run_id.as_ref() == Some(&attempt.run_id)
            && item.provider_thread_id.as_ref() == Some(&attempt.provider_thread_id)
            && item.provider_turn_id.as_deref() == Some(provider_turn_id)
    };
    let images: Vec<_> = image_ids
        .iter()
        .filter_map(|id| state.turn_items.get(id))
        .filter(belongs)
        .filter(|item| item.status == TurnItemStatus::Completed)
        .filter_map(|item| match &item.body {
            TurnItemBody::DynamicTool {
                tool_name: Some(name),
                output: Some(output),
                ..
            } if name == "image_gen.imagegen"
                && output.get("failure").is_none_or(Value::is_null) =>
            {
                output
                    .get("savedPath")
                    .and_then(Value::as_str)
                    .map(|path| (item, path))
            }
            _ => None,
        })
        .collect();
    let Some((first, _)) = images.first() else {
        return Ok(Decision::default());
    };
    let turn = state
        .provider_turns
        .get(provider_turn_id)
        .filter(|turn| turn.run_attempt_id.as_deref() == Some(attempt_id))
        .ok_or("generated images do not belong to this provider turn")?;
    let existing = state.turn_items.values().filter(belongs)
        .filter(|item|matches!(&item.body,TurnItemBody::AssistantMessage {phase:Some(phase),..} if phase=="final_answer"))
        .max_by_key(|item|item.ordinal);
    let previous = existing
        .and_then(|item| match &item.body {
            TurnItemBody::AssistantMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    let paths: Vec<_> = images.iter().map(|(_, path)| *path).collect();
    let text = crate::markdown::append_generated_image_embeds(previous, &paths);
    if text.as_ref() == previous {
        return Ok(Decision::default());
    }
    let thread = state
        .provider_threads
        .get(&attempt.provider_thread_id)
        .ok_or("image provider thread is missing")?;
    let source_id = first
        .native_item_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_deref())
        .unwrap_or(&first.id);
    let native = format!("generated-image-message:{source_id}");
    let item_id = existing.map(|item| item.id.clone()).unwrap_or_else(|| {
        derived_id(
            "turn-item",
            &["provider", &thread.driver, "native-item", &native],
        )
    });
    let node_id = existing
        .and_then(|item| item.node_id.clone())
        .unwrap_or_else(|| {
            derived_id(
                "node",
                &["provider", &thread.driver, "native-item", &native],
            )
        });
    let native_ref = existing
        .and_then(|item| item.native_item_ref.clone())
        .or_else(|| {
            first.native_item_ref.as_ref().map(|reference| ProviderRef {
                native_id: Some(native.clone()),
                ..reference.clone()
            })
        });
    let mut body =
        existing
            .map(|item| item.body.clone())
            .unwrap_or_else(|| TurnItemBody::AssistantMessage {
                message_id: derived_id(
                    "message",
                    &["provider", &thread.driver, "native-item", &native],
                ),
                text: String::new(),
                phase: Some("final_answer".into()),
                attachments: vec![],
                streaming: false,
            });
    if let TurnItemBody::AssistantMessage {
        text: value,
        streaming,
        ..
    } = &mut body
    {
        *value = text.into_owned();
        *streaming = false;
    }
    let item = TurnItem {
        detail_omitted: false,
        id: item_id,
        thread_id: state.thread.id.clone(),
        run_id: Some(attempt.run_id.clone()),
        node_id: Some(node_id.clone()),
        provider_thread_id: Some(thread.id.clone()),
        provider_turn_id: Some(turn.id.clone()),
        native_item_ref: native_ref.clone(),
        parent_item_id: existing.and_then(|item| item.parent_item_id.clone()),
        ordinal: existing.map_or(0, |item| item.ordinal),
        status: TurnItemStatus::Completed,
        title: existing.and_then(|item| item.title.clone()),
        started_at: existing
            .and_then(|item| item.started_at.clone())
            .or_else(|| Some(now.into())),
        completed_at: Some(now.into()),
        updated_at: now.into(),
        body,
    };
    let node = ExecutionNode {
        id: node_id.clone(),
        thread_id: state.thread.id.clone(),
        run_id: Some(attempt.run_id.clone()),
        parent_node_id: Some(attempt.root_node_id.clone()),
        root_node_id: attempt.root_node_id.clone(),
        kind: NodeKind::AssistantMessage,
        status: NodeStatus::Completed,
        counts_for_run: false,
        provider_thread_id: Some(thread.id.clone()),
        provider_turn_id: Some(turn.id.clone()),
        native_item_ref: native_ref,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: state
            .nodes
            .get(&node_id)
            .and_then(|node| node.started_at.clone())
            .or_else(|| Some(now.into())),
        completed_at: Some(now.into()),
    };
    Ok(Decision {
        changes: vec![Change::Node(node), Change::TurnItem(item)],
        ..Default::default()
    })
}
