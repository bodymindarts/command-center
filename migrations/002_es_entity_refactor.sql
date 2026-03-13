-- Convert tasks from flat CRUD to event-sourced entity (es-entity pattern).

-- Events table (immutable log).
CREATE TABLE IF NOT EXISTS task_events (
    id          TEXT NOT NULL REFERENCES tasks(id),
    sequence    INTEGER NOT NULL,
    event_type  TEXT NOT NULL,
    event       TEXT NOT NULL,
    context     TEXT DEFAULT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(id, sequence)
);

-- Add columns expected by es-entity on the index table.
ALTER TABLE tasks ADD COLUMN created_at TEXT;
ALTER TABLE tasks ADD COLUMN deleted INTEGER DEFAULT 0;

-- Backfill created_at from started_at for existing rows.
UPDATE tasks SET created_at = started_at WHERE created_at IS NULL;

-- Migrate existing tasks into task_events.
-- For each task, create a Spawned event (sequence 1).
INSERT INTO task_events (id, sequence, event_type, event, recorded_at)
SELECT
    id,
    1,
    'spawned',
    json_object(
        'type', 'spawned',
        'id', id,
        'name', name,
        'skill_name', skill_name,
        'params_json', params_json,
        'work_dir', work_dir,
        'session_id', COALESCE(session_id, ''),
        'project_id', project_id
    ),
    COALESCE(started_at, datetime('now'))
FROM tasks;

-- For tasks with tmux_pane set, create an AgentLaunched event (sequence 2).
INSERT INTO task_events (id, sequence, event_type, event, recorded_at)
SELECT
    id,
    2,
    'agent_launched',
    json_object(
        'type', 'agent_launched',
        'tmux_pane', tmux_pane,
        'tmux_window', tmux_window
    ),
    COALESCE(started_at, datetime('now'))
FROM tasks
WHERE tmux_pane IS NOT NULL;

-- For completed/failed tasks, create a Completed event.
INSERT INTO task_events (id, sequence, event_type, event, recorded_at)
SELECT
    id,
    CASE WHEN tmux_pane IS NOT NULL THEN 3 ELSE 2 END,
    'completed',
    json_object(
        'type', 'completed',
        'exit_code', exit_code,
        'output', output
    ),
    COALESCE(completed_at, datetime('now'))
FROM tasks
WHERE status IN ('completed', 'failed');

-- For closed tasks, create a Closed event.
INSERT INTO task_events (id, sequence, event_type, event, recorded_at)
SELECT
    id,
    CASE WHEN tmux_pane IS NOT NULL THEN 3 ELSE 2 END,
    'closed',
    json_object(
        'type', 'closed',
        'output', output
    ),
    COALESCE(completed_at, datetime('now'))
FROM tasks
WHERE status = 'closed';

-- Drop columns that now live exclusively in the event log.
ALTER TABLE tasks DROP COLUMN skill_name;
ALTER TABLE tasks DROP COLUMN params_json;
