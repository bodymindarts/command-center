use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::config::Paths;
use crate::primitives::TaskId;
use crate::runtime::Runtime;
use crate::skill::SkillFile;
use crate::store::Store;
use crate::task::{Task, TaskMessage};

#[derive(Debug)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub params: Vec<String>,
}

#[derive(Debug)]
pub struct SpawnOutput {
    pub task_id: TaskId,
    pub task_name: String,
    pub skill_name: String,
    pub window_id: String,
}

#[derive(Debug)]
pub struct CloseOutput {
    pub task_id: TaskId,
    pub task_name: String,
}

#[derive(Debug)]
pub struct SendOutput {
    pub task_id: TaskId,
    pub task_name: String,
}

#[derive(Debug)]
pub struct LogOutput {
    pub task: Task,
    pub messages: Vec<TaskMessage>,
    pub live_output: Option<String>,
}

const EXO_CHAT_ID: &str = "exo";

pub struct TaskService<'a, R: Runtime> {
    store: &'a Store,
    runtime: &'a R,
    paths: &'a Paths,
}

impl<'a, R: Runtime> TaskService<'a, R> {
    pub fn new(store: &'a Store, runtime: &'a R, paths: &'a Paths) -> Self {
        Self {
            store,
            runtime,
            paths,
        }
    }

    pub fn spawn(
        &self,
        task_name: &str,
        skill_name: &str,
        params: Vec<(String, String)>,
    ) -> Result<SpawnOutput> {
        let skill = SkillFile::load(&self.paths.skills_dir, skill_name)?;

        let params_map: HashMap<String, String> = params.into_iter().collect();
        skill.validate_params(&params_map)?;

        let rendered = skill.render_prompt(&params_map)?;

        let id = TaskId::generate();
        let worktree_name = format!("{task_name}-{}", id.short());
        let worktree_path = self
            .runtime
            .create_worktree(&self.paths.root, &worktree_name)?;

        let task = Task::new(id, task_name, skill_name, &params_map, &worktree_path);

        self.store.insert_task(&task)?;
        self.store
            .insert_message(task.id.as_str(), "system", &rendered)?;

        let result = self
            .runtime
            .spawn_agent(task_name, &rendered, &worktree_path)?;
        self.store
            .update_tmux_pane(task.id.as_str(), &result.pane_id)?;
        self.store
            .update_tmux_window(task.id.as_str(), &result.window_id)?;

        Ok(SpawnOutput {
            task_id: task.id,
            task_name: task_name.to_string(),
            skill_name: skill_name.to_string(),
            window_id: result.window_id,
        })
    }

    pub fn close(&self, id_prefix: &str) -> Result<CloseOutput> {
        let task = self.resolve_task(id_prefix)?;

        if !task.status.is_running() {
            bail!(
                "task {} ({}) is '{}', not 'running'",
                task.name,
                task.id.short(),
                task.status
            );
        }

        let output = task
            .tmux_pane
            .as_deref()
            .and_then(|pane| self.runtime.capture_pane_output(pane).ok());

        if let Some(window_id) = &task.tmux_window {
            let _ = self.runtime.kill_tmux_window(window_id);
        }

        let closed = self.store.close_task(task.id.as_str(), output.as_deref())?;
        if !closed {
            bail!("failed to close task {} ({})", task.name, task.id.short());
        }

        Ok(CloseOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub fn delete(&self, task_id: &str) -> Result<()> {
        let task = self.resolve_task(task_id)?;

        if task.status.is_running() {
            if let Some(window_id) = &task.tmux_window {
                let _ = self.runtime.kill_tmux_window(window_id);
            }
            let _ = self.store.close_task(task.id.as_str(), None);
        }

        self.store.delete_task(task.id.as_str())
    }

    pub fn reopen(&self, task_id: &str) -> Result<String> {
        let task = self.resolve_task(task_id)?;

        if task.status.is_running() {
            bail!(
                "task {} ({}) is already running",
                task.name,
                task.id.short()
            );
        }

        let work_dir = task
            .work_dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no work_dir", task.id.short()))?;

        let work_dir = std::path::Path::new(work_dir);
        if !work_dir.is_dir() {
            bail!("worktree directory {} no longer exists", work_dir.display(),);
        }

        let result = self.runtime.resume_agent(&task.name, work_dir)?;

        self.store
            .reopen_task(task.id.as_str(), &result.pane_id, &result.window_id)?;

        Ok(result.window_id)
    }

    pub fn send(&self, id_prefix: &str, message: &str) -> Result<SendOutput> {
        let task = self.resolve_task(id_prefix)?;

        let pane_id = task
            .tmux_pane
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux pane", task.id.short()))?;

        self.runtime.send_keys_to_pane(pane_id, message)?;
        self.store
            .insert_message(task.id.as_str(), "user", message)?;

        Ok(SendOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub fn send_by_id(&self, task_id: &str, pane_id: &str, message: &str) {
        let _ = self.runtime.send_keys_to_pane(pane_id, message);
        let _ = self.store.insert_message(task_id, "user", message);
    }

    pub fn goto(&self, id_prefix: &str) -> Result<()> {
        let task = self.resolve_task(id_prefix)?;

        let window_id = task
            .tmux_window
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux window", task.id.short()))?;

        self.runtime.select_window(window_id)
    }

    pub fn goto_window(&self, window_id: &str) {
        let _ = self.runtime.select_window(window_id);
    }

    pub fn log(&self, id_prefix: &str) -> Result<LogOutput> {
        let task = self.resolve_task(id_prefix)?;
        let messages = self.store.list_messages(task.id.as_str())?;

        let live_output = if task.status.is_running() {
            task.tmux_pane
                .as_deref()
                .and_then(|pane| self.runtime.capture_pane_output(pane).ok())
        } else {
            None
        };

        Ok(LogOutput {
            task,
            messages,
            live_output,
        })
    }

    pub fn list_active(&self) -> Result<Vec<Task>> {
        self.store.list_active_tasks()
    }

    pub fn list_all(&self) -> Result<Vec<Task>> {
        self.store.list_tasks()
    }

    pub fn messages(&self, task_id: &str) -> Result<Vec<TaskMessage>> {
        self.store.list_messages(task_id)
    }

    pub fn list_skills(&self) -> Result<Vec<SkillSummary>> {
        let mut skills = Vec::new();
        let entries = std::fs::read_dir(&self.paths.skills_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if let Ok(skill) = SkillFile::load(&self.paths.skills_dir, &name) {
                skills.push(SkillSummary {
                    name: skill.skill.name,
                    description: skill.skill.description,
                    params: skill.skill.params.iter().map(|p| p.name.clone()).collect(),
                });
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(skills)
    }

    pub fn capture_pane(&self, pane_id: &str) -> Option<String> {
        self.runtime.capture_pane_output(pane_id).ok()
    }

    pub fn insert_exo_message(&self, role: &str, content: &str) -> Result<()> {
        self.store.insert_message(EXO_CHAT_ID, role, content)
    }

    pub fn exo_messages(&self) -> Result<Vec<TaskMessage>> {
        self.store.list_messages(EXO_CHAT_ID)
    }

    pub fn complete(&self, id: &str, exit_code: i32, output: Option<&str>) -> Result<()> {
        self.store.complete_task(id, exit_code, output)
    }

    fn resolve_task(&self, id_prefix: &str) -> Result<Task> {
        self.store
            .get_task_by_prefix(id_prefix)?
            .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use anyhow::{Result, bail};

    use chrono::Utc;

    use crate::primitives::{TaskId, TaskStatus};
    use crate::runtime::{Runtime, SpawnResult};
    use crate::store::Store;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        CreateWorktree {
            name: String,
        },
        SpawnAgent {
            task_name: String,
        },
        ResumeAgent {
            task_name: String,
            work_dir: PathBuf,
        },
        SendKeys {
            pane_id: String,
            message: String,
        },
        CaptureOutput {
            pane_id: String,
        },
        KillWindow {
            window_id: String,
        },
        SelectWindow {
            window_id: String,
        },
    }

    struct FakeRuntime {
        calls: RefCell<Vec<Call>>,
        worktree_dir: PathBuf,
        spawn_window_id: String,
        spawn_pane_id: String,
        capture_result: RefCell<Option<String>>,
        kill_should_fail: RefCell<bool>,
    }

    impl FakeRuntime {
        fn new(worktree_dir: &Path) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                worktree_dir: worktree_dir.to_path_buf(),
                spawn_window_id: "@fake-win".to_string(),
                spawn_pane_id: "%fake-pane".to_string(),
                capture_result: RefCell::new(Some("captured output".to_string())),
                kill_should_fail: RefCell::new(false),
            }
        }
    }

    impl Runtime for FakeRuntime {
        fn create_worktree(&self, _repo_root: &Path, name: &str) -> Result<PathBuf> {
            self.calls.borrow_mut().push(Call::CreateWorktree {
                name: name.to_string(),
            });
            let path = self.worktree_dir.join(name);
            std::fs::create_dir_all(&path)?;
            Ok(path)
        }

        fn spawn_agent(
            &self,
            task_name: &str,
            _prompt: &str,
            _work_dir: &Path,
        ) -> Result<SpawnResult> {
            self.calls.borrow_mut().push(Call::SpawnAgent {
                task_name: task_name.to_string(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn resume_agent(&self, task_name: &str, work_dir: &Path) -> Result<SpawnResult> {
            self.calls.borrow_mut().push(Call::ResumeAgent {
                task_name: task_name.to_string(),
                work_dir: work_dir.to_path_buf(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> Result<()> {
            self.calls.borrow_mut().push(Call::SendKeys {
                pane_id: pane_id.to_string(),
                message: message.to_string(),
            });
            Ok(())
        }

        fn capture_pane_output(&self, pane_id: &str) -> Result<String> {
            self.calls.borrow_mut().push(Call::CaptureOutput {
                pane_id: pane_id.to_string(),
            });
            self.capture_result
                .borrow()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no capture result"))
        }

        fn kill_tmux_window(&self, window_id: &str) -> Result<()> {
            self.calls.borrow_mut().push(Call::KillWindow {
                window_id: window_id.to_string(),
            });
            if *self.kill_should_fail.borrow() {
                bail!("kill failed");
            }
            Ok(())
        }

        fn select_window(&self, window_id: &str) -> Result<()> {
            self.calls.borrow_mut().push(Call::SelectWindow {
                window_id: window_id.to_string(),
            });
            Ok(())
        }
    }

    fn test_paths(tmp: &Path) -> Paths {
        let skills_dir = tmp.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // Write a minimal noop skill
        std::fs::write(
            skills_dir.join("noop.toml"),
            r#"
[skill]
name = "noop"
description = "do nothing"
params = []

[agent]
allowed_tools = []

[template]
prompt = "noop prompt"
"#,
        )
        .unwrap();

        Paths {
            root: tmp.to_path_buf(),
            skills_dir,
            data_dir: tmp.join("data"),
            db_path: tmp.join("data/cc.db"),
        }
    }

    fn spawn_test_task(service: &TaskService<impl Runtime>) -> SpawnOutput {
        service
            .spawn("test-task", "noop", vec![])
            .expect("spawn should succeed")
    }

    #[test]
    fn spawn_creates_task_and_calls_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let output = spawn_test_task(&service);

        assert_eq!(output.task_name, "test-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        // Verify task is in store
        let tasks = store.list_active_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "test-task");
        assert_eq!(tasks[0].tmux_pane.as_deref(), Some("%fake-pane"));

        // Verify system message recorded
        let messages = store.list_messages(tasks[0].id.as_str()).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "system");

        // Verify call order
        let calls = runtime.calls.borrow();
        assert!(matches!(calls[0], Call::CreateWorktree { .. }));
        assert!(matches!(calls[1], Call::SpawnAgent { .. }));
    }

    #[test]
    fn close_captures_output_before_killing_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        let result = service.close(spawned.task_id.as_str()).unwrap();

        assert_eq!(result.task_name, "test-task");

        // Verify task is closed with output
        let task = store
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.output.as_deref(), Some("captured output"));

        // Verify call order: CaptureOutput before KillWindow
        let calls = runtime.calls.borrow();
        let capture_pos = calls
            .iter()
            .position(|c| matches!(c, Call::CaptureOutput { .. }))
            .unwrap();
        let kill_pos = calls
            .iter()
            .position(|c| matches!(c, Call::KillWindow { .. }))
            .unwrap();
        assert!(capture_pos < kill_pos);
    }

    #[test]
    fn close_rejects_non_running_task() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        let task = store
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        service.complete(task.id.as_str(), 0, None).unwrap();

        let err = service.close(spawned.task_id.as_str());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not 'running'"));
    }

    #[test]
    fn close_succeeds_even_if_kill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        *runtime.kill_should_fail.borrow_mut() = true;
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        let result = service.close(spawned.task_id.as_str());
        assert!(result.is_ok());
    }

    #[test]
    fn send_dispatches_and_records_message() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        let result = service
            .send(spawned.task_id.as_str(), "hello agent")
            .unwrap();

        assert_eq!(result.task_name, "test-task");

        // Verify SendKeys call
        let calls = runtime.calls.borrow();
        assert!(calls.iter().any(|c| matches!(c,
            Call::SendKeys { pane_id, message }
            if pane_id == "%fake-pane" && message == "hello agent"
        )));

        // Verify user message in store
        let task = store
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        let messages = store.list_messages(task.id.as_str()).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "user" && m.content == "hello agent")
        );
    }

    #[test]
    fn goto_calls_select_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        service.goto(spawned.task_id.as_str()).unwrap();

        let calls = runtime.calls.borrow();
        assert!(calls.iter().any(|c| matches!(c,
            Call::SelectWindow { window_id } if window_id == "@fake-win"
        )));
    }

    #[test]
    fn goto_errors_on_missing_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        // Insert a task with no tmux_window directly
        let task = Task {
            id: TaskId::from("aaaa-bbbb".to_string()),
            name: "no-window".to_string(),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            status: TaskStatus::Running,
            tmux_pane: None,
            tmux_window: None,
            work_dir: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
        };
        store.insert_task(&task).unwrap();

        let err = service.goto("aaaa-bbb");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("no tmux window"));
    }

    #[test]
    fn log_returns_messages_and_live_output() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        service.send(spawned.task_id.as_str(), "hello").unwrap();

        let log = service.log(spawned.task_id.as_str()).unwrap();
        assert_eq!(log.task.name, "test-task");
        assert_eq!(log.messages.len(), 2); // system + user
        assert!(log.live_output.is_some());
        assert_eq!(log.live_output.as_deref(), Some("captured output"));
    }

    #[test]
    fn list_active_excludes_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned1 = spawn_test_task(&service);
        let _spawned2 = spawn_test_task(&service);

        // Complete one
        let task1 = store
            .get_task_by_prefix(spawned1.task_id.as_str())
            .unwrap()
            .unwrap();
        service.complete(task1.id.as_str(), 0, None).unwrap();

        let active = service.list_active().unwrap();
        assert_eq!(active.len(), 1);

        let all = service.list_all().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn send_by_id_swallows_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        // Sending to a nonexistent task/pane should not panic
        service.send_by_id("nonexistent", "%bad-pane", "hello");
    }

    #[test]
    fn list_skills_returns_available_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        // test_paths already writes noop.toml — add a second skill
        std::fs::write(
            paths.skills_dir.join("deploy.toml"),
            r#"
[skill]
name = "deploy"
description = "deploy to prod"
params = [{ name = "env", required = true }]

[agent]
allowed_tools = []

[template]
prompt = "deploy to {{ env }}"
"#,
        )
        .unwrap();

        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let skills = service.list_skills().unwrap();
        assert_eq!(skills.len(), 2);
        // Sorted by name
        assert_eq!(skills[0].name, "deploy");
        assert_eq!(skills[0].description, "deploy to prod");
        assert_eq!(skills[0].params, vec!["env"]);
        assert_eq!(skills[1].name, "noop");
    }

    #[test]
    fn reopen_passes_work_dir_to_resume_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        service.complete(spawned.task_id.as_str(), 0, None).unwrap();

        let window_id = service.reopen(spawned.task_id.as_str()).unwrap();
        assert_eq!(window_id, "@fake-win");

        // Verify resume_agent was called with the task's work_dir
        let calls = runtime.calls.borrow();
        let resume_call = calls
            .iter()
            .find(|c| matches!(c, Call::ResumeAgent { .. }))
            .expect("expected ResumeAgent call");
        if let Call::ResumeAgent {
            task_name,
            work_dir,
        } = resume_call
        {
            assert_eq!(task_name, "test-task");
            assert!(
                work_dir.starts_with(tmp.path()),
                "work_dir should be inside the temp dir"
            );
        }

        // Verify task is back to running with new pane/window
        let task = store
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.tmux_pane.as_deref(), Some("%fake-pane"));
        assert_eq!(task.tmux_window.as_deref(), Some("@fake-win"));
    }

    #[test]
    fn reopen_errors_if_work_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);
        service.complete(spawned.task_id.as_str(), 0, None).unwrap();

        // Delete the worktree directory
        let task = store
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        let work_dir = task.work_dir.as_deref().unwrap();
        std::fs::remove_dir_all(work_dir).unwrap();

        let err = service.reopen(spawned.task_id.as_str());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("no longer exists"));
    }

    #[test]
    fn reopen_rejects_already_running_task() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = TaskService::new(&store, &runtime, &paths);

        let spawned = spawn_test_task(&service);

        let err = service.reopen(spawned.task_id.as_str());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("already running"));
    }
}
