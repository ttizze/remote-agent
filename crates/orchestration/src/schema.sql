PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS orchestration_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_kind TEXT NOT NULL CHECK(aggregate_kind='thread'),
    stream_id TEXT NOT NULL,
    stream_version INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    command_id TEXT,
    payload_json TEXT NOT NULL,
    application_event_version INTEGER NOT NULL CHECK(application_event_version=2),
    UNIQUE(aggregate_kind,stream_id,stream_version)
);
CREATE INDEX IF NOT EXISTS orchestration_events_stream ON orchestration_events(stream_id,sequence);
CREATE INDEX IF NOT EXISTS orchestration_events_command ON orchestration_events(command_id);
CREATE TABLE IF NOT EXISTS orchestration_command_receipts (
    command_id TEXT PRIMARY KEY,
    aggregate_kind TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    result_sequence INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN('accepted','rejected')),
    error TEXT,
    command_type TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT NOT NULL,
    archived_at TEXT,
    deleted_at TEXT,
    payload_json TEXT NOT NULL,
    projection_updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS orchestration_v2_projection_metadata (
    projection_name TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL,
    updated_at TEXT
);
INSERT OR IGNORE INTO orchestration_v2_projection_metadata VALUES('v2',2,0,NULL);
CREATE TABLE IF NOT EXISTS orchestration_v2_turn_item_positions (
    thread_id TEXT NOT NULL,
    turn_item_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    PRIMARY KEY(thread_id,turn_item_id),
    UNIQUE(thread_id,ordinal)
);
CREATE TABLE IF NOT EXISTS orchestration_v2_effect_outbox (
    effect_id TEXT PRIMARY KEY,
    command_id TEXT,
    thread_id TEXT NOT NULL,
    effect_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN('pending','running','succeeded','failed','cancelled')),
    attempt_count INTEGER NOT NULL,
    available_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at INTEGER,
    last_error TEXT
);
CREATE INDEX IF NOT EXISTS orchestration_v2_effect_lane ON orchestration_v2_effect_outbox(thread_id,status);

CREATE TABLE IF NOT EXISTS orchestration_host_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO orchestration_host_metadata VALUES('instance', lower(hex(randomblob(16))));
