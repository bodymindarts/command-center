-- Natural memories (simple CRUD, no event sourcing)
CREATE TABLE IF NOT EXISTS natural_memories (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL,
    content         TEXT NOT NULL,
    tags            TEXT NOT NULL DEFAULT '[]',
    project         TEXT,
    source_task     TEXT,
    source_type     TEXT NOT NULL DEFAULT 'manual',
    file_path       TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    last_accessed   TEXT,
    access_count    INTEGER NOT NULL DEFAULT 0,
    pinned          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_natural_memories_project ON natural_memories(project);
CREATE INDEX IF NOT EXISTS idx_natural_memories_created ON natural_memories(created_at);

-- Research reports (es-entity: index table + events table)
CREATE TABLE IF NOT EXISTS research_reports (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL DEFAULT '',
    project         TEXT,
    status          TEXT NOT NULL DEFAULT 'active',
    created_at      TEXT,
    deleted         INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_research_reports_project ON research_reports(project);

CREATE TABLE IF NOT EXISTS research_report_events (
    id              TEXT NOT NULL REFERENCES research_reports(id),
    sequence        INTEGER NOT NULL,
    event_type      TEXT NOT NULL,
    event           TEXT NOT NULL,
    context         TEXT DEFAULT NULL,
    recorded_at     TEXT NOT NULL,
    UNIQUE(id, sequence)
);

-- FTS5 full-text search across both memory types.
-- We use a standalone (non-content) FTS table that we populate manually
-- so we can index content from multiple source tables.
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    memory_id,
    memory_type,
    title,
    content,
    tags
);
