-- Convert projects from flat CRUD to event-sourced entity (es-entity pattern).

-- Events table (immutable log).
CREATE TABLE IF NOT EXISTS project_events (
    id          TEXT NOT NULL REFERENCES projects(id),
    sequence    INTEGER NOT NULL,
    event_type  TEXT NOT NULL,
    event       TEXT NOT NULL,
    context     TEXT DEFAULT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(id, sequence)
);

-- Add columns expected by es-entity on the index table.
ALTER TABLE projects ADD COLUMN deleted INTEGER DEFAULT 0;

-- Migrate existing projects into project_events.
-- For each project, create an Initialized event (sequence 1).
INSERT INTO project_events (id, sequence, event_type, event, recorded_at)
SELECT
    id,
    1,
    'initialized',
    json_object(
        'type', 'initialized',
        'id', id,
        'name', name,
        'description', COALESCE(description, '')
    ),
    COALESCE(created_at, datetime('now'))
FROM projects;
