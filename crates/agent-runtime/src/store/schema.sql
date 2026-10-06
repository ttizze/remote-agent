CREATE TABLE runtime_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE threads (
    thread_id TEXT PRIMARY KEY,
    thread_seq INTEGER NOT NULL,
    input_seq INTEGER NOT NULL,
    last_global_seq INTEGER NOT NULL,
    needs_recovery INTEGER NOT NULL
) STRICT;

CREATE TABLE facts (
    global_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL,
    thread_seq INTEGER NOT NULL,
    input_seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    at TEXT NOT NULL,
    payload TEXT NOT NULL,
    UNIQUE (thread_id, thread_seq)
) STRICT;
CREATE INDEX facts_thread_global ON facts (thread_id, global_seq);
CREATE INDEX facts_thread_created ON facts (thread_id) WHERE kind = 'ThreadCreated';
CREATE INDEX facts_scope_bound ON facts (thread_id) WHERE kind = 'CheckpointScopeBound';

CREATE TABLE receipts (
    command_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    reply TEXT NOT NULL,
    thread_seq INTEGER NOT NULL,
    global_seq INTEGER NOT NULL,
    accepted_at TEXT NOT NULL
) STRICT;

CREATE TABLE outbox (
    effect_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    lane TEXT NOT NULL CHECK (lane IN ('main', 'title')),
    kind TEXT NOT NULL,
    attempt_id TEXT,
    payload TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts INTEGER NOT NULL,
    available_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    last_error TEXT
) STRICT;
CREATE INDEX outbox_thread_lane ON outbox (thread_id, lane, status);
CREATE INDEX outbox_open ON outbox (status, available_at) WHERE status IN ('pending', 'running');

CREATE TABLE thread_snapshots (
    thread_id TEXT PRIMARY KEY,
    thread_seq INTEGER NOT NULL,
    format TEXT NOT NULL,
    blob BLOB NOT NULL
) STRICT;

CREATE TABLE thread_shells (
    thread_id TEXT PRIMARY KEY,
    global_seq INTEGER NOT NULL,
    project TEXT NOT NULL,
    archived INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    needs_recovery INTEGER NOT NULL,
    payload TEXT NOT NULL
) STRICT;
CREATE INDEX thread_shells_global ON thread_shells (global_seq);

CREATE TABLE search_messages (
    thread_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    role TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (thread_id, message_id)
) STRICT;

CREATE TABLE launches (
    command_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    project TEXT NOT NULL,
    strategy TEXT NOT NULL,
    status TEXT NOT NULL,
    worktree_path TEXT,
    branch TEXT,
    last_error TEXT,
    request TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX launches_thread ON launches (thread_id);

CREATE TABLE checkpoint_baselines (
    scope_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    checkpoint_id TEXT NOT NULL,
    file_ref TEXT NOT NULL,
    native_heads TEXT NOT NULL,
    PRIMARY KEY (scope_id, ordinal)
) STRICT;

CREATE TABLE attachment_refs (
    path TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    PRIMARY KEY (path, thread_id)
) STRICT;

CREATE TABLE imported_sources (
    instance TEXT NOT NULL,
    path TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    project_root TEXT NOT NULL,
    native_session TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (instance, path, fingerprint)
) STRICT;
CREATE INDEX imported_sources_root ON imported_sources (project_root);
CREATE INDEX facts_native_session
    ON facts (COALESCE(
        json_extract(payload, '$.SessionBound.native_thread'),
        json_extract(payload, '$.NativeSessionBound.native_thread'),
        json_extract(payload, '$.NativeChildBound.native_thread')))
    WHERE kind IN ('SessionBound', 'NativeSessionBound', 'NativeChildBound');
