-- Watch entity tables (es-entity pattern).

-- Index table for watches.
CREATE TABLE IF NOT EXISTS watches (
    id          TEXT PRIMARY KEY NOT NULL,
    task_id     TEXT NOT NULL,
    name        TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'active',
    job_id      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    deleted     INTEGER DEFAULT 0
);

-- Events table (immutable log).
CREATE TABLE IF NOT EXISTS watch_events (
    id          TEXT NOT NULL REFERENCES watches(id),
    sequence    INTEGER NOT NULL,
    event_type  TEXT NOT NULL,
    event       TEXT NOT NULL,
    context     TEXT DEFAULT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(id, sequence)
);

-- Unique partial index: only one active watch per (task_id, name).
-- Enables upsert semantics — creating a watch with an existing name
-- reschedules the existing entity in place.
CREATE UNIQUE INDEX idx_watches_task_name_active
    ON watches(task_id, name) WHERE status = 'active';

-- Index for fast lookup by job_id (used by runners to check watch status).
CREATE INDEX idx_watches_job_id ON watches(job_id);

-- Index for listing active watches by task.
CREATE INDEX idx_watches_task_status ON watches(task_id, status);
