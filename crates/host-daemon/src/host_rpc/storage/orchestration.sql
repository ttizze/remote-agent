-- T3 Code 1a12c4af: current schema after migrations 1..91.
-- Approved differences: no legacy/unused projections, message
-- bodies live in turn items, foreign keys and lifecycle checks are enforced.
-- Cyclic run/item/attempt references commit together, so their keys are deferred.

CREATE TABLE projection_projects (
    project_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    workspace_root TEXT NOT NULL,
    scripts_json TEXT NOT NULL CHECK (json_valid(scripts_json) AND json_type(scripts_json)='array'),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT,
    default_model_selection_json TEXT,
    default_thread_env_mode TEXT,
    favicon_path TEXT,
    auto_pull INTEGER NOT NULL DEFAULT 0,
    project_icon_json TEXT,
    pinned_at TEXT,
    archived_at TEXT,
    has_custom_title INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE orchestration_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_kind TEXT NOT NULL,
    stream_id TEXT NOT NULL,
    stream_version INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    command_id TEXT,
    causation_event_id TEXT,
    correlation_id TEXT,
    actor_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    metadata_json TEXT NOT NULL,
    application_event_version INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE orchestration_command_receipts (
    command_id TEXT PRIMARY KEY,
    aggregate_kind TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    result_sequence INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('accepted','rejected')),
    error TEXT,
    command_type TEXT NOT NULL DEFAULT 'legacy'
) STRICT;

CREATE TABLE orchestration_v2_effect_outbox (
    effect_id TEXT PRIMARY KEY,
    command_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    effect_type TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json) AND json_extract(payload_json,'$.type') IS effect_type),
    status TEXT NOT NULL CHECK (
      status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')
    ),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_expires_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    last_error TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE orchestration_v2_projection_metadata (
    projection_name TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY,
    -- Bex supports chats outside a registered project. No synthetic project is created.
    project_id TEXT,
    title TEXT NOT NULL,
    default_provider TEXT NOT NULL,
    runtime_mode TEXT NOT NULL,
    interaction_mode TEXT NOT NULL,
    active_provider_thread_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    deleted_at TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    provider_instance_id TEXT,
    FOREIGN KEY (project_id) REFERENCES projection_projects (project_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, active_provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (thread_id, provider_thread_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE orchestration_v2_projection_runs (
    run_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal>0),
    provider TEXT NOT NULL,
    provider_thread_id TEXT,
    status TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    completed_at TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    provider_instance_id TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (thread_id, provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('preparing', 'queued', 'starting', 'running', 'waiting', 'completed', 'interrupted', 'failed', 'cancelled', 'rolled_back'))
    ,
    UNIQUE (thread_id, run_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_run_attempts (
    attempt_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_ordinal INTEGER NOT NULL,
    root_node_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_thread_id TEXT NOT NULL,
    provider_turn_id TEXT,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    provider_instance_id TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, root_node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (thread_id, provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_turn_id) REFERENCES orchestration_v2_projection_provider_turns (thread_id, provider_turn_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('pending', 'running', 'completed', 'interrupted', 'failed', 'cancelled', 'superseded'))
    ,
    UNIQUE (thread_id, attempt_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_nodes (
    node_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT,
    parent_node_id TEXT,
    root_node_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('root_turn','assistant_message','reasoning','plan','todo_list','tool_call','approval_request','user_input_request','subagent','hook','system')),
    status TEXT NOT NULL CHECK (status IN ('idle','pending','running','waiting','completed','interrupted','failed','cancelled','rolled_back')),
    provider_thread_id TEXT,
    provider_turn_id TEXT,
    runtime_request_id TEXT,
    checkpoint_scope_id TEXT,
    started_at TEXT,
    completed_at TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    UNIQUE (thread_id,node_id),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads(thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id,run_id) REFERENCES orchestration_v2_projection_runs(thread_id,run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (parent_node_id) REFERENCES orchestration_v2_projection_nodes(node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (root_node_id) REFERENCES orchestration_v2_projection_nodes(node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads(provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_turn_id) REFERENCES orchestration_v2_projection_provider_turns(provider_turn_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (runtime_request_id) REFERENCES orchestration_v2_projection_runtime_requests(runtime_request_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (checkpoint_scope_id) REFERENCES orchestration_v2_projection_checkpoint_scopes(scope_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE INDEX orchestration_v2_projection_nodes_thread_run_idx ON orchestration_v2_projection_nodes(thread_id,run_id);
CREATE INDEX orchestration_v2_projection_nodes_parent_idx ON orchestration_v2_projection_nodes(parent_node_id);
CREATE INDEX orchestration_v2_projection_nodes_provider_turn_idx ON orchestration_v2_projection_nodes(provider_turn_id);

CREATE TABLE orchestration_v2_projection_provider_sessions (
    provider_session_id TEXT PRIMARY KEY,
    thread_id TEXT,
    provider TEXT NOT NULL,
    status TEXT NOT NULL,
    model TEXT,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    driver TEXT,
    provider_instance_id TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads(thread_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('starting', 'ready', 'running', 'waiting', 'stopped', 'error'))
) STRICT;

CREATE TABLE orchestration_v2_projection_provider_session_bindings (
    provider_session_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    PRIMARY KEY (provider_session_id, thread_id),
    FOREIGN KEY (provider_session_id) REFERENCES orchestration_v2_projection_provider_sessions (provider_session_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE orchestration_v2_projection_provider_threads (
    provider_thread_id TEXT PRIMARY KEY,
    thread_id TEXT,
    owner_node_id TEXT,
    provider TEXT NOT NULL,
    provider_session_id TEXT,
    status TEXT NOT NULL,
    first_run_ordinal INTEGER,
    last_run_ordinal INTEGER,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    driver TEXT,
    provider_instance_id TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_session_id) REFERENCES orchestration_v2_projection_provider_sessions (provider_session_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (owner_node_id) REFERENCES orchestration_v2_projection_nodes (node_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('unknown', 'not_loaded', 'idle', 'active', 'archived', 'closed', 'error'))
    ,
    UNIQUE (thread_id, provider_thread_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_provider_turns (
    provider_turn_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    provider_thread_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    run_attempt_id TEXT,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (thread_id, provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_attempt_id) REFERENCES orchestration_v2_projection_run_attempts (thread_id, attempt_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('pending', 'running', 'completed', 'interrupted', 'failed', 'cancelled'))
    ,
    UNIQUE (thread_id, provider_turn_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_turn_items (
    turn_item_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT,
    node_id TEXT,
    provider_thread_id TEXT,
    provider_turn_id TEXT,
    parent_item_id TEXT,
    ordinal INTEGER NOT NULL,
    type TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('idle','pending','running','waiting','completed','failed','cancelled','interrupted')),
    input_intent TEXT,
    created_by TEXT,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, parent_item_id) REFERENCES orchestration_v2_projection_turn_items (thread_id, turn_item_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_turn_id) REFERENCES orchestration_v2_projection_provider_turns (thread_id, provider_turn_id) DEFERRABLE INITIALLY DEFERRED
    ,
    UNIQUE (thread_id, turn_item_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_runtime_requests (
    runtime_request_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    provider_turn_id TEXT,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, provider_turn_id) REFERENCES orchestration_v2_projection_provider_turns (thread_id, provider_turn_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('pending', 'resolved', 'expired', 'cancelled'))
    ,
    UNIQUE (thread_id, runtime_request_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_subagents (
    subagent_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT,
    parent_node_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_thread_id TEXT,
    child_thread_id TEXT,
    origin TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    driver TEXT,
    provider_instance_id TEXT,
    completion_delivery_state TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (parent_node_id) REFERENCES orchestration_v2_projection_nodes (node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (child_thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('idle', 'pending', 'running', 'waiting', 'completed', 'failed', 'cancelled', 'interrupted'))
    ,
    UNIQUE (thread_id, subagent_id)
) STRICT;

CREATE INDEX orchestration_v2_projection_subagents_child ON orchestration_v2_projection_subagents (child_thread_id);

CREATE TABLE orchestration_v2_projection_plans (
    plan_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT,
    node_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('draft', 'active', 'completed', 'superseded'))
    ,
    UNIQUE (thread_id, plan_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_checkpoint_scopes (
    scope_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    run_id TEXT,
    node_id TEXT NOT NULL,
    parent_scope_id TEXT,
    provider_thread_id TEXT,
    kind TEXT NOT NULL,
    ordinal_within_parent INTEGER NOT NULL,
    advances_app_run_count INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, parent_scope_id) REFERENCES orchestration_v2_projection_checkpoint_scopes (thread_id, scope_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (provider_thread_id) DEFERRABLE INITIALLY DEFERRED
    ,
    UNIQUE (thread_id, scope_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    run_id TEXT,
    node_id TEXT NOT NULL,
    parent_checkpoint_id TEXT,
    ordinal_within_scope INTEGER NOT NULL,
    app_run_ordinal INTEGER,
    status TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, scope_id) REFERENCES orchestration_v2_projection_checkpoint_scopes (thread_id, scope_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, node_id) REFERENCES orchestration_v2_projection_nodes (thread_id, node_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, parent_checkpoint_id) REFERENCES orchestration_v2_projection_checkpoints (thread_id, checkpoint_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('ready', 'missing', 'error', 'stale'))
    ,
    UNIQUE (thread_id, checkpoint_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_context_handoffs (
    context_handoff_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    target_run_id TEXT NOT NULL,
    to_provider_thread_id TEXT NOT NULL,
    strategy TEXT NOT NULL,
    status TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, target_run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, to_provider_thread_id) REFERENCES orchestration_v2_projection_provider_threads (thread_id, provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('pending', 'ready', 'failed', 'superseded'))
    ,
    UNIQUE (thread_id, context_handoff_id)
) STRICT;

CREATE TABLE orchestration_v2_projection_context_transfers (
    context_transfer_id TEXT PRIMARY KEY,
    source_thread_id TEXT NOT NULL,
    target_thread_id TEXT NOT NULL,
    target_run_id TEXT,
    type TEXT NOT NULL,
    status TEXT NOT NULL,
    source_provider TEXT,
    target_provider TEXT,
    updated_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    source_provider_instance_id TEXT,
    target_provider_instance_id TEXT,
    FOREIGN KEY (source_thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (target_thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (target_thread_id, target_run_id) REFERENCES orchestration_v2_projection_runs (thread_id, run_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('pending', 'resolved_native', 'resolved_portable', 'failed', 'consumed', 'superseded'))
) STRICT;

CREATE TABLE orchestration_v2_turn_item_positions (
    thread_id TEXT NOT NULL,
    turn_item_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY (thread_id, turn_item_id),
    UNIQUE (thread_id, ordinal),
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (thread_id, turn_item_id) REFERENCES orchestration_v2_projection_turn_items (thread_id, turn_item_id) DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE provider_session_runtime (
    thread_id TEXT PRIMARY KEY,
    provider_name TEXT NOT NULL,
    adapter_key TEXT NOT NULL,
    runtime_mode TEXT NOT NULL DEFAULT 'full-access',
    status TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    resume_cursor_json TEXT,
    runtime_payload_json TEXT,
    provider_instance_id TEXT,
    FOREIGN KEY (thread_id) REFERENCES orchestration_v2_projection_threads (thread_id) DEFERRABLE INITIALLY DEFERRED,
    CHECK (status IN ('starting', 'running', 'stopped', 'error'))
) STRICT;

CREATE INDEX orchestration_events_stream_sequence ON orchestration_events (aggregate_kind, stream_id, sequence);

CREATE INDEX orchestration_events_command ON orchestration_events (command_id);

CREATE INDEX orchestration_events_expiry ON orchestration_events (occurred_at, sequence);

CREATE INDEX orchestration_command_receipts_owner ON orchestration_command_receipts (aggregate_kind, aggregate_id);

CREATE INDEX orchestration_v2_effect_outbox_claim ON orchestration_v2_effect_outbox (status, available_at, lease_expires_at, created_at);

CREATE INDEX orchestration_v2_runs_queue ON orchestration_v2_projection_runs (thread_id, status, ordinal);

CREATE INDEX orchestration_v2_items_history ON orchestration_v2_projection_turn_items (thread_id, ordinal);

CREATE INDEX orchestration_v2_items_run ON orchestration_v2_projection_turn_items (thread_id, run_id, ordinal);

CREATE INDEX orchestration_v2_provider_threads_instance ON orchestration_v2_projection_provider_threads (provider_instance_id, thread_id);

CREATE INDEX orchestration_v2_provider_turns_attempt ON orchestration_v2_projection_provider_turns (run_attempt_id, ordinal);

CREATE UNIQUE INDEX orchestration_v2_runs_ordinal ON orchestration_v2_projection_runs (thread_id, ordinal);

CREATE UNIQUE INDEX orchestration_v2_attempts_ordinal ON orchestration_v2_projection_run_attempts (run_id, attempt_ordinal);

CREATE UNIQUE INDEX orchestration_v2_turns_ordinal ON orchestration_v2_projection_provider_turns (provider_thread_id, ordinal);

CREATE UNIQUE INDEX orchestration_v2_item_positions_ordinal ON orchestration_v2_turn_item_positions (thread_id, ordinal);

CREATE UNIQUE INDEX orchestration_events_version ON orchestration_events (aggregate_kind, stream_id, stream_version);

-- Physical identities remain queryable after logical provider rows are rebound.
CREATE INDEX orchestration_v2_provider_threads_native_idx ON orchestration_v2_projection_provider_threads(provider_instance_id,driver,json_extract(payload_json,'$.nativeThreadRef.nativeId'));
CREATE INDEX orchestration_v2_attempts_native_idx ON orchestration_v2_projection_run_attempts(provider_instance_id,json_extract(payload_json,'$.nativeThreadRef.driver'),json_extract(payload_json,'$.nativeThreadRef.nativeId'));
CREATE INDEX orchestration_v2_provider_turns_native_idx ON orchestration_v2_projection_provider_turns(json_extract(payload_json,'$.nativeThreadRef.driver'),json_extract(payload_json,'$.nativeThreadRef.nativeId'));
