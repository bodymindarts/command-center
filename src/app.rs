use std::collections::HashMap;
use std::path::Path;

use crate::config::Paths;
use crate::primitives::{ChatId, MessageRole, ProjectId, TaskId, TaskName, WindowId};
use crate::runtime::{LaunchConfig, Runtime};
use crate::skill::SkillFile;
use crate::store::Store;
use crate::task::{Project, Task, TaskMessage};
use anyhow::bail;

pub enum WorkDirMode<'a> {
    Worktree {
        repo: &'a Path,
        branch: Option<&'a str>,
    },
    Scratch,
    Existing {
        dir: &'a Path,
    },
}

pub enum PromptMode {
    Full,
    Interactive,
}

pub struct SpawnRequest<'a> {
    pub task_name: &'a str,
    pub skill_name: &'a str,
    pub params: Vec<(String, String)>,
    pub work_dir_mode: WorkDirMode<'a>,
    pub prompt_mode: PromptMode,
    pub project: Option<String>,
}

#[derive(Debug)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub params: Vec<String>,
}

#[derive(Debug)]
pub struct SpawnOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
    pub skill_name: String,
    pub window_id: WindowId,
}

#[derive(Debug)]
pub struct CloseOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
}

#[derive(Debug)]
pub struct SendOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
}

#[derive(Debug)]
pub struct CompleteOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
}

#[derive(Debug)]
pub struct DeleteOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
}

#[derive(Debug)]
pub struct LogOutput {
    pub task: Task,
    pub messages: Vec<TaskMessage>,
    pub live_output: Option<String>,
}

const EXO_CHAT: ChatId = ChatId::Exo;

pub struct ClatApp<R: Runtime> {
    store: Store,
    runtime: R,
    paths: Paths,
}

impl<R: Runtime> ClatApp<R> {
    pub fn try_new(runtime: R) -> anyhow::Result<Self> {
        let paths = Paths::resolve()?;
        paths.ensure_dirs()?;
        let store = Store::open(&paths.db_path)?;
        Ok(Self {
            store,
            runtime,
            paths,
        })
    }

    #[cfg(test)]
    pub fn new(store: Store, runtime: R, paths: Paths) -> Self {
        Self {
            store,
            runtime,
            paths,
        }
    }

    pub fn project_root(&self) -> &std::path::Path {
        &self.paths.root
    }

    #[cfg(test)]
    pub fn store(&self) -> &Store {
        &self.store
    }

    #[cfg(test)]
    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    pub fn read_exo_session_id(&self) -> Option<String> {
        std::fs::read_to_string(self.paths.exo_session_file())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_exo_session_id(&self, session_id: &str) {
        let _ = std::fs::write(self.paths.exo_session_file(), session_id);
    }

    pub fn read_project_session_id(&self, project_id: &ProjectId) -> Option<String> {
        std::fs::read_to_string(self.paths.project_session_file(project_id.as_str()))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_project_session_id(&self, project_id: &ProjectId, session_id: &str) {
        let _ = std::fs::write(
            self.paths.project_session_file(project_id.as_str()),
            session_id,
        );
    }

    pub fn spawn(&self, req: SpawnRequest) -> anyhow::Result<SpawnOutput> {
        // 1. Load skill, validate params
        let skill = SkillFile::load(&self.paths.skills_dir, req.skill_name)?;

        let mut params_map: HashMap<String, String> = req.params.into_iter().collect();
        params_map
            .entry("task".to_string())
            .or_insert_with(|| req.task_name.to_string());
        skill.validate_params(&params_map)?;

        // 2. Render prompts
        let system_prompt = skill.render_system(&self.paths.root)?;
        let user_prompt = match req.prompt_mode {
            PromptMode::Full => Some(skill.render_prompt(&params_map, &self.paths.root)?),
            PromptMode::Interactive => None,
        };

        // 3. Set up working directory
        // For Worktree mode, generate TaskId first since worktree name includes id.short()
        let id = TaskId::generate();
        let perms = crate::runtime::SkillPermissions {
            allowed_tools: &skill.agent.allowed_tools,
            base_tools: &skill.agent.base_tools,
            bash_patterns: &skill.agent.allowed_bash_patterns,
        };
        let work_dir = match req.work_dir_mode {
            WorkDirMode::Worktree { repo, branch } => {
                let worktree_name = format!("{}-{}", req.task_name, id.short());
                self.runtime.create_worktree(
                    repo,
                    &worktree_name,
                    &perms,
                    branch,
                    &self.paths.root,
                )?
            }
            WorkDirMode::Scratch => {
                let scratch_dir = self.paths.data_dir.join("scratch").join(req.task_name);
                self.runtime.init_scratch_dir(&scratch_dir)?;
                self.runtime
                    .setup_dir_config(&self.paths.root, &scratch_dir, &perms)?;
                scratch_dir
            }
            WorkDirMode::Existing { dir } => {
                self.runtime
                    .setup_dir_config(&self.paths.root, dir, &perms)?;
                dir.to_path_buf()
            }
        };

        // 4. Resolve project name → ID (if given)
        //    Also write a breadcrumb so spawned sub-tasks can inherit the project.
        let project_id = req
            .project
            .as_deref()
            .map(|name| self.resolve_project_id(name))
            .transpose()?;
        if let Some(ref name) = req.project {
            let breadcrumb = work_dir.join(".claude").join("project");
            let _ = std::fs::write(&breadcrumb, name);
        }

        // 5. Insert task into DB
        let mut task = Task::new(
            id,
            TaskName::from(req.task_name.to_string()),
            req.skill_name,
            &params_map,
            &work_dir,
            project_id,
        );
        self.store.insert_task(&task)?;

        // Store user prompt message only if Full mode
        if let Some(ref prompt) = user_prompt {
            let chat = ChatId::Task(task.id.clone());
            self.store
                .insert_message(&chat, MessageRole::System, prompt)?;
        }

        // 6. Launch agent
        let session_id = task.session_id.as_ref().expect("session_id set in new()");
        let result = self.runtime.launch_agent(LaunchConfig {
            task_name: req.task_name,
            session_id: session_id.as_str(),
            system_prompt: system_prompt.as_deref(),
            work_dir: &work_dir,
            user_prompt: user_prompt.as_deref(),
        })?;
        task.tmux_pane = Some(result.pane_id.clone());
        task.tmux_window = Some(result.window_id.clone());
        self.store.update_task(&task)?;

        Ok(SpawnOutput {
            task_id: task.id,
            task_name: TaskName::from(req.task_name.to_string()),
            skill_name: req.skill_name.to_string(),
            window_id: result.window_id,
        })
    }

    pub fn close(&self, id_prefix: &str) -> anyhow::Result<CloseOutput> {
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
            .as_ref()
            .and_then(|pane| self.runtime.capture_pane_output(pane.as_str()).ok());

        if let Some(ref window_id) = task.tmux_window {
            let _ = self.runtime.kill_tmux_window(window_id.as_str());
        }

        let closed = self.store.close_task(&task.id, output.as_deref())?;
        if !closed {
            bail!("failed to close task {} ({})", task.name, task.id.short());
        }

        Ok(CloseOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub fn delete(&self, task_id: &str) -> anyhow::Result<DeleteOutput> {
        let task = self.resolve_task(task_id)?;

        if task.status.is_running() {
            if let Some(ref window_id) = task.tmux_window {
                let _ = self.runtime.kill_tmux_window(window_id.as_str());
            }
            let _ = self.store.close_task(&task.id, None);
        }

        // Clean up the git worktree if the task used one.
        if let Some(ref work_dir) = task.work_dir
            && work_dir.contains(".claude/worktrees/")
        {
            let _ = self.runtime.remove_worktree(Path::new(work_dir));
        }

        self.store.delete_task(&task.id)?;
        Ok(DeleteOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub fn reopen(&self, task_id: &str) -> anyhow::Result<WindowId> {
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
            // Worktree was removed (e.g. after merging and cleaning up).
            // Recreate it so the agent can resume.
            self.runtime.recreate_worktree(&self.paths.root, work_dir)?;
        }

        let session_id = task.session_id.as_ref().map(|s| s.as_str()).unwrap_or("");
        let result = if session_id.is_empty() {
            // Legacy task without session_id — fall back to re-running launch.sh
            // which still exists in the worktree's .claude/ directory.
            self.runtime.relaunch_agent(task.name.as_str(), work_dir)?
        } else {
            self.runtime
                .resume_agent(task.name.as_str(), session_id, work_dir)?
        };

        self.store
            .reopen_task(&task.id, &result.pane_id, &result.window_id)?;

        Ok(result.window_id)
    }

    pub fn send(&self, id_prefix: &str, message: &str) -> anyhow::Result<SendOutput> {
        let task = self.resolve_task(id_prefix)?;

        let pane_id = task
            .tmux_pane
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux pane", task.id.short()))?;

        self.runtime.send_keys_to_pane(pane_id.as_str(), message)?;
        let chat = ChatId::Task(task.id.clone());
        self.store
            .insert_message(&chat, MessageRole::User, message)?;

        Ok(SendOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub fn goto(&self, id_prefix: &str) -> anyhow::Result<()> {
        let task = self.resolve_task(id_prefix)?;

        let window_id = task
            .tmux_window
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux window", task.id.short()))?;

        self.runtime.select_window(window_id.as_str())
    }

    pub fn goto_window(&self, window_id: &WindowId) {
        let _ = self.runtime.select_window(window_id.as_str());
    }

    pub fn log(&self, id_prefix: &str) -> anyhow::Result<LogOutput> {
        let task = self.resolve_task(id_prefix)?;
        let chat = ChatId::Task(task.id.clone());
        let messages = self.store.list_messages(&chat)?;

        let live_output = if task.status.is_running() {
            task.tmux_pane
                .as_ref()
                .and_then(|pane| self.runtime.capture_pane_output(pane.as_str()).ok())
        } else {
            None
        };

        Ok(LogOutput {
            task,
            messages,
            live_output,
        })
    }

    pub fn list_tasks(&self, all: bool, project: Option<&str>) -> anyhow::Result<Vec<Task>> {
        if all {
            self.store.list_tasks()
        } else if let Some(name) = project {
            let project = self
                .store
                .get_project_by_name(name)?
                .ok_or_else(|| anyhow::anyhow!("no project found with name '{name}'"))?;
            self.store.list_visible_tasks_for_project(Some(&project.id))
        } else {
            self.store.list_active_tasks()
        }
    }

    /// Close any running tasks whose tmux pane no longer exists.
    pub fn close_stale_tasks(&self) {
        let tasks = match self.store.list_active_tasks() {
            Ok(t) => t,
            Err(_) => return,
        };
        // Collect all existing pane IDs in one tmux call.
        let existing: std::collections::HashSet<String> =
            match crate::runtime::tmux_cmd(&["list-panes", "-a", "-F", "#{pane_id}"]) {
                Ok(output) => output.lines().map(|l| l.trim().to_string()).collect(),
                Err(_) => return, // tmux not running — can't determine staleness
            };
        for task in tasks {
            if let Some(ref pane) = task.tmux_pane
                && !existing.contains(pane.as_str())
            {
                let _ = self.store.close_task(&task.id, None);
            }
        }
    }

    pub fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        self.store.list_active_tasks()
    }

    pub fn list_visible(&self, project_id: Option<&ProjectId>) -> anyhow::Result<Vec<Task>> {
        self.store.list_visible_tasks_for_project(project_id)
    }

    pub fn messages(&self, chat_id: &ChatId) -> anyhow::Result<Vec<TaskMessage>> {
        self.store.list_messages(chat_id)
    }

    pub fn list_skills(&self) -> anyhow::Result<Vec<SkillSummary>> {
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

    pub fn window_numbers(&self) -> HashMap<WindowId, String> {
        crate::runtime::tmux_window_numbers()
    }

    pub fn insert_session_message(
        &self,
        project_id: Option<&ProjectId>,
        role: MessageRole,
        content: &str,
    ) -> anyhow::Result<()> {
        let chat = Self::chat_id(project_id);
        self.store.insert_message(&chat, role, content)
    }

    pub fn session_messages(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<TaskMessage>> {
        let chat = Self::chat_id(project_id);
        self.store.list_messages(&chat)
    }

    // -- Project methods --

    pub fn create_project(&self, name: &str, description: &str) -> anyhow::Result<Project> {
        let id = crate::primitives::ProjectId::generate();
        self.store.insert_project(&id, name, description)?;
        self.store
            .get_project_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("failed to retrieve project after insert"))
    }

    pub fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.store.list_projects()
    }

    pub fn delete_project(&self, name: &str) -> anyhow::Result<()> {
        let project = self
            .store
            .get_project_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("no project found with name '{name}'"))?;
        self.store.delete_project(&project.id)
    }

    pub fn resolve_project(&self, name: &str) -> anyhow::Result<Project> {
        self.store
            .get_project_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("no project found with name '{name}'"))
    }

    pub fn resolve_project_id(&self, name: &str) -> anyhow::Result<ProjectId> {
        Ok(self.resolve_project(name)?.id)
    }

    fn chat_id(project_id: Option<&ProjectId>) -> ChatId {
        match project_id {
            None => EXO_CHAT,
            Some(pid) => ChatId::Project(pid.clone()),
        }
    }

    pub fn complete(
        &self,
        id_prefix: &str,
        exit_code: i32,
        output: Option<&str>,
    ) -> anyhow::Result<CompleteOutput> {
        let task = self.resolve_task(id_prefix)?;
        self.store.complete_task(&task.id, exit_code, output)?;
        Ok(CompleteOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    fn resolve_task(&self, id_prefix: &str) -> anyhow::Result<Task> {
        self.store
            .get_task_by_prefix(id_prefix)?
            .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use anyhow::bail;

    use chrono::Utc;

    use crate::primitives::{PaneId, TaskId, TaskName, TaskStatus, WindowId};
    use crate::runtime::{LaunchConfig, Runtime, SpawnResult};
    use crate::store::Store;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        CreateWorktree {
            name: String,
        },
        RecreateWorktree {
            work_dir: PathBuf,
        },
        SetupDirConfig {
            work_dir: PathBuf,
        },
        InitScratchDir {
            scratch_dir: PathBuf,
        },
        LaunchAgent {
            task_name: String,
            has_user_prompt: bool,
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
        RemoveWorktree {
            path: PathBuf,
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
        spawn_window_id: WindowId,
        spawn_pane_id: PaneId,
        capture_result: RefCell<Option<String>>,
        kill_should_fail: RefCell<bool>,
    }

    impl FakeRuntime {
        fn new(worktree_dir: &Path) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                worktree_dir: worktree_dir.to_path_buf(),
                spawn_window_id: WindowId::from("@fake-win".to_string()),
                spawn_pane_id: PaneId::from("%fake-pane".to_string()),
                capture_result: RefCell::new(Some("captured output".to_string())),
                kill_should_fail: RefCell::new(false),
            }
        }
    }

    impl Runtime for FakeRuntime {
        fn create_worktree(
            &self,
            _repo_root: &Path,
            name: &str,
            _perms: &crate::runtime::SkillPermissions,
            _branch: Option<&str>,
            _hooks_source: &Path,
        ) -> anyhow::Result<PathBuf> {
            self.calls.borrow_mut().push(Call::CreateWorktree {
                name: name.to_string(),
            });
            let path = self.worktree_dir.join(name);
            std::fs::create_dir_all(&path)?;
            Ok(path)
        }

        fn recreate_worktree(&self, _repo_root: &Path, work_dir: &Path) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::RecreateWorktree {
                work_dir: work_dir.to_path_buf(),
            });
            std::fs::create_dir_all(work_dir)?;
            Ok(())
        }

        fn setup_dir_config(
            &self,
            _hooks_source: &Path,
            work_dir: &Path,
            _perms: &crate::runtime::SkillPermissions,
        ) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::SetupDirConfig {
                work_dir: work_dir.to_path_buf(),
            });
            Ok(())
        }

        fn init_scratch_dir(&self, scratch_dir: &Path) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::InitScratchDir {
                scratch_dir: scratch_dir.to_path_buf(),
            });
            std::fs::create_dir_all(scratch_dir)?;
            Ok(())
        }

        fn launch_agent(&self, config: LaunchConfig) -> anyhow::Result<SpawnResult> {
            self.calls.borrow_mut().push(Call::LaunchAgent {
                task_name: config.task_name.to_string(),
                has_user_prompt: config.user_prompt.is_some(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn resume_agent(
            &self,
            task_name: &str,
            _session_id: &str,
            work_dir: &Path,
        ) -> anyhow::Result<SpawnResult> {
            self.calls.borrow_mut().push(Call::ResumeAgent {
                task_name: task_name.to_string(),
                work_dir: work_dir.to_path_buf(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn relaunch_agent(&self, task_name: &str, work_dir: &Path) -> anyhow::Result<SpawnResult> {
            self.calls.borrow_mut().push(Call::ResumeAgent {
                task_name: task_name.to_string(),
                work_dir: work_dir.to_path_buf(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::SendKeys {
                pane_id: pane_id.to_string(),
                message: message.to_string(),
            });
            Ok(())
        }

        fn capture_pane_output(&self, pane_id: &str) -> anyhow::Result<String> {
            self.calls.borrow_mut().push(Call::CaptureOutput {
                pane_id: pane_id.to_string(),
            });
            self.capture_result
                .borrow()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no capture result"))
        }

        fn remove_worktree(&self, path: &Path) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::RemoveWorktree {
                path: path.to_path_buf(),
            });
            Ok(())
        }

        fn kill_tmux_window(&self, window_id: &str) -> anyhow::Result<()> {
            self.calls.borrow_mut().push(Call::KillWindow {
                window_id: window_id.to_string(),
            });
            if *self.kill_should_fail.borrow() {
                bail!("kill failed");
            }
            Ok(())
        }

        fn select_window(&self, window_id: &str) -> anyhow::Result<()> {
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

    fn spawn_test_task(service: &ClatApp<impl Runtime>) -> SpawnOutput {
        service
            .spawn(SpawnRequest {
                task_name: "test-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Worktree {
                    repo: service.project_root(),
                    branch: None,
                },
                prompt_mode: PromptMode::Full,
                project: None,
            })
            .expect("spawn should succeed")
    }

    #[test]
    fn spawn_creates_task_and_calls_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let output = spawn_test_task(&service);

        assert_eq!(output.task_name, "test-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        // Verify task is in store
        let tasks = service.store().list_active_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "test-task");
        assert_eq!(
            tasks[0].tmux_pane.as_ref().map(|p| p.as_str()),
            Some("%fake-pane")
        );

        // Verify system message recorded
        let chat = ChatId::Task(tasks[0].id.clone());
        let messages = service.store().list_messages(&chat).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::System);

        // Verify call order
        let calls = service.runtime().calls.borrow();
        assert!(matches!(calls[0], Call::CreateWorktree { .. }));
        assert!(matches!(
            calls[1],
            Call::LaunchAgent {
                has_user_prompt: true,
                ..
            }
        ));
    }

    #[test]
    fn close_captures_output_before_killing_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        let result = service.close(spawned.task_id.as_str()).unwrap();

        assert_eq!(result.task_name, "test-task");

        // Verify task is closed with output
        let task = service
            .store()
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.output.as_deref(), Some("captured output"));

        // Verify call order: CaptureOutput before KillWindow
        let calls = service.runtime().calls.borrow();
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
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        let task = service
            .store()
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
        let service = ClatApp::new(store, runtime, paths);

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
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        let result = service
            .send(spawned.task_id.as_str(), "hello agent")
            .unwrap();

        assert_eq!(result.task_name, "test-task");

        // Verify SendKeys call
        let calls = service.runtime().calls.borrow();
        assert!(calls.iter().any(|c| matches!(c,
            Call::SendKeys { pane_id, message }
            if pane_id == "%fake-pane" && message == "hello agent"
        )));

        // Verify user message in store
        let task = service
            .store()
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        let chat = ChatId::Task(task.id.clone());
        let messages = service.store().list_messages(&chat).unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == MessageRole::User && m.content == "hello agent")
        );
    }

    #[test]
    fn goto_calls_select_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        service.goto(spawned.task_id.as_str()).unwrap();

        let calls = service.runtime().calls.borrow();
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
        let service = ClatApp::new(store, runtime, paths);

        // Insert a task with no tmux_window directly
        let task_id = TaskId::generate();
        let task = Task {
            id: task_id.clone(),
            name: TaskName::from("no-window".to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            status: TaskStatus::Running,
            tmux_pane: None,
            tmux_window: None,
            work_dir: None,
            session_id: None,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            project_id: None,
        };
        service.store().insert_task(&task).unwrap();

        let err = service.goto(task_id.as_str());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("no tmux window"));
    }

    #[test]
    fn log_returns_messages_and_live_output() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

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
        let service = ClatApp::new(store, runtime, paths);

        let spawned1 = spawn_test_task(&service);
        let _spawned2 = spawn_test_task(&service);

        // Complete one
        let task1 = service
            .store()
            .get_task_by_prefix(spawned1.task_id.as_str())
            .unwrap()
            .unwrap();
        service.complete(task1.id.as_str(), 0, None).unwrap();

        let active = service.list_active().unwrap();
        assert_eq!(active.len(), 1);

        let all = service.list_tasks(true, None).unwrap();
        assert_eq!(all.len(), 2);
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
        let service = ClatApp::new(store, runtime, paths);

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
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        service.complete(spawned.task_id.as_str(), 0, None).unwrap();

        let window_id = service.reopen(spawned.task_id.as_str()).unwrap();
        assert_eq!(window_id, "@fake-win");

        // Verify resume_agent was called with the task's work_dir
        let calls = service.runtime().calls.borrow();
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
        let task = service
            .store()
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(
            task.tmux_pane.as_ref().map(|p| p.as_str()),
            Some("%fake-pane")
        );
        assert_eq!(
            task.tmux_window.as_ref().map(|w| w.as_str()),
            Some("@fake-win")
        );
    }

    #[test]
    fn reopen_recreates_missing_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);
        service.complete(spawned.task_id.as_str(), 0, None).unwrap();

        // Delete the worktree directory to simulate post-merge cleanup
        let task = service
            .store()
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        let work_dir = task.work_dir.as_deref().unwrap();
        std::fs::remove_dir_all(work_dir).unwrap();

        // Reopen should recreate the worktree and succeed
        let window_id = service.reopen(spawned.task_id.as_str()).unwrap();
        assert_eq!(window_id, "@fake-win");

        // Verify RecreateWorktree was called
        let calls = service.runtime().calls.borrow();
        assert!(
            calls
                .iter()
                .any(|c| matches!(c, Call::RecreateWorktree { .. })),
            "expected RecreateWorktree call"
        );

        // Verify task is back to running
        let task = service
            .store()
            .get_task_by_prefix(spawned.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
    }

    #[test]
    fn reopen_rejects_already_running_task() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service);

        let err = service.reopen(spawned.task_id.as_str());
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("already running"));
    }

    // -- spawn_no_worktree tests --

    #[test]
    fn spawn_existing_uses_setup_dir_config_and_interactive() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let output = service
            .spawn(SpawnRequest {
                task_name: "nw-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Existing {
                    dir: service.project_root(),
                },
                prompt_mode: PromptMode::Interactive,
                project: None,
            })
            .unwrap();

        assert_eq!(output.task_name, "nw-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        // Task is stored and running
        let tasks = service.store().list_active_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "nw-task");
        assert_eq!(
            tasks[0].tmux_pane.as_ref().map(|p| p.as_str()),
            Some("%fake-pane")
        );

        // Verify call order: SetupDirConfig then LaunchAgent (no CreateWorktree)
        let calls = service.runtime().calls.borrow();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, Call::CreateWorktree { .. })),
            "Existing mode must not create a worktree"
        );
        assert!(matches!(calls[0], Call::SetupDirConfig { .. }));
        assert!(matches!(
            calls[1],
            Call::LaunchAgent {
                has_user_prompt: false,
                ..
            }
        ));
    }

    #[test]
    fn spawn_existing_uses_custom_dir_path() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let custom_repo = tmp.path().join("other-repo");
        std::fs::create_dir_all(&custom_repo).unwrap();

        let output = service
            .spawn(SpawnRequest {
                task_name: "nw-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Existing { dir: &custom_repo },
                prompt_mode: PromptMode::Interactive,
                project: None,
            })
            .unwrap();
        assert_eq!(output.task_name, "nw-task");

        // Task work_dir should be the custom path
        let task = service
            .store()
            .get_task_by_prefix(output.task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(
            task.work_dir.as_deref(),
            Some(custom_repo.to_str().unwrap())
        );

        // Verify SetupDirConfig was called with the custom path
        let calls = service.runtime().calls.borrow();
        let setup_call = calls
            .iter()
            .find(|c| matches!(c, Call::SetupDirConfig { .. }))
            .unwrap();
        if let Call::SetupDirConfig { work_dir } = setup_call {
            assert_eq!(work_dir, &custom_repo);
        }
    }

    // -- spawn_scratch tests --

    #[test]
    fn spawn_scratch_creates_scratch_dir_and_launches_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let output = service
            .spawn(SpawnRequest {
                task_name: "scratch-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Scratch,
                prompt_mode: PromptMode::Full,
                project: None,
            })
            .unwrap();

        assert_eq!(output.task_name, "scratch-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        // Task is stored and running
        let tasks = service.store().list_active_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "scratch-task");

        // Work dir should be data_dir/scratch/scratch-task
        let expected_scratch = tmp.path().join("data").join("scratch").join("scratch-task");
        assert_eq!(
            tasks[0].work_dir.as_deref(),
            Some(expected_scratch.to_str().unwrap())
        );

        // Verify call order: InitScratchDir, SetupDirConfig, LaunchAgent
        let calls = service.runtime().calls.borrow();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, Call::CreateWorktree { .. })),
            "Scratch mode must not create a worktree"
        );
        assert!(matches!(calls[0], Call::InitScratchDir { .. }));
        assert!(matches!(calls[1], Call::SetupDirConfig { .. }));
        assert!(matches!(
            calls[2],
            Call::LaunchAgent {
                has_user_prompt: true,
                ..
            }
        ));
    }

    // -- Project CRUD via service layer --

    #[test]
    fn create_and_list_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let project = service
            .create_project("web-app", "frontend project")
            .unwrap();
        assert_eq!(project.name, "web-app");
        assert_eq!(project.description, "frontend project");
        assert!(!project.id.as_str().is_empty());

        let projects = service.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "web-app");
    }

    #[test]
    fn delete_project_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        service.create_project("temp-proj", "").unwrap();
        assert_eq!(service.list_projects().unwrap().len(), 1);

        service.delete_project("temp-proj").unwrap();
        assert!(service.list_projects().unwrap().is_empty());
    }

    #[test]
    fn delete_project_errors_on_unknown_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let err = service.delete_project("ghost");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("no project found"));
    }

    #[test]
    fn list_visible_scoped_to_project() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        // Spawn two tasks: one with project, one without
        let proj = service.create_project("test-proj", "").unwrap();
        let proj_id = proj.id;
        let out1 = service
            .spawn(SpawnRequest {
                task_name: "proj-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Worktree {
                    repo: service.project_root(),
                    branch: None,
                },
                prompt_mode: PromptMode::Full,
                project: Some("test-proj".to_string()),
            })
            .unwrap();
        let out2 = spawn_test_task(&service); // no project

        // list_visible(None) should only return the unscoped task
        let unscoped = service.list_visible(None).unwrap();
        assert_eq!(unscoped.len(), 1);
        assert_eq!(unscoped[0].id, out2.task_id);

        // list_visible(Some(proj_id)) should only return the project task
        let scoped = service.list_visible(Some(&proj_id)).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, out1.task_id);
    }
}
