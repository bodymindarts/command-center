use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row};
use rusqlite_migration::{M, Migrations};

use crate::primitives::{
    ChatId, ClatAction, ClaudeSessionId, MessageRole, PaneId, ProjectId, ProjectName, ScheduleId,
    TaskId, TaskName, TaskStatus, WindowId,
};
use crate::schedule::{DiffMode, Schedule, ScheduleType};
use crate::task::{Project, Task, TaskMessage};

static MIGRATION_STEPS: [M<'static>; 7] = [
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
    M::up("ALTER TABLE tasks ADD COLUMN session_id TEXT;"),
    // Migration 3: Schedules table
    M::up(
        "CREATE TABLE IF NOT EXISTS schedules (
            id            TEXT PRIMARY KEY,
            name          TEXT NOT NULL UNIQUE,
            schedule_type TEXT NOT NULL,
            schedule_expr TEXT NOT NULL,
            action        TEXT NOT NULL,
            enabled       INTEGER NOT NULL DEFAULT 1,
            last_run_at   TEXT,
            next_run_at   TEXT,
            created_at    TEXT NOT NULL,
            run_count     INTEGER NOT NULL DEFAULT 0,
            max_runs      INTEGER
        );",
    ),
    // Migration 4: Task completion triggers
    M::up(
        "ALTER TABLE tasks ADD COLUMN on_complete_success TEXT;
         ALTER TABLE tasks ADD COLUMN on_complete_failure TEXT;",
    ),
    // Migration 5: Watch support — add check_command, diff_mode, last_check_output to schedules
    M::up(
        "ALTER TABLE schedules ADD COLUMN check_command TEXT;
         ALTER TABLE schedules ADD COLUMN diff_mode TEXT NOT NULL DEFAULT 'string';
         ALTER TABLE schedules ADD COLUMN last_check_output TEXT;",
    ),
    // Migration 6: On-idle trigger for tasks
    M::up(
        "ALTER TABLE tasks ADD COLUMN on_idle TEXT;
         ALTER TABLE tasks ADD COLUMN on_idle_fired INTEGER NOT NULL DEFAULT 0;",
    ),
];
static MIGRATIONS: Migrations<'static> = Migrations::from_slice(&MIGRATION_STEPS);

fn row_to_task(row: &Row) -> rusqlite::Result<Task> {
    let started_at: String = row.get(8)?;
    let completed_at: Option<String> = row.get(9)?;
    Ok(Task {
        id: TaskId::from(row.get::<_, String>(0)?),
        name: TaskName::from(row.get::<_, String>(1)?),
        skill_name: row.get(2)?,
        params_json: row.get(3)?,
        status: TaskStatus::from(row.get::<_, String>(4)?),
        tmux_pane: row.get::<_, Option<String>>(5)?.map(PaneId::from),
        tmux_window: row.get::<_, Option<String>>(6)?.map(WindowId::from),
        work_dir: row.get(7)?,
        session_id: row.get::<_, Option<String>>(13)?.map(ClaudeSessionId::from),
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
        project_id: row.get::<_, Option<String>>(12)?.map(ProjectId::from),
        on_complete_success: row.get::<_, Option<String>>(14)?.map(ClatAction::from),
        on_complete_failure: row.get::<_, Option<String>>(15)?.map(ClatAction::from),
        on_idle: row.get::<_, Option<String>>(16)?.map(ClatAction::from),
        on_idle_fired: row.get::<_, bool>(17).unwrap_or(false),
    })
}

const SCHEDULE_COLUMNS: &str = "id, name, schedule_type, schedule_expr, action, enabled,
     last_run_at, next_run_at, created_at, run_count, max_runs,
     check_command, diff_mode, last_check_output";

fn row_to_schedule(row: &Row) -> rusqlite::Result<Schedule> {
    let created_at: String = row.get(8)?;
    let last_run_at: Option<String> = row.get(6)?;
    let next_run_at: Option<String> = row.get(7)?;
    Ok(Schedule {
        id: ScheduleId::from(row.get::<_, String>(0)?),
        name: row.get(1)?,
        schedule_type: ScheduleType::from(row.get::<_, String>(2)?),
        schedule_expr: row.get(3)?,
        action: row.get(4)?,
        enabled: row.get(5)?,
        last_run_at: last_run_at.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        next_run_at: next_run_at.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .unwrap_or_default()
            .with_timezone(&Utc),
        run_count: row.get(9)?,
        max_runs: row.get(10)?,
        check_command: row.get(11)?,
        diff_mode: DiffMode::from(
            row.get::<_, String>(12)
                .unwrap_or_else(|_| "string".to_string()),
        ),
        last_check_output: row.get(13)?,
    })
}

const TASK_COLUMNS: &str =
    "id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
     started_at, completed_at, exit_code, output, project_id, session_id,
     on_complete_success, on_complete_failure, on_idle, on_idle_fired";

pub struct Store {
    conn: Connection,
}

impl Store {
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let mut conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        Self::run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let mut conn = Connection::open(db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        Self::run_migrations(&mut conn)?;
        Ok(Self { conn })
    }

    fn run_migrations(conn: &mut Connection) -> anyhow::Result<()> {
        MIGRATIONS
            .to_latest(conn)
            .context("failed to run database migrations")?;
        Ok(())
    }

    pub fn insert_task(&self, task: &Task) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir, started_at, project_id, session_id, on_complete_success, on_complete_failure, on_idle)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            (
                task.id.as_str(),
                task.name.as_str(),
                &task.skill_name,
                &task.params_json,
                task.status.as_str(),
                task.tmux_pane.as_ref().map(|p| p.as_str()),
                task.tmux_window.as_ref().map(|w| w.as_str()),
                &task.work_dir,
                task.started_at.to_rfc3339(),
                task.project_id.as_ref().map(|p| p.as_str()),
                task.session_id.as_ref().map(|s| s.as_str()),
                task.on_complete_success.as_ref().map(|a| a.as_str()),
                task.on_complete_failure.as_ref().map(|a| a.as_str()),
                task.on_idle.as_ref().map(|a| a.as_str()),
            ),
        )?;
        Ok(())
    }

    pub fn complete_task(
        &self,
        id: &TaskId,
        exit_code: i32,
        output: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let status = if exit_code == 0 {
            "completed"
        } else {
            "failed"
        };
        self.conn.execute(
            "UPDATE tasks SET status = ?1, exit_code = ?2, completed_at = ?3, output = ?4
             WHERE id = ?5",
            (status, exit_code, &now, output, id.as_str()),
        )?;
        Ok(())
    }

    pub fn close_task(&self, id: &TaskId, output: Option<&str>) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'closed', completed_at = ?1, output = ?2
             WHERE id = ?3 AND status = 'running'",
            (&now, output, id.as_str()),
        )?;
        Ok(rows > 0)
    }

    pub fn reopen_task(
        &self,
        id: &TaskId,
        pane: &PaneId,
        window: &WindowId,
    ) -> anyhow::Result<bool> {
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'running', tmux_pane = ?1, tmux_window = ?2, completed_at = NULL
             WHERE id = ?3 AND status != 'running'",
            (pane.as_str(), window.as_str(), id.as_str()),
        )?;
        Ok(rows > 0)
    }

    pub fn delete_task(&self, id: &TaskId) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM task_messages WHERE task_id = ?1",
            [id.as_str()],
        )?;
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", [id.as_str()])?;
        Ok(())
    }

    pub fn list_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY started_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn list_active_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'running' ORDER BY started_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, tmux_pane = ?2, tmux_window = ?3,
             completed_at = ?4, exit_code = ?5, output = ?6
             WHERE id = ?7",
            (
                task.status.as_str(),
                task.tmux_pane.as_ref().map(|p| p.as_str()),
                task.tmux_window.as_ref().map(|w| w.as_str()),
                task.completed_at.as_ref().map(|dt| dt.to_rfc3339()),
                task.exit_code,
                task.output.as_deref(),
                task.id.as_str(),
            ),
        )?;
        Ok(())
    }

    pub fn get_task_by_prefix(&self, prefix: &str) -> anyhow::Result<Option<Task>> {
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

    pub fn insert_message(
        &self,
        chat_id: &ChatId,
        role: MessageRole,
        content: &str,
    ) -> anyhow::Result<()> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        let key = chat_id.as_db_key();
        self.conn.execute(
            "INSERT INTO task_messages (id, task_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (&id, &key, role.as_str(), content, &now),
        )?;
        Ok(())
    }

    pub fn list_messages(&self, chat_id: &ChatId) -> anyhow::Result<Vec<TaskMessage>> {
        let key = chat_id.as_db_key();
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, role, content, created_at
             FROM task_messages WHERE task_id = ?1 ORDER BY created_at ASC",
        )?;

        let messages = stmt
            .query_map([&key], |row| {
                let created_at: String = row.get(4)?;
                Ok(TaskMessage {
                    id: row.get(0)?,
                    chat_id: row.get(1)?,
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

    pub fn insert_project(
        &self,
        id: &ProjectId,
        name: &str,
        description: &str,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO projects (id, name, description, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            (id.as_str(), name, description, &now),
        )?;
        Ok(())
    }

    pub fn get_project_by_name(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description, created_at FROM projects WHERE name = ?1")?;
        let mut rows = stmt.query_map([name], |row| {
            let created_at: String = row.get(3)?;
            Ok(Project {
                id: ProjectId::from(row.get::<_, String>(0)?),
                name: ProjectName::from(row.get::<_, String>(1)?),
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

    pub fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at FROM projects ORDER BY created_at ASC",
        )?;
        let projects = stmt
            .query_map([], |row| {
                let created_at: String = row.get(3)?;
                Ok(Project {
                    id: ProjectId::from(row.get::<_, String>(0)?),
                    name: ProjectName::from(row.get::<_, String>(1)?),
                    description: row.get(2)?,
                    created_at: DateTime::parse_from_rfc3339(&created_at)
                        .unwrap_or_default()
                        .with_timezone(&Utc),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    pub fn delete_project(&self, id: &ProjectId) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", [id.as_str()])?;
        Ok(())
    }

    // -- Schedule CRUD --

    pub fn insert_schedule(&self, schedule: &Schedule) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO schedules (id, name, schedule_type, schedule_expr, action, enabled, next_run_at, created_at, run_count, max_runs, check_command, diff_mode, last_check_output)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            (
                schedule.id.as_str(),
                &schedule.name,
                schedule.schedule_type.as_str(),
                &schedule.schedule_expr,
                &schedule.action,
                schedule.enabled,
                schedule.next_run_at.as_ref().map(|dt| dt.to_rfc3339()),
                schedule.created_at.to_rfc3339(),
                schedule.run_count,
                schedule.max_runs,
                schedule.check_command.as_deref(),
                schedule.diff_mode.as_str(),
                schedule.last_check_output.as_deref(),
            ),
        )?;
        Ok(())
    }

    pub fn list_schedules(&self) -> anyhow::Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules ORDER BY created_at ASC"
        ))?;
        let schedules = stmt
            .query_map([], row_to_schedule)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(schedules)
    }

    pub fn list_due_schedules(&self, now: &DateTime<Utc>) -> anyhow::Result<Vec<Schedule>> {
        let now_str = now.to_rfc3339();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules \
                 WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1"
        ))?;
        let schedules = stmt
            .query_map([&now_str], row_to_schedule)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(schedules)
    }

    pub fn update_schedule_after_run(
        &self,
        id: &ScheduleId,
        last_run_at: &DateTime<Utc>,
        next_run_at: Option<&DateTime<Utc>>,
        run_count: i64,
        enabled: bool,
        last_check_output: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE schedules SET last_run_at = ?1, next_run_at = ?2, run_count = ?3, enabled = ?4, last_check_output = ?5
             WHERE id = ?6",
            (
                last_run_at.to_rfc3339(),
                next_run_at.map(|dt| dt.to_rfc3339()),
                run_count,
                enabled,
                last_check_output,
                id.as_str(),
            ),
        )?;
        Ok(())
    }

    /// Update only the last_check_output without firing (for watch schedules that didn't change).
    pub fn update_schedule_check_output(
        &self,
        id: &ScheduleId,
        last_run_at: &DateTime<Utc>,
        next_run_at: Option<&DateTime<Utc>>,
        last_check_output: Option<&str>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE schedules SET last_run_at = ?1, next_run_at = ?2, last_check_output = ?3
             WHERE id = ?4",
            (
                last_run_at.to_rfc3339(),
                next_run_at.map(|dt| dt.to_rfc3339()),
                last_check_output,
                id.as_str(),
            ),
        )?;
        Ok(())
    }

    /// Mark a task's on-idle trigger as fired.
    pub fn mark_on_idle_fired(&self, id: &TaskId) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE tasks SET on_idle_fired = 1 WHERE id = ?1",
            [id.as_str()],
        )?;
        Ok(())
    }

    /// Find a running task by name that has an unfired on-idle trigger.
    pub fn find_task_with_idle_trigger(&self, name: &str) -> anyhow::Result<Option<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE name = ?1 AND status = 'running' AND on_idle IS NOT NULL AND on_idle_fired = 0"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut tasks: Vec<Task> = stmt
            .query_map([name], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks.pop())
    }

    pub fn get_schedule_by_name(&self, name: &str) -> anyhow::Result<Option<Schedule>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE name = ?1"
        ))?;
        let mut rows = stmt.query_map([name], row_to_schedule)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_schedule_by_prefix(&self, prefix: &str) -> anyhow::Result<Option<Schedule>> {
        let pattern = format!("{prefix}%");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE id LIKE ?1"
        ))?;
        let mut schedules: Vec<Schedule> = stmt
            .query_map([&pattern], row_to_schedule)?
            .collect::<Result<Vec<_>, _>>()?;
        if schedules.len() > 1 {
            anyhow::bail!(
                "ambiguous prefix '{prefix}': matches {} schedules",
                schedules.len()
            );
        }
        Ok(schedules.pop())
    }

    pub fn delete_schedule(&self, id: &ScheduleId) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM schedules WHERE id = ?1", [id.as_str()])?;
        Ok(())
    }

    pub fn set_schedule_enabled(&self, id: &ScheduleId, enabled: bool) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE schedules SET enabled = ?1 WHERE id = ?2",
            (enabled, id.as_str()),
        )?;
        Ok(())
    }

    /// Returns visible tasks scoped to a project.
    /// When project_id is None, returns tasks with no project (default/ExO scope).
    /// When Some, returns tasks belonging to that project.
    pub fn list_visible_tasks_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
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
                .query_map([pid.as_str()], row_to_task)?
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

    fn insert_running_task(store: &Store, name: &str) -> TaskId {
        let id = TaskId::generate();
        let task = Task {
            id: id.clone(),
            name: TaskName::from(name.to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            status: TaskStatus::Running,
            tmux_pane: Some(PaneId::from("%1".to_string())),
            tmux_window: Some(WindowId::from("@1".to_string())),
            work_dir: None,
            session_id: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id: None,
            on_complete_success: None,
            on_complete_failure: None,
            on_idle: None,
            on_idle_fired: false,
        };
        store.insert_task(&task).unwrap();
        id
    }

    #[test]
    fn close_task_sets_status_and_timestamp() {
        let store = test_store();
        let id = insert_running_task(&store, "t1");

        let ok = store.close_task(&id, Some("output text")).unwrap();
        assert!(ok);

        let task = store.get_task_by_prefix(id.as_str()).unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.completed_at.is_some());
        assert_eq!(task.output.as_deref(), Some("output text"));
    }

    #[test]
    fn close_task_only_affects_running() {
        let store = test_store();
        let id = insert_running_task(&store, "t2");
        store.complete_task(&id, 0, None).unwrap();

        let ok = store.close_task(&id, None).unwrap();
        assert!(!ok);

        let task = store.get_task_by_prefix(id.as_str()).unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn list_active_excludes_non_running() {
        let store = test_store();
        let _id1 = insert_running_task(&store, "active");
        let id2 = insert_running_task(&store, "done");
        store.complete_task(&id2, 0, None).unwrap();
        let id3 = insert_running_task(&store, "closed");
        store.close_task(&id3, None).unwrap();

        let active = store.list_active_tasks().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active");

        let all = store.list_tasks().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn insert_and_list_messages() {
        let store = test_store();
        let id = insert_running_task(&store, "t1");
        let chat = ChatId::Task(id);

        store
            .insert_message(&chat, MessageRole::System, "initial prompt")
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "hello agent")
            .unwrap();

        let messages = store.list_messages(&chat).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[0].content, "initial prompt");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "hello agent");
    }

    #[test]
    fn list_messages_empty_for_unknown_task() {
        let store = test_store();
        let chat = ChatId::Task(TaskId::generate());
        let messages = store.list_messages(&chat).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn messages_ordered_by_created_at() {
        let store = test_store();
        let id = insert_running_task(&store, "t1");
        let chat = ChatId::Task(id);

        store
            .insert_message(&chat, MessageRole::System, "first")
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "second")
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "third")
            .unwrap();

        let messages = store.list_messages(&chat).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(messages[0].created_at <= messages[1].created_at);
        assert!(messages[1].created_at <= messages[2].created_at);
    }

    // -- Project CRUD tests --

    #[test]
    fn insert_and_get_project_by_name() {
        let store = test_store();
        let pid = ProjectId::generate();
        store
            .insert_project(&pid, "my-project", "a description")
            .unwrap();

        let project = store.get_project_by_name("my-project").unwrap().unwrap();
        assert_eq!(project.id, pid);
        assert_eq!(project.name, "my-project");
        assert_eq!(project.description, "a description");
    }

    #[test]
    fn get_project_by_name_returns_none_for_unknown() {
        let store = test_store();
        let project = store.get_project_by_name("nonexistent").unwrap();
        assert!(project.is_none());
    }

    #[test]
    fn list_projects_empty() {
        let store = test_store();
        let projects = store.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_projects_ordered_by_created_at() {
        let store = test_store();
        let p1 = ProjectId::generate();
        let p2 = ProjectId::generate();
        let p3 = ProjectId::generate();
        store.insert_project(&p1, "alpha", "first").unwrap();
        store.insert_project(&p2, "beta", "second").unwrap();
        store.insert_project(&p3, "gamma", "third").unwrap();

        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
        assert_eq!(projects[2].name, "gamma");
        assert!(projects[0].created_at <= projects[1].created_at);
        assert!(projects[1].created_at <= projects[2].created_at);
    }

    #[test]
    fn insert_project_rejects_duplicate_name() {
        let store = test_store();
        let p1 = ProjectId::generate();
        let p2 = ProjectId::generate();
        store.insert_project(&p1, "dup", "first").unwrap();
        let err = store.insert_project(&p2, "dup", "second");
        assert!(err.is_err());
    }

    #[test]
    fn delete_project_removes_it() {
        let store = test_store();
        let pid = ProjectId::generate();
        store.insert_project(&pid, "doomed", "bye").unwrap();
        assert!(store.get_project_by_name("doomed").unwrap().is_some());

        store.delete_project(&pid).unwrap();
        assert!(store.get_project_by_name("doomed").unwrap().is_none());
        assert!(store.list_projects().unwrap().is_empty());
    }

    // -- list_visible_tasks_for_project tests --

    fn insert_task_with_project(
        store: &Store,
        name: &str,
        project_id: Option<&ProjectId>,
    ) -> TaskId {
        let id = TaskId::generate();
        let task = Task {
            id: id.clone(),
            name: TaskName::from(name.to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            status: TaskStatus::Running,
            tmux_pane: Some(PaneId::from("%1".to_string())),
            tmux_window: Some(WindowId::from("@1".to_string())),
            work_dir: None,
            session_id: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id: project_id.cloned(),
            on_complete_success: None,
            on_complete_failure: None,
            on_idle: None,
            on_idle_fired: false,
        };
        store.insert_task(&task).unwrap();
        id
    }

    #[test]
    fn visible_tasks_null_project_returns_only_unscoped() {
        let store = test_store();
        let proj = ProjectId::generate();
        insert_task_with_project(&store, "no-project", None);
        insert_task_with_project(&store, "has-project", Some(&proj));

        let visible = store.list_visible_tasks_for_project(None).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "no-project");
    }

    #[test]
    fn visible_tasks_with_project_returns_only_matching() {
        let store = test_store();
        let proj_a = ProjectId::generate();
        let proj_b = ProjectId::generate();
        insert_task_with_project(&store, "no-project", None);
        insert_task_with_project(&store, "proj-a-task", Some(&proj_a));
        insert_task_with_project(&store, "proj-b-task", Some(&proj_b));

        let visible = store.list_visible_tasks_for_project(Some(&proj_a)).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "proj-a-task");
    }

    #[test]
    fn visible_tasks_running_sorted_before_completed() {
        let store = test_store();
        let id1 = insert_task_with_project(&store, "completed-task", None);
        store.complete_task(&id1, 0, None).unwrap();
        insert_task_with_project(&store, "running-task", None);

        let visible = store.list_visible_tasks_for_project(None).unwrap();
        assert_eq!(visible.len(), 2);
        // Running tasks should come first
        assert_eq!(visible[0].name, "running-task");
        assert!(visible[0].status.is_running());
        assert_eq!(visible[1].name, "completed-task");
        assert!(!visible[1].status.is_running());
    }

    #[test]
    fn visible_tasks_empty_for_unknown_project() {
        let store = test_store();
        let proj = ProjectId::generate();
        insert_task_with_project(&store, "task", Some(&proj));

        let unknown = ProjectId::generate();
        let visible = store
            .list_visible_tasks_for_project(Some(&unknown))
            .unwrap();
        assert!(visible.is_empty());
    }
}
