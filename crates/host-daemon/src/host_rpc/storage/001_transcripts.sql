CREATE TABLE IF NOT EXISTS conversations (
    provider TEXT NOT NULL,
    id TEXT NOT NULL,
    cwd TEXT,
    metadata BLOB,
    PRIMARY KEY (provider, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS turns (
    provider TEXT NOT NULL,
    id TEXT NOT NULL,
    position INTEGER NOT NULL,
    turn_id TEXT NOT NULL,
    running INTEGER NOT NULL,
    body BLOB NOT NULL,
    PRIMARY KEY (provider, id, position)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS turn_ids ON turns (provider, id, turn_id, position);
-- Host-wide order of each conversation's latest change.
CREATE TABLE IF NOT EXISTS versions (
    provider TEXT NOT NULL,
    id TEXT NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (provider, id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS version_order ON versions (version);
