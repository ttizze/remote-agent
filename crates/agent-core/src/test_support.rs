use orchestration::*;
pub fn now() -> Timestamp {
    Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
}
pub fn projection() -> ThreadProjection {
    let id = ThreadId::new("thread").unwrap();
    ThreadProjection::empty(AppThread {
        created_by: CreatedBy::User,
        creation_source: CreationSource::Desktop,
        id: id.clone(),
        project_id: ProjectId::new("project").unwrap(),
        title: "Build Bex".into(),
        provider_instance_id: ProviderInstanceId::new("codex").unwrap(),
        model_selection: ModelSelection {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            model: "model".into(),
            options: Default::default(),
        },
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: None,
        active_provider_thread_id: None,
        lineage: Lineage {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: id,
        },
        forked_from: None,
        created_at: now(),
        updated_at: now(),
        archived_at: None,
        settled_override: None,
        settled_at: None,
        unsettled_at: None,
        snoozed_until: None,
        snoozed_at: None,
        pinned_at: None,
        auto_settle_disabled_at: None,
        pin_order_key: None,
        active_order_key: None,
        last_visited_at: None,
        deleted_at: None,
        imported: false,
    })
}
pub fn event(id: &str, payload: EventPayload) -> DomainEvent {
    DomainEvent {
        id: EventId::new(id).unwrap(),
        thread_id: ThreadId::new("thread").unwrap(),
        occurred_at: now(),
        payload,
    }
}
pub fn item(id: &str, ordinal: u64, body: TurnItemBody) -> TurnItem {
    TurnItem {
        id: TurnItemId::new(id).unwrap(),
        thread_id: ThreadId::new("thread").unwrap(),
        run_id: Some(RunId::new("run").unwrap()),
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: ItemStatus::Completed,
        title: None,
        started_at: Some(now()),
        completed_at: Some(now()),
        updated_at: now(),
        body,
    }
}
