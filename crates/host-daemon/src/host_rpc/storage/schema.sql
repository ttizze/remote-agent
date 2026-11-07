-- The row is authoritative. Values used by queries and constraints are
-- generated from its JSON, so they cannot diverge from the stored entity.
CREATE TABLE threads (
    thread_id TEXT PRIMARY KEY NOT NULL CHECK (json_valid(thread_id)),
    metadata_json TEXT NOT NULL CHECK (json_valid(metadata_json)),
    imported INTEGER NOT NULL DEFAULT 0 CHECK (imported IN (0, 1)),
    managed INTEGER NOT NULL DEFAULT 0 CHECK (managed IN (0, 1)),
    listed INTEGER NOT NULL DEFAULT 0 CHECK (listed IN (0, 1)),
    active_provider_thread_id TEXT REFERENCES provider_threads(provider_thread_id)
        DEFERRABLE INITIALLY DEFERRED,
    host_title TEXT,
    host_preview TEXT,
    cwd TEXT GENERATED ALWAYS AS (json_extract(metadata_json, '$.thread.cwd')) VIRTUAL,
    updated_at_ms INTEGER GENERATED ALWAYS AS (CAST(json_extract(metadata_json, '$.thread.updatedAt') * 1000 AS INTEGER)) VIRTUAL
) STRICT;
CREATE INDEX threads_updated ON threads(updated_at_ms DESC, thread_id);

CREATE TABLE provider_threads (
    provider_thread_id TEXT PRIMARY KEY NOT NULL CHECK (json_valid(provider_thread_id)),
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    covered_runs INTEGER CHECK (covered_runs >= 0),
    UNIQUE (thread_id, ordinal)
) STRICT;

-- A native alias owned by another public thread is never a list/import root.
CREATE VIEW public_threads AS
SELECT * FROM threads
WHERE NOT EXISTS (
    SELECT 1 FROM provider_threads AS native
    WHERE native.provider_thread_id = threads.thread_id AND native.thread_id <> threads.thread_id
);

CREATE TABLE history_segments (
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    provider_thread_id TEXT NOT NULL REFERENCES provider_threads(provider_thread_id) DEFERRABLE INITIALLY DEFERRED,
    start_run INTEGER NOT NULL CHECK (start_run >= 0),
    end_run INTEGER CHECK (end_run >= start_run),
    PRIMARY KEY (thread_id, ordinal)
) STRICT;

CREATE TABLE context_handoffs (
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    provider_thread_id TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.session')) VIRTUAL
        REFERENCES provider_threads(provider_thread_id),
    delivery TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.delivery')) VIRTUAL
        CHECK (delivery IN ('Ready', 'Pending', 'Injected', 'Inline')),
    PRIMARY KEY (thread_id, ordinal)
) STRICT;

CREATE TABLE thread_startups (
    thread_id TEXT PRIMARY KEY NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json))
) STRICT;

-- Persist intent before calling the provider and retain its receipt afterward.
-- A lost acknowledgement is unknown, never permission to send the input twice.
CREATE TABLE command_receipts (
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    command_id TEXT NOT NULL CHECK (length(command_id) BETWEEN 1 AND 256),
    delivery_json TEXT NOT NULL CHECK (json_valid(delivery_json)),
    PRIMARY KEY (thread_id, command_id)
) STRICT;

CREATE TABLE model_selections (
    native_session_id TEXT PRIMARY KEY NOT NULL CHECK (json_valid(native_session_id)),
    options_json TEXT NOT NULL CHECK (json_valid(options_json))
) STRICT;

-- App run identity is independent of the provider turn id. Providers can reuse
-- turn ids; ordinal and the items distinguish their occurrences.
CREATE TABLE runs (
    run_id TEXT PRIMARY KEY NOT NULL,
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    native_turn_id TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.id')) VIRTUAL NOT NULL,
    status TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.status')) VIRTUAL NOT NULL
        CHECK (status IN ('running', 'completed', 'failed', 'interrupted', 'unknown')),
    items_known INTEGER NOT NULL CHECK (items_known IN (0, 1)),
    UNIQUE (thread_id, ordinal)
) STRICT;
CREATE INDEX runs_native ON runs(thread_id, native_turn_id, ordinal DESC);

-- Messages, plans and tools are all display items. Their bodies occur once;
-- execution and handoff relationships refer to these identities.
CREATE TABLE turn_items (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    item_id TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.id')) VIRTUAL NOT NULL,
    status TEXT GENERATED ALWAYS AS (json_extract(payload_json, '$.status')) VIRTUAL NOT NULL
        CHECK (status IN ('running', 'completed', 'failed', 'declined', 'interrupted', 'unknown')),
    PRIMARY KEY (run_id, ordinal),
    UNIQUE (run_id, item_id)
) STRICT;

-- This is a changes feed, not an event archive: only the newest revision of an
-- entity is retained. AUTOINCREMENT prevents cursors moving backwards after
-- pruning or replacement. No prompt or streamed body is copied into this log.
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL REFERENCES threads(thread_id) ON DELETE CASCADE,
    entity_type TEXT NOT NULL CHECK (entity_type IN ('thread', 'run', 'item', 'binding', 'command')),
    entity_id TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    UNIQUE (thread_id, entity_type, entity_id)
) STRICT;
CREATE INDEX changes_thread ON changes(thread_id, sequence);
