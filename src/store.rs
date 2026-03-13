use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::primitives::{ChatId, MessageRole, ProjectId, ProjectName, TaskId};
use crate::task::{Project, TaskMessage, TaskRepo};

// ── Store (wraps TaskRepo + message/project CRUD) ────────────────────

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

pub struct Store {
    pool: SqlitePool,
    pub tasks: TaskRepo,
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
        let tasks = TaskRepo::new(&pool);
        Ok(Self { pool, tasks })
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
        let tasks = TaskRepo::new(&pool);
        Ok(Self { pool, tasks })
    }

    // -- Message CRUD (not event-sourced) --

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

    pub async fn list_messages_last(
        &self,
        chat_id: &ChatId,
        limit: u32,
    ) -> anyhow::Result<Vec<TaskMessage>> {
        let rows = sqlx::query(
            "SELECT id, task_id, role, content, created_at FROM (
                 SELECT id, task_id, role, content, created_at
                 FROM task_messages WHERE task_id = ? ORDER BY created_at DESC LIMIT ?
             ) ORDER BY created_at ASC",
        )
        .bind(chat_id.as_db_key())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_message).collect()
    }

    pub async fn delete_task_messages(&self, task_id: &TaskId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM task_messages WHERE task_id = ?")
            .bind(task_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -- Project CRUD (not event-sourced) --

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
        .bind(id.to_string())
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
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{ClaudeSessionId, PaneId, TaskName, TaskStatus, WindowId};
    use crate::task::{NewTask, Task};

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    async fn create_running_task(store: &Store, name: &str) -> Task {
        let new_task = NewTask {
            id: TaskId::new(),
            name: TaskName::from(name.to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            work_dir: None,
            session_id: ClaudeSessionId::new(),
            project_id: None,
        };
        let mut task = store.tasks.create(new_task).await.unwrap();
        let _ = task.launch_agent(
            PaneId::from("%1".to_string()),
            WindowId::from("@1".to_string()),
        );
        store.tasks.update(&mut task).await.unwrap();
        task
    }

    async fn create_task_with_project(
        store: &Store,
        name: &str,
        project_id: Option<&ProjectId>,
    ) -> Task {
        let new_task = NewTask {
            id: TaskId::new(),
            name: TaskName::from(name.to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            work_dir: None,
            session_id: ClaudeSessionId::new(),
            project_id: project_id.copied(),
        };
        let mut task = store.tasks.create(new_task).await.unwrap();
        let _ = task.launch_agent(
            PaneId::from("%1".to_string()),
            WindowId::from("@1".to_string()),
        );
        store.tasks.update(&mut task).await.unwrap();
        task
    }

    #[tokio::test]
    async fn close_task_sets_status_and_output() {
        let store = test_store().await;
        let mut task = create_running_task(&store, "t1").await;

        let result = task.close(Some("output text".to_string()));
        assert!(result.did_execute());
        store.tasks.update(&mut task).await.unwrap();

        let loaded = store
            .tasks
            .find_by_id_prefix(&task.id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, TaskStatus::Closed);
        assert!(loaded.completed_at.is_some());
        assert_eq!(loaded.output.as_deref(), Some("output text"));
    }

    #[tokio::test]
    async fn close_task_only_affects_running() {
        let store = test_store().await;
        let mut task = create_running_task(&store, "t2").await;

        let _ = task.complete(0, None).unwrap();
        store.tasks.update(&mut task).await.unwrap();

        let result = task.close(None);
        assert!(result.was_already_applied());
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn list_active_excludes_non_running() {
        let store = test_store().await;
        let _task1 = create_running_task(&store, "active").await;
        let mut task2 = create_running_task(&store, "done").await;
        let _ = task2.complete(0, None).unwrap();
        store.tasks.update(&mut task2).await.unwrap();
        let mut task3 = create_running_task(&store, "closed").await;
        let _ = task3.close(None);
        store.tasks.update(&mut task3).await.unwrap();

        let active = store.tasks.list_active().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "active");

        let all = store.tasks.list_all().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn insert_and_list_messages() {
        let store = test_store().await;
        let task = create_running_task(&store, "t1").await;
        let chat = ChatId::Task(task.id);

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
        let chat = ChatId::Task(TaskId::new());
        let messages = store.list_messages(&chat).await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn messages_ordered_by_created_at() {
        let store = test_store().await;
        let task = create_running_task(&store, "t1").await;
        let chat = ChatId::Task(task.id);

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
        let pid = ProjectId::new();
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
        let p1 = ProjectId::new();
        let p2 = ProjectId::new();
        let p3 = ProjectId::new();
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
        let p1 = ProjectId::new();
        let p2 = ProjectId::new();
        store.insert_project(&p1, "dup", "first").await.unwrap();
        let err = store.insert_project(&p2, "dup", "second").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn delete_project_removes_it() {
        let store = test_store().await;
        let pid = ProjectId::new();
        store.insert_project(&pid, "doomed", "bye").await.unwrap();
        assert!(store.get_project_by_name("doomed").await.unwrap().is_some());

        store.delete_project(&pid).await.unwrap();
        assert!(store.get_project_by_name("doomed").await.unwrap().is_none());
        assert!(store.list_projects().await.unwrap().is_empty());
    }

    // -- list_visible_for_project tests --

    #[tokio::test]
    async fn visible_tasks_null_project_returns_only_unscoped() {
        let store = test_store().await;
        let proj = ProjectId::new();
        create_task_with_project(&store, "no-project", None).await;
        create_task_with_project(&store, "has-project", Some(&proj)).await;

        let visible = store.tasks.list_visible_for_project(None).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "no-project");
    }

    #[tokio::test]
    async fn visible_tasks_with_project_returns_only_matching() {
        let store = test_store().await;
        let proj_a = ProjectId::new();
        let proj_b = ProjectId::new();
        create_task_with_project(&store, "no-project", None).await;
        create_task_with_project(&store, "proj-a-task", Some(&proj_a)).await;
        create_task_with_project(&store, "proj-b-task", Some(&proj_b)).await;

        let visible = store
            .tasks
            .list_visible_for_project(Some(&proj_a))
            .await
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "proj-a-task");
    }

    #[tokio::test]
    async fn visible_tasks_running_sorted_before_completed() {
        let store = test_store().await;
        let mut task1 = create_task_with_project(&store, "completed-task", None).await;
        let _ = task1.complete(0, None).unwrap();
        store.tasks.update(&mut task1).await.unwrap();
        create_task_with_project(&store, "running-task", None).await;

        let visible = store.tasks.list_visible_for_project(None).await.unwrap();
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
        let proj = ProjectId::new();
        create_task_with_project(&store, "task", Some(&proj)).await;

        let unknown = ProjectId::new();
        let visible = store
            .tasks
            .list_visible_for_project(Some(&unknown))
            .await
            .unwrap();
        assert!(visible.is_empty());
    }
}
