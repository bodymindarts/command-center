use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::primitives::{
    ChatId, ClaudeSessionId, MessageRole, PaneId, ProjectId, ProjectName, TaskId, TaskName,
    TaskStatus, WindowId,
};
use crate::task::{Project, Task, TaskMessage};

fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Task> {
    let started_at: String = row.try_get("started_at")?;
    let completed_at: Option<String> = row.try_get("completed_at")?;
    Ok(Task {
        id: TaskId::from(row.try_get::<String, _>("id")?),
        name: TaskName::from(row.try_get::<String, _>("name")?),
        skill_name: row.try_get("skill_name")?,
        params_json: row.try_get("params_json")?,
        status: TaskStatus::from(row.try_get::<String, _>("status")?),
        tmux_pane: row
            .try_get::<Option<String>, _>("tmux_pane")?
            .map(PaneId::from),
        tmux_window: row
            .try_get::<Option<String>, _>("tmux_window")?
            .map(WindowId::from),
        work_dir: row.try_get("work_dir")?,
        session_id: row
            .try_get::<Option<String>, _>("session_id")?
            .map(ClaudeSessionId::from),
        started_at: DateTime::parse_from_rfc3339(&started_at)
            .unwrap_or_default()
            .with_timezone(&Utc),
        completed_at: completed_at.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }),
        exit_code: row.try_get("exit_code")?,
        output: row.try_get("output")?,
        project_id: row
            .try_get::<Option<String>, _>("project_id")?
            .map(ProjectId::from),
    })
}

fn row_to_project(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Project> {
    let created_at: String = row.try_get("created_at")?;
    Ok(Project {
        id: ProjectId::from(row.try_get::<String, _>("id")?),
        name: ProjectName::from(row.try_get::<String, _>("name")?),
        description: row.try_get("description")?,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .unwrap_or_default()
            .with_timezone(&Utc),
    })
}

fn row_to_message(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<TaskMessage> {
    let created_at: String = row.try_get("created_at")?;
    Ok(TaskMessage {
        id: row.try_get("id")?,
        chat_id: row.try_get("task_id")?,
        role: MessageRole::from(row.try_get::<String, _>("role")?),
        content: row.try_get("content")?,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .unwrap_or_default()
            .with_timezone(&Utc),
    })
}

const TASK_COLUMNS: &str =
    "id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir,
     started_at, completed_at, exit_code, output, project_id, session_id";

pub struct Store {
    pool: SqlitePool,
}

impl Store {
    #[cfg(test)]
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .context("failed to open in-memory database")?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .context("failed to run database migrations")?;
        Ok(Self { pool })
    }

    pub async fn open(db_path: &Path) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .with_context(|| format!("failed to run migrations at {}", db_path.display()))?;
        Ok(Self { pool })
    }

    pub async fn insert_task(&self, task: &Task) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO tasks (id, name, skill_name, params_json, status, tmux_pane, tmux_window, work_dir, started_at, project_id, session_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id.as_str())
        .bind(task.name.as_str())
        .bind(&task.skill_name)
        .bind(&task.params_json)
        .bind(task.status.as_str())
        .bind(task.tmux_pane.as_ref().map(|p| p.as_str().to_string()))
        .bind(task.tmux_window.as_ref().map(|w| w.as_str().to_string()))
        .bind(&task.work_dir)
        .bind(task.started_at.to_rfc3339())
        .bind(task.project_id.as_ref().map(|p| p.as_str().to_string()))
        .bind(task.session_id.as_ref().map(|s| s.as_str().to_string()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_task(
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
        sqlx::query(
            "UPDATE tasks SET status = ?, exit_code = ?, completed_at = ?, output = ?
             WHERE id = ?",
        )
        .bind(status)
        .bind(exit_code)
        .bind(&now)
        .bind(output)
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close_task(&self, id: &TaskId, output: Option<&str>) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE tasks SET status = 'closed', completed_at = ?, output = ?
             WHERE id = ? AND status = 'running'",
        )
        .bind(&now)
        .bind(output)
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn reopen_task(
        &self,
        id: &TaskId,
        pane: &PaneId,
        window: &WindowId,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query(
            "UPDATE tasks SET status = 'running', tmux_pane = ?, tmux_window = ?, completed_at = NULL
             WHERE id = ? AND status != 'running'",
        )
        .bind(pane.as_str())
        .bind(window.as_str())
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_task(&self, id: &TaskId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM task_messages WHERE task_id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY started_at DESC");
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_task).collect()
    }

    pub async fn list_active_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'running' ORDER BY started_at DESC"
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_task).collect()
    }

    pub async fn update_task(&self, task: &Task) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE tasks SET status = ?, tmux_pane = ?, tmux_window = ?,
             completed_at = ?, exit_code = ?, output = ?
             WHERE id = ?",
        )
        .bind(task.status.as_str())
        .bind(task.tmux_pane.as_ref().map(|p| p.as_str().to_string()))
        .bind(task.tmux_window.as_ref().map(|w| w.as_str().to_string()))
        .bind(task.completed_at.as_ref().map(|dt| dt.to_rfc3339()))
        .bind(task.exit_code)
        .bind(task.output.as_deref())
        .bind(task.id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_task_by_prefix(&self, prefix: &str) -> anyhow::Result<Option<Task>> {
        let pattern = format!("{prefix}%");
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id LIKE ?");
        let rows = sqlx::query(&sql)
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?;

        if rows.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", rows.len());
        }

        rows.first().map(row_to_task).transpose()
    }

    pub async fn insert_message(
        &self,
        chat_id: &ChatId,
        role: MessageRole,
        content: &str,
    ) -> anyhow::Result<()> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO task_messages (id, task_id, role, content, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(chat_id.as_db_key())
        .bind(role.as_str())
        .bind(content)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_messages(&self, chat_id: &ChatId) -> anyhow::Result<Vec<TaskMessage>> {
        let rows = sqlx::query(
            "SELECT id, task_id, role, content, created_at
             FROM task_messages WHERE task_id = ? ORDER BY created_at ASC",
        )
        .bind(chat_id.as_db_key())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_message).collect()
    }

    // -- Project CRUD --

    pub async fn insert_project(
        &self,
        id: &ProjectId,
        name: &str,
        description: &str,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO projects (id, name, description, created_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id.as_str())
        .bind(name)
        .bind(description)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_project_by_name(&self, name: &str) -> anyhow::Result<Option<Project>> {
        let row =
            sqlx::query("SELECT id, name, description, created_at FROM projects WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        row.as_ref().map(row_to_project).transpose()
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        let rows = sqlx::query(
            "SELECT id, name, description, created_at FROM projects ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_project).collect()
    }

    pub async fn delete_project(&self, id: &ProjectId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
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
        let rows = match project_id {
            Some(pid) => {
                let sql = format!(
                    "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id = ? \
                     ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
                );
                sqlx::query(&sql)
                    .bind(pid.as_str())
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let sql = format!(
                    "SELECT {TASK_COLUMNS} FROM tasks WHERE project_id IS NULL \
                     ORDER BY CASE WHEN status = 'running' THEN 0 ELSE 1 END, started_at DESC"
                );
                sqlx::query(&sql).fetch_all(&self.pool).await?
            }
        };
        rows.iter().map(row_to_task).collect()
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
