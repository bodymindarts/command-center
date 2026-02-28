use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

#[allow(dead_code)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub skill_name: String,
    pub params_json: String,
    pub status: String,
    pub tmux_pane: Option<String>,
    pub tmux_window: Option<String>,
    pub work_dir: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id           TEXT PRIMARY KEY,
                skill_name   TEXT NOT NULL,
                params_json  TEXT NOT NULL,
                status       TEXT NOT NULL DEFAULT 'running',
                tmux_pane    TEXT,
                tmux_window  TEXT,
                work_dir     TEXT,
                started_at   TEXT NOT NULL,
                completed_at TEXT,
                exit_code    INTEGER,
                output       TEXT
            );",
        )?;
        // Migrations: add columns to existing databases
        let _ = self
            .conn
            .execute_batch("ALTER TABLE tasks ADD COLUMN tmux_window TEXT");
        let _ = self
            .conn
            .execute_batch("ALTER TABLE tasks ADD COLUMN name TEXT NOT NULL DEFAULT ''");
        Ok(())
    }

    pub fn insert_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                &task.id,
                &task.name,
                &task.skill_name,
                &task.params_json,
                &task.status,
                &task.tmux_pane,
                &task.tmux_window,
                &task.work_dir,
                task.started_at.to_rfc3339(),
            ),
        )?;
        Ok(())
    }

    pub fn complete_task(&self, id: &str, exit_code: i32, output: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let status = if exit_code == 0 {
            "completed"
        } else {
            "failed"
        };
        self.conn.execute(
            "UPDATE tasks SET status = ?1, exit_code = ?2, completed_at = ?3, output = ?4
             WHERE id = ?5",
            (status, exit_code, &now, output, id),
        )?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
                    started_at, completed_at, exit_code, output
             FROM tasks ORDER BY started_at DESC",
        )?;

        let tasks = stmt
            .query_map([], |row| {
                let started_at: String = row.get(8)?;
                let completed_at: Option<String> = row.get(9)?;
                Ok(Task {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    skill_name: row.get(2)?,
                    params_json: row.get(3)?,
                    status: row.get(4)?,
                    tmux_pane: row.get(5)?,
                    tmux_window: row.get(6)?,
                    work_dir: row.get(7)?,
                    started_at: DateTime::parse_from_rfc3339(&started_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                    completed_at: completed_at.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                    exit_code: row.get(10)?,
                    output: row.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tasks)
    }

    pub fn update_tmux_pane(&self, id: &str, pane_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET tmux_pane = ?1 WHERE id = ?2",
            (pane_id, id),
        )?;
        Ok(())
    }

    pub fn update_tmux_window(&self, id: &str, window_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET tmux_window = ?1 WHERE id = ?2",
            (window_id, id),
        )?;
        Ok(())
    }

    pub fn get_task_by_prefix(&self, prefix: &str) -> Result<Option<Task>> {
        let pattern = format!("{prefix}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
                    started_at, completed_at, exit_code, output
             FROM tasks WHERE id LIKE ?1",
        )?;

        let mut tasks: Vec<Task> = stmt
            .query_map([&pattern], |row| {
                let started_at: String = row.get(8)?;
                let completed_at: Option<String> = row.get(9)?;
                Ok(Task {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    skill_name: row.get(2)?,
                    params_json: row.get(3)?,
                    status: row.get(4)?,
                    tmux_pane: row.get(5)?,
                    tmux_window: row.get(6)?,
                    work_dir: row.get(7)?,
                    started_at: DateTime::parse_from_rfc3339(&started_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                    completed_at: completed_at.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                    exit_code: row.get(10)?,
                    output: row.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        if tasks.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", tasks.len());
        }

        Ok(tasks.pop())
    }
}
