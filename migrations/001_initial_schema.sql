-- Full schema (collapsed from 3 rusqlite_migration steps).
-- Uses CREATE TABLE IF NOT EXISTS so this is safe to run on existing databases.

CREATE TABLE IF NOT EXISTS tasks (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL DEFAULT '',
    skill_name   TEXT NOT NULL,
    params_json  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'running',
    tmux_pane    TEXT,
    tmux_window  TEXT,
    work_dir     TEXT,
    started_at   TEXT NOT NULL,
    completed_at TEXT,
    exit_code    INTEGER,
    output       TEXT,
    project_id   TEXT,
    session_id   TEXT
);

CREATE TABLE IF NOT EXISTS task_messages (
    id         TEXT PRIMARY KEY,
    task_id    TEXT NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);
