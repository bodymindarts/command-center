use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::Row;
use rusqlite_migration::{M, Migrations};
use tokio_rusqlite::Connection;

use crate::primitives::{
    ChatId, ClaudeSessionId, MessageRole, PaneId, ProjectId, ProjectName, TaskId, TaskName,
    TaskStatus, WindowId,
};
use crate::task::{Project, Task, TaskMessage};

static MIGRATION_STEPS: [M<'static>; 3] = [
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
    })
}

const TASK_COLUMNS: &str =
    "id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
     started_at, completed_at, exit_code, output, project_id, session_id";

pub struct Store {
    conn: Connection,
}

impl Store {
    #[cfg(test)]
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()
            .await
            .context("failed to open in-memory database")?;
        conn.call(|conn| -> Result<(), rusqlite_migration::Error> {
            MIGRATIONS.to_latest(conn)?;
            Ok(())
        })
        .await
        .context("failed to run database migrations")?;
        Ok(Self { conn })
    }

    pub async fn open(db_path: &Path) -> anyhow::Result<Self> {
        let path = db_path.to_path_buf();
        let conn = Connection::open(&path)
            .await
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        conn.call(|conn| -> Result<(), rusqlite_migration::Error> {
            MIGRATIONS.to_latest(conn)?;
            Ok(())
        })
        .await
        .with_context(|| format!("failed to run migrations at {}", path.display()))?;
        Ok(Self { conn })
    }

    pub async fn insert_task(&self, task: &Task) -> anyhow::Result<()> {
        let task = task.clone();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                conn.execute(
                    "INSERT INTO tasks (id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir, started_at, project_id, session_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    (
                        task.id.as_str(),
                        task.name.as_str(),
                        &task.skill_name,
                        &task.params_json,
                        task.status.as_str(),
                        task.tmux_pane.as_ref().map(|p| p.as_str().to_string()),
                        task.tmux_window.as_ref().map(|w| w.as_str().to_string()),
                        &task.work_dir,
                        task.started_at.to_rfc3339(),
                        task.project_id.as_ref().map(|p| p.as_str().to_string()),
                        task.session_id.as_ref().map(|s| s.as_str().to_string()),
                    ),
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn complete_task(
        &self,
        id: &TaskId,
        exit_code: i32,
        output: Option<&str>,
    ) -> anyhow::Result<()> {
        let id = id.as_str().to_string();
        let output = output.map(|s| s.to_string());
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                let now = Utc::now().to_rfc3339();
                let status = if exit_code == 0 {
                    "completed"
                } else {
                    "failed"
                };
                conn.execute(
                    "UPDATE tasks SET status = ?1, exit_code = ?2, completed_at = ?3, output = ?4
                     WHERE id = ?5",
                    (status, exit_code, &now, output.as_deref(), &id),
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn close_task(&self, id: &TaskId, output: Option<&str>) -> anyhow::Result<bool> {
        let id = id.as_str().to_string();
        let output = output.map(|s| s.to_string());
        let rows = self
            .conn
            .call(move |conn| -> Result<usize, rusqlite::Error> {
                let now = Utc::now().to_rfc3339();
                let rows = conn.execute(
                    "UPDATE tasks SET status = 'closed', completed_at = ?1, output = ?2
                     WHERE id = ?3 AND status = 'running'",
                    (&now, output.as_deref(), &id),
                )?;
                Ok(rows)
            })
            .await?;
        Ok(rows > 0)
    }

    pub async fn reopen_task(
        &self,
        id: &TaskId,
        pane: &PaneId,
        window: &WindowId,
    ) -> anyhow::Result<bool> {
        let id = id.as_str().to_string();
        let pane = pane.as_str().to_string();
        let window = window.as_str().to_string();
        let rows = self
            .conn
            .call(move |conn| -> Result<usize, rusqlite::Error> {
                let rows = conn.execute(
                    "UPDATE tasks SET status = 'running', tmux_pane = ?1, tmux_window = ?2, completed_at = NULL
                     WHERE id = ?3 AND status != 'running'",
                    (&pane, &window, &id),
                )?;
                Ok(rows)
            })
            .await?;
        Ok(rows > 0)
    }

    pub async fn delete_task(&self, id: &TaskId) -> anyhow::Result<()> {
        let id = id.as_str().to_string();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                conn.execute("DELETE FROM task_messages WHERE task_id = ?1", [&id])?;
                conn.execute("DELETE FROM tasks WHERE id = ?1", [&id])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn list_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let tasks = self
            .conn
            .call(|conn| -> Result<Vec<Task>, rusqlite::Error> {
                let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY started_at DESC");
                let mut stmt = conn.prepare(&sql)?;
                let tasks = stmt
                    .query_map([], row_to_task)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(tasks)
            })
            .await?;
        Ok(tasks)
    }

    pub async fn list_active_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let tasks = self
            .conn
            .call(|conn| -> Result<Vec<Task>, rusqlite::Error> {
                let sql = format!(
                    "SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'running' ORDER BY started_at DESC"
                );
                let mut stmt = conn.prepare(&sql)?;
                let tasks = stmt
                    .query_map([], row_to_task)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(tasks)
            })
            .await?;
        Ok(tasks)
    }

    pub async fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        let task = task.clone();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                conn.execute(
                    "UPDATE tasks SET status = ?1, tmux_pane = ?2, tmux_window = ?3,
                     completed_at = ?4, exit_code = ?5, output = ?6
                     WHERE id = ?7",
                    (
                        task.status.as_str(),
                        task.tmux_pane.as_ref().map(|p| p.as_str().to_string()),
                        task.tmux_window.as_ref().map(|w| w.as_str().to_string()),
                        task.completed_at.as_ref().map(|dt| dt.to_rfc3339()),
                        task.exit_code,
                        task.output.as_deref(),
                        task.id.as_str(),
                    ),
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn get_task_by_prefix(&self, prefix: &str) -> anyhow::Result<Option<Task>> {
        let pattern = format!("{prefix}%");
        let tasks = self
            .conn
            .call(move |conn| -> Result<Vec<Task>, rusqlite::Error> {
                let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id LIKE ?1");
                let mut stmt = conn.prepare(&sql)?;
                let tasks: Vec<Task> = stmt
                    .query_map([&pattern], row_to_task)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(tasks)
            })
            .await?;

        if tasks.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", tasks.len());
        }

        Ok(tasks.into_iter().next())
    }

    pub async fn insert_message(
        &self,
        chat_id: &ChatId,
        role: MessageRole,
        content: &str,
    ) -> anyhow::Result<()> {
        let key = chat_id.as_db_key();
        let role_str = role.as_str().to_string();
        let content = content.to_string();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                let id = uuid::Uuid::now_v7().to_string();
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "INSERT INTO task_messages (id, task_id, role, content, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    (&id, &key, &role_str, &content, &now),
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn list_messages(&self, chat_id: &ChatId) -> anyhow::Result<Vec<TaskMessage>> {
        let key = chat_id.as_db_key();
        let messages = self
            .conn
            .call(move |conn| -> Result<Vec<TaskMessage>, rusqlite::Error> {
                let mut stmt = conn.prepare(
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
            })
            .await?;
        Ok(messages)
    }

    // -- Project CRUD --

    pub async fn insert_project(
        &self,
        id: &ProjectId,
        name: &str,
        description: &str,
    ) -> anyhow::Result<()> {
        let id = id.as_str().to_string();
        let name = name.to_string();
        let description = description.to_string();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "INSERT INTO projects (id, name, description, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    (&id, &name, &description, &now),
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn get_project_by_name(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let name = name.to_string();
        let project = self
            .conn
            .call(move |conn| -> Result<Option<Project>, rusqlite::Error> {
                let mut stmt = conn.prepare(
                    "SELECT id, name, description, created_at FROM projects WHERE name = ?1",
                )?;
                let mut rows = stmt.query_map([&name], |row| {
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
            })
            .await?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let projects = self
            .conn
            .call(|conn| -> Result<Vec<Project>, rusqlite::Error> {
                let mut stmt = conn.prepare(
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
            })
            .await?;
        Ok(projects)
    }

    pub async fn delete_project(&self, id: &ProjectId) -> anyhow::Result<()> {
        let id = id.as_str().to_string();
        self.conn
            .call(move |conn| -> Result<(), rusqlite::Error> {
                conn.execute("DELETE FROM projects WHERE id = ?1", [&id])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Returns visible tasks scoped to a project.
    /// When project_id is None, returns tasks with no project (default/ExO scope).
    /// When Some, returns tasks belonging to that project.
    pub async fn list_visible_tasks_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
        let pid = project_id.map(|p| p.as_str().to_string());
        let tasks = self
            .conn
            .call(move |conn| -> Result<Vec<Task>, rusqlite::Error> {
                let sql = match pid {
                    Some(_) => format!(
                        "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id = ?1 \
                         ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
                    ),
                    None => format!(
                        "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id IS NULL \
                         ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
                    ),
                };
                let mut stmt = conn.prepare(&sql)?;
                let tasks = match pid {
                    Some(ref pid) => stmt
                        .query_map([pid.as_str()], row_to_task)?
                        .collect::<Result<Vec<_>, _>>()?,
                    None => stmt
                        .query_map([], row_to_task)?
                        .collect::<Result<Vec<_>, _>>()?,
                };
                Ok(tasks)
            })
            .await?;
        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    async fn insert_running_task(store: &Store, name: &str) -> TaskId {
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
        };
        store.insert_task(&task).await.unwrap();
        id
    }

    #[tokio::test]
    async fn close_task_sets_status_and_timestamp() {
        let store = test_store().await;
        let id = insert_running_task(&store, "t1").await;

        let ok = store.close_task(&id, Some("output text")).await.unwrap();
        assert!(ok);

        let task = store
            .get_task_by_prefix(id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.completed_at.is_some());
        assert_eq!(task.output.as_deref(), Some("output text"));
    }

    #[tokio::test]
    async fn close_task_only_affects_running() {
        let store = test_store().await;
        let id = insert_running_task(&store, "t2").await;
        store.complete_task(&id, 0, None).await.unwrap();

        let ok = store.close_task(&id, None).await.unwrap();
        assert!(!ok);

        let task = store
            .get_task_by_prefix(id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn list_active_excludes_non_running() {
        let store = test_store().await;
        let _id1 = insert_running_task(&store, "active").await;
        let id2 = insert_running_task(&store, "done").await;
        store.complete_task(&id2, 0, None).await.unwrap();
        let id3 = insert_running_task(&store, "closed").await;
        store.close_task(&id3, None).await.unwrap();

        let active = store.list_active_tasks().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active");

        let all = store.list_tasks().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn insert_and_list_messages() {
        let store = test_store().await;
        let id = insert_running_task(&store, "t1").await;
        let chat = ChatId::Task(id);

        store
            .insert_message(&chat, MessageRole::System, "initial prompt")
            .await
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "hello agent")
            .await
            .unwrap();

        let messages = store.list_messages(&chat).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[0].content, "initial prompt");
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content, "hello agent");
    }

    #[tokio::test]
    async fn list_messages_empty_for_unknown_task() {
        let store = test_store().await;
        let chat = ChatId::Task(TaskId::generate());
        let messages = store.list_messages(&chat).await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn messages_ordered_by_created_at() {
        let store = test_store().await;
        let id = insert_running_task(&store, "t1").await;
        let chat = ChatId::Task(id);

        store
            .insert_message(&chat, MessageRole::System, "first")
            .await
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "second")
            .await
            .unwrap();
        store
            .insert_message(&chat, MessageRole::User, "third")
            .await
            .unwrap();

        let messages = store.list_messages(&chat).await.unwrap();
        assert_eq!(messages.len(), 3);
        assert!(messages[0].created_at <= messages[1].created_at);
        assert!(messages[1].created_at <= messages[2].created_at);
    }

    // -- Project CRUD tests --

    #[tokio::test]
    async fn insert_and_get_project_by_name() {
        let store = test_store().await;
        let pid = ProjectId::generate();
        store
            .insert_project(&pid, "my-project", "a description")
            .await
            .unwrap();

        let project = store
            .get_project_by_name("my-project")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(project.id, pid);
        assert_eq!(project.name, "my-project");
        assert_eq!(project.description, "a description");
    }

    #[tokio::test]
    async fn get_project_by_name_returns_none_for_unknown() {
        let store = test_store().await;
        let project = store.get_project_by_name("nonexistent").await.unwrap();
        assert!(project.is_none());
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let store = test_store().await;
        let projects = store.list_projects().await.unwrap();
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn list_projects_ordered_by_created_at() {
        let store = test_store().await;
        let p1 = ProjectId::generate();
        let p2 = ProjectId::generate();
        let p3 = ProjectId::generate();
        store.insert_project(&p1, "alpha", "first").await.unwrap();
        store.insert_project(&p2, "beta", "second").await.unwrap();
        store.insert_project(&p3, "gamma", "third").await.unwrap();

        let projects = store.list_projects().await.unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
        assert_eq!(projects[2].name, "gamma");
        assert!(projects[0].created_at <= projects[1].created_at);
        assert!(projects[1].created_at <= projects[2].created_at);
    }

    #[tokio::test]
    async fn insert_project_rejects_duplicate_name() {
        let store = test_store().await;
        let p1 = ProjectId::generate();
        let p2 = ProjectId::generate();
        store.insert_project(&p1, "dup", "first").await.unwrap();
        let err = store.insert_project(&p2, "dup", "second").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn delete_project_removes_it() {
        let store = test_store().await;
        let pid = ProjectId::generate();
        store.insert_project(&pid, "doomed", "bye").await.unwrap();
        assert!(store.get_project_by_name("doomed").await.unwrap().is_some());

        store.delete_project(&pid).await.unwrap();
        assert!(store.get_project_by_name("doomed").await.unwrap().is_none());
        assert!(store.list_projects().await.unwrap().is_empty());
    }

    // -- list_visible_tasks_for_project tests --

    async fn insert_task_with_project(
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
        };
        store.insert_task(&task).await.unwrap();
        id
    }

    #[tokio::test]
    async fn visible_tasks_null_project_returns_only_unscoped() {
        let store = test_store().await;
        let proj = ProjectId::generate();
        insert_task_with_project(&store, "no-project", None).await;
        insert_task_with_project(&store, "has-project", Some(&proj)).await;

        let visible = store.list_visible_tasks_for_project(None).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "no-project");
    }

    #[tokio::test]
    async fn visible_tasks_with_project_returns_only_matching() {
        let store = test_store().await;
        let proj_a = ProjectId::generate();
        let proj_b = ProjectId::generate();
        insert_task_with_project(&store, "no-project", None).await;
        insert_task_with_project(&store, "proj-a-task", Some(&proj_a)).await;
        insert_task_with_project(&store, "proj-b-task", Some(&proj_b)).await;

        let visible = store
            .list_visible_tasks_for_project(Some(&proj_a))
            .await
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "proj-a-task");
    }

    #[tokio::test]
    async fn visible_tasks_running_sorted_before_completed() {
        let store = test_store().await;
        let id1 = insert_task_with_project(&store, "completed-task", None).await;
        store.complete_task(&id1, 0, None).await.unwrap();
        insert_task_with_project(&store, "running-task", None).await;

        let visible = store.list_visible_tasks_for_project(None).await.unwrap();
        assert_eq!(visible.len(), 2);
        // Running tasks should come first
        assert_eq!(visible[0].name, "running-task");
        assert!(visible[0].status.is_running());
        assert_eq!(visible[1].name, "completed-task");
        assert!(!visible[1].status.is_running());
    }

    #[tokio::test]
    async fn visible_tasks_empty_for_unknown_project() {
        let store = test_store().await;
        let proj = ProjectId::generate();
        insert_task_with_project(&store, "task", Some(&proj)).await;

        let unknown = ProjectId::generate();
        let visible = store
            .list_visible_tasks_for_project(Some(&unknown))
            .await
            .unwrap();
        assert!(visible.is_empty());
    }
}
