use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use es_entity::*;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::primitives::{ChatId, MessageRole, ProjectId, ProjectName, TaskId};
use crate::task::{NewTask, Project, Task, TaskMessage};

// ── TaskRepo (manual implementation using es-entity types) ──────────

pub struct TaskRepo {
    pool: SqlitePool,
}

impl TaskRepo {
    fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// Create a new task: persist initial events and insert index row.
    pub async fn create(&self, new_task: NewTask) -> anyhow::Result<Task> {
        let events = new_task.into_events();
        let task = Task::try_from_events(events)?;

        // Write index row first (task_events has FK to tasks)
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO tasks (id, created_at, name, skill_name, params_json, status, \
             tmux_pane, tmux_window, work_dir, session_id, started_at, completed_at, \
             exit_code, output, project_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id.to_string())
        .bind(&now)
        .bind(&task.name)
        .bind(&task.skill_name)
        .bind(&task.params_json)
        .bind(task.status)
        .bind(task.tmux_pane.as_ref())
        .bind(task.tmux_window.as_ref())
        .bind(&task.work_dir)
        .bind(task.session_id.map(|s| s.to_string()))
        .bind(task.started_at.to_rfc3339())
        .bind(task.completed_at.as_ref().map(|dt| dt.to_rfc3339()))
        .bind(task.exit_code)
        .bind(task.output.as_deref())
        .bind(task.project_id.map(|p| p.to_string()))
        .execute(&self.pool)
        .await?;

        // Write initial events
        self.persist_new_events(&task).await?;

        // Re-load to get persisted event metadata
        self.find_by_id(task.id).await
    }

    /// Persist new events and update the index row.
    pub async fn update(&self, task: &mut Task) -> anyhow::Result<()> {
        if !task.events().any_new() {
            return Ok(());
        }

        self.persist_new_events(task).await?;

        // Update index row
        sqlx::query(
            "UPDATE tasks SET status = ?, tmux_pane = ?, tmux_window = ?, \
             completed_at = ?, exit_code = ?, output = ?, name = ?, \
             session_id = ?, work_dir = ?, project_id = ? \
             WHERE id = ?",
        )
        .bind(task.status)
        .bind(task.tmux_pane.as_ref())
        .bind(task.tmux_window.as_ref())
        .bind(task.completed_at.as_ref().map(|dt| dt.to_rfc3339()))
        .bind(task.exit_code)
        .bind(task.output.as_deref())
        .bind(&task.name)
        .bind(task.session_id.map(|s| s.to_string()))
        .bind(&task.work_dir)
        .bind(task.project_id.map(|p| p.to_string()))
        .bind(task.id.to_string())
        .execute(&self.pool)
        .await?;

        // Mark events as persisted
        let now = Utc::now();
        task.events_mut().mark_new_events_persisted_at(now);
        Ok(())
    }

    /// Find a task by exact ID, hydrating from events.
    pub async fn find_by_id(&self, id: TaskId) -> anyhow::Result<Task> {
        let rows = self.load_events_for_id(&id).await?;
        EntityEvents::load_first::<Task>(rows)?
            .ok_or_else(|| anyhow::anyhow!("task {} not found", id))
    }

    /// Find a task by ID prefix. Returns None if no match, errors if ambiguous.
    pub async fn find_by_id_prefix(&self, prefix: &str) -> anyhow::Result<Option<Task>> {
        let pattern = format!("{prefix}%");
        let ids = sqlx::query_scalar::<_, String>("SELECT id FROM tasks WHERE id LIKE ?")
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?;

        if ids.len() > 1 {
            anyhow::bail!("ambiguous prefix '{prefix}': matches {} tasks", ids.len());
        }

        match ids.into_iter().next() {
            Some(id_str) => {
                let id: TaskId = id_str.into();
                Ok(Some(self.find_by_id(id).await?))
            }
            None => Ok(None),
        }
    }

    /// List all tasks ordered by created_at DESC.
    pub async fn list_all(&self) -> anyhow::Result<Vec<Task>> {
        self.list_with_filter("1=1").await
    }

    /// List tasks with status = 'running'.
    pub async fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        self.list_with_filter("t.status = 'running'").await
    }

    /// List tasks scoped to a project.
    pub async fn list_visible_for_project(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<Task>> {
        let sql_base = "SELECT e.id, e.sequence, e.event_type, e.event, e.recorded_at \
                        FROM task_events e \
                        INNER JOIN tasks t ON t.id = e.id";
        let order = "ORDER BY CASE WHEN t.status = 'running' THEN 0 ELSE 1 END, \
                     t.created_at DESC, e.id, e.sequence";

        let rows = match project_id {
            Some(pid) => {
                let pid_str = pid.to_string();
                sqlx::query_as::<_, EventRow>(&format!("{sql_base} WHERE t.project_id = ? {order}"))
                    .bind(&pid_str)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query_as::<_, EventRow>(&format!(
                    "{sql_base} WHERE t.project_id IS NULL {order}"
                ))
                .fetch_all(&self.pool)
                .await?
            }
        };

        let generic: Vec<_> = rows.into_iter().map(|r| r.into_generic()).collect();
        let (tasks, _) = EntityEvents::load_n::<Task>(generic, usize::MAX)?;
        Ok(tasks)
    }

    /// Delete a task and its events.
    pub async fn delete_task(&self, id: TaskId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM task_events WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── Internal helpers ──

    async fn persist_new_events(&self, task: &Task) -> anyhow::Result<()> {
        let events = task.events();
        if !events.any_new() {
            return Ok(());
        }

        let id_str = task.id.to_string();
        let now = Utc::now().to_rfc3339();
        let base_sequence = events.len_persisted() as i32;
        let event_types = events.new_event_types();
        let event_jsons = events.serialize_new_events();

        for (i, (event_type, event_json)) in event_types.iter().zip(event_jsons.iter()).enumerate()
        {
            let seq = base_sequence + 1 + i as i32;
            let json_str = serde_json::to_string(event_json)?;
            sqlx::query(
                "INSERT INTO task_events (id, sequence, event_type, event, recorded_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id_str)
            .bind(seq)
            .bind(event_type)
            .bind(&json_str)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    async fn load_events_for_id(&self, id: &TaskId) -> anyhow::Result<Vec<GenericEvent<TaskId>>> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT id, sequence, event_type, event, recorded_at \
             FROM task_events WHERE id = ? ORDER BY sequence",
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.into_generic()).collect())
    }

    async fn list_with_filter(&self, filter: &str) -> anyhow::Result<Vec<Task>> {
        let sql = format!(
            "SELECT e.id, e.sequence, e.event_type, e.event, e.recorded_at \
             FROM task_events e \
             INNER JOIN tasks t ON t.id = e.id \
             WHERE {filter} \
             ORDER BY t.created_at DESC, e.id, e.sequence"
        );
        let rows = sqlx::query_as::<_, EventRow>(&sql)
            .fetch_all(&self.pool)
            .await?;
        let generic: Vec<_> = rows.into_iter().map(|r| r.into_generic()).collect();
        let (tasks, _) = EntityEvents::load_n::<Task>(generic, usize::MAX)?;
        Ok(tasks)
    }
}

// Helper struct for mapping sqlx rows to GenericEvent.
#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    sequence: i32,
    #[allow(dead_code)]
    event_type: String,
    event: String,
    recorded_at: String,
}

impl EventRow {
    fn into_generic(self) -> GenericEvent<TaskId> {
        let entity_id: TaskId = self.id.into();
        let event: serde_json::Value =
            serde_json::from_str(&self.event).unwrap_or(serde_json::Value::Null);
        let recorded_at = DateTime::parse_from_rfc3339(&self.recorded_at)
            .unwrap_or_default()
            .with_timezone(&Utc);

        GenericEvent {
            entity_id,
            sequence: self.sequence,
            event,
            context: None,
            recorded_at,
        }
    }
}

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
        task.launch_agent(
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
        task.launch_agent(
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

        let closed = task.close(Some("output text".to_string())).unwrap();
        assert!(closed);
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

        task.complete(0, None).unwrap();
        store.tasks.update(&mut task).await.unwrap();

        let closed = task.close(None).unwrap();
        assert!(!closed);
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn list_active_excludes_non_running() {
        let store = test_store().await;
        let _task1 = create_running_task(&store, "active").await;
        let mut task2 = create_running_task(&store, "done").await;
        task2.complete(0, None).unwrap();
        store.tasks.update(&mut task2).await.unwrap();
        let mut task3 = create_running_task(&store, "closed").await;
        task3.close(None).unwrap();
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
        task1.complete(0, None).unwrap();
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
