use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[cfg(test)]
use crate::primitives::TaskId;
use crate::primitives::{ChatId, MessageRole};
use crate::project::ProjectRepo;
use crate::task::{TaskMessage, TaskRepo};
use crate::watch::WatchRepo;

// ── Store (wraps TaskRepo + ProjectRepo + message CRUD) ──────────────

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
    pub projects: ProjectRepo,
    #[allow(dead_code)]
    pub watches: WatchRepo,
}

impl Store {
    #[cfg(test)]
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .context("failed to open in-memory database")?;
        use job::IncludeMigrations;
        sqlx::migrate!()
            .include_job_migrations()
            .run(&pool)
            .await
            .context("failed to run database migrations")?;
        let tasks = TaskRepo::new(&pool);
        let projects = ProjectRepo::new(&pool);
        let watches = WatchRepo::new(&pool);
        Ok(Self {
            pool,
            tasks,
            projects,
            watches,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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
        use job::IncludeMigrations;
        sqlx::migrate!()
            .include_job_migrations()
            .run(&pool)
            .await
            .with_context(|| format!("failed to run migrations at {}", db_path.display()))?;
        let tasks = TaskRepo::new(&pool);
        let projects = ProjectRepo::new(&pool);
        let watches = WatchRepo::new(&pool);
        Ok(Self {
            pool,
            tasks,
            projects,
            watches,
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{
        ClaudeSessionId, PaneId, ProjectId, ProjectName, TaskName, TaskStatus, WindowId,
    };
    use crate::project::NewProject;
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
            .maybe_find_by_id_prefix(&task.id.to_string())
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

    // -- Project repo tests --

    fn pname(s: &str) -> ProjectName {
        ProjectName::from(s.to_string())
    }

    #[tokio::test]
    async fn create_and_find_project_by_name() {
        let store = test_store().await;
        let new = NewProject {
            id: ProjectId::new(),
            name: pname("my-project"),
            description: "a description".to_string(),
        };
        let project = store.projects.create(new).await.unwrap();
        assert_eq!(project.name, "my-project");
        assert_eq!(project.description, "a description");

        let found = store
            .projects
            .find_by_name(pname("my-project"))
            .await
            .unwrap();
        assert_eq!(found.id, project.id);
        assert_eq!(found.name, "my-project");
        assert_eq!(found.description, "a description");
    }

    #[tokio::test]
    async fn find_project_by_name_returns_err_for_unknown() {
        let store = test_store().await;
        let result = store.projects.find_by_name(pname("nonexistent")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let store = test_store().await;
        let projects = store.projects.list_all().await.unwrap();
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn list_projects_ordered_by_created_at() {
        let store = test_store().await;
        for name in ["alpha", "beta", "gamma"] {
            let new = NewProject {
                id: ProjectId::new(),
                name: pname(name),
                description: String::new(),
            };
            store.projects.create(new).await.unwrap();
        }

        let projects = store.projects.list_all().await.unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
        assert_eq!(projects[2].name, "gamma");
        assert!(projects[0].created_at <= projects[1].created_at);
        assert!(projects[1].created_at <= projects[2].created_at);
    }

    #[tokio::test]
    async fn create_project_rejects_duplicate_name() {
        let store = test_store().await;
        let new1 = NewProject {
            id: ProjectId::new(),
            name: pname("dup"),
            description: String::new(),
        };
        store.projects.create(new1).await.unwrap();

        let new2 = NewProject {
            id: ProjectId::new(),
            name: pname("dup"),
            description: String::new(),
        };
        let err = store.projects.create(new2).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn delete_project_soft_deletes() {
        let store = test_store().await;
        let new = NewProject {
            id: ProjectId::new(),
            name: pname("doomed"),
            description: "bye".to_string(),
        };
        let mut project = store.projects.create(new).await.unwrap();

        // Can find by name after creation.
        let found = store.projects.find_by_name(pname("doomed")).await;
        assert!(found.is_ok());

        let _ = project.delete();
        store.projects.delete(project).await.unwrap();

        // Soft-deleted: find_by_name should error, list should be empty.
        let result = store.projects.find_by_name(pname("doomed")).await;
        assert!(result.is_err());
        assert!(store.projects.list_all().await.unwrap().is_empty());
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
