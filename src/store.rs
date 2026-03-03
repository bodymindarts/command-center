use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row};
use rusqlite_migration::{M, Migrations};

use crate::primitives::{MessageRole, TaskId, TaskStatus};
use crate::task::{Project, Task, TaskMessage};

static MIGRATION_STEPS: [M<'static>; 2] = [
    M::up(
        "CREATE TABLE IF NOT EXISTS tasks (
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
            output       TEXT
        );
        CREATE TABLE IF NOT EXISTS task_messages (
            id         TEXT PRIMARY KEY,
            task_id    TEXT NOT NULL,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    ),
    M::up(
        "CREATE TABLE IF NOT EXISTS projects (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL
        );
        ALTER TABLE tasks ADD COLUMN project_id TEXT;",
    ),
];
static MIGRATIONS: Migrations<'static> = Migrations::from_slice(&MIGRATION_STEPS);

fn row_to_task(row: &Row) -> rusqlite::Result<Task> {
    let started_at: String = row.get(8)?;
    let completed_at: Option<String> = row.get(9)?;
    Ok(Task {
        id: TaskId::from(row.get::<_, String>(0)?),
        name: row.get(1)?,
        skill_name: row.get(2)?,
        params_json: row.get(3)?,
        status: TaskStatus::from(row.get::<_, String>(4)?),
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
        project_id: row.get(12)?,
    })
}

const TASK_COLUMNS: &str =
    "id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
     started_at, completed_at, exit_code, output, project_id";

pub struct Store {
    conn: Connection,
}

impl Store {
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        Self::run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open(db_path: &Path) -> Result<Self> {
        let mut conn = Connection::open(db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        Self::run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    fn run_migrations(conn: &mut Connection) -> Result<()> {
        MIGRATIONS
            .to_latest(conn)
            .context("failed to run database migrations")?;
        Ok(())
    }

    pub fn insert_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir, started_at, project_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                task.id.as_str(),
                &task.name,
                &task.skill_name,
                &task.params_json,
                task.status.as_str(),
                &task.tmux_pane,
                &task.tmux_window,
                &task.work_dir,
                task.started_at.to_rfc3339(),
                &task.project_id,
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

    pub fn close_task(&self, id: &str, output: Option<&str>) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'closed', completed_at = ?1, output = ?2
             WHERE id = ?3 AND status = 'running'",
            (&now, output, id),
        )?;
        Ok(rows > 0)
    }

    pub fn reopen_task(&self, id: &str, pane: &str, window: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'running', tmux_pane = ?1, tmux_window = ?2, completed_at = NULL
             WHERE id = ?3 AND status != 'running'",
            (pane, window, id),
        )?;
        Ok(rows > 0)
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM task_messages WHERE task_id = ?1", [id])?;
        self.conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY started_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn list_active_tasks(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'running' ORDER BY started_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], row_to_task)?
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
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id LIKE ?1");
        let mut stmt = self.conn.prepare(&sql)?;

        let mut tasks: Vec<Task> = stmt
            .query_map([&pattern], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;

        if tasks.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", tasks.len());
        }

        Ok(tasks.pop())
    }

    pub fn insert_message(&self, task_id: &str, role: MessageRole, content: &str) -> Result<()> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO task_messages (id, task_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (&id, task_id, role.as_str(), content, &now),
        )?;
        Ok(())
    }

    pub fn list_messages(&self, task_id: &str) -> Result<Vec<TaskMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, role, content, created_at
             FROM task_messages WHERE task_id = ?1 ORDER BY created_at ASC",
        )?;

        let messages = stmt
            .query_map([task_id], |row| {
                let created_at: String = row.get(4)?;
                Ok(TaskMessage {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    role: MessageRole::from(row.get::<_, String>(2)?),
                    content: row.get(3)?,
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(messages)
    }

    // -- Project CRUD --

    pub fn insert_project(&self, id: &str, name: &str, description: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO projects (id, name, description, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            (id, name, description, &now),
        )?;
        Ok(())
    }

    pub fn get_project_by_name(&self, name: &str) -> Result<Option<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description, created_at FROM projects WHERE name = ?1")?;
        let mut rows = stmt.query_map([name], |row| {
            let created_at: String = row.get(3)?;
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: DateTime::parse_from_rfc3339(&created_at)
                    .unwrap_or_default()
                    .with_timezone(&Utc),
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at FROM projects ORDER BY created_at ASC",
        )?;
        let projects = stmt
            .query_map([], |row| {
                let created_at: String = row.get(3)?;
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Returns visible tasks scoped to a project.
    /// When project_id is None, returns tasks with no project (default/ExO scope).
    /// When Some, returns tasks belonging to that project.
    pub fn list_visible_tasks_for_project(&self, project_id: Option<&str>) -> Result<Vec<Task>> {
        let sql = match project_id {
            Some(_) => format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id = ?1 \
                 ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
            ),
            None => format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id IS NULL \
                 ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = match project_id {
            Some(pid) => stmt
                .query_map([pid], row_to_task)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map([], row_to_task)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn insert_running_task(store: &Store, id: &str, name: &str) {
        let task = Task {
            id: TaskId::from(id.to_string()),
            name: name.to_string(),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            status: TaskStatus::Running,
            tmux_pane: Some("%1".to_string()),
            tmux_window: Some("@1".to_string()),
            work_dir: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id: None,
        };
        store.insert_task(&task).unwrap();
    }

    #[test]
    fn close_task_sets_status_and_timestamp() {
        let store = test_store();
        insert_running_task(&store, "aaa", "t1");

        let ok = store.close_task("aaa", Some("output text")).unwrap();
        assert!(ok);

        let task = store.get_task_by_prefix("aaa").unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.completed_at.is_some());
        assert_eq!(task.output.as_deref(), Some("output text"));
    }

    #[test]
    fn close_task_only_affects_running() {
        let store = test_store();
        insert_running_task(&store, "bbb", "t2");
        store.complete_task("bbb", 0, None).unwrap();

        let ok = store.close_task("bbb", None).unwrap();
        assert!(!ok);

        let task = store.get_task_by_prefix("bbb").unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn list_active_excludes_non_running() {
        let store = test_store();
        insert_running_task(&store, "ccc", "active");
        insert_running_task(&store, "ddd", "done");
        store.complete_task("ddd", 0, None).unwrap();
        insert_running_task(&store, "eee", "closed");
        store.close_task("eee", None).unwrap();

        let active = store.list_active_tasks().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active");

        let all = store.list_tasks().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn insert_and_list_messages() {
        let store = test_store();
        insert_running_task(&store, "msg-task", "t1");

        store
            .insert_message("msg-task", MessageRole::System, "initial prompt")
            .unwrap();
        store
            .insert_message("msg-task", MessageRole::User, "hello agent")
            .unwrap();

        let messages = store.list_messages("msg-task").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[0].content, "initial prompt");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "hello agent");
    }

    #[test]
    fn list_messages_empty_for_unknown_task() {
        let store = test_store();
        let messages = store.list_messages("nonexistent").unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn messages_ordered_by_created_at() {
        let store = test_store();
        insert_running_task(&store, "ord-task", "t1");

        store
            .insert_message("ord-task", MessageRole::System, "first")
            .unwrap();
        store
            .insert_message("ord-task", MessageRole::User, "second")
            .unwrap();
        store
            .insert_message("ord-task", MessageRole::User, "third")
            .unwrap();

        let messages = store.list_messages("ord-task").unwrap();
        assert_eq!(messages.len(), 3);
        assert!(messages[0].created_at <= messages[1].created_at);
        assert!(messages[1].created_at <= messages[2].created_at);
    }
}
