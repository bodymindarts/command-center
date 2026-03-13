use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::config::Paths;
use crate::jwt::JwtSigner;
use crate::primitives::{
    ChatId, ClaudeSessionId, MessageRole, ProjectId, ProjectName, TaskId, TaskName, WindowId,
};
use crate::project::{NewProject, Project};
use crate::runtime::{LaunchConfig, Runtime};
use crate::skill::SkillFile;
use crate::store::Store;
use crate::task::{NewTask, Task, TaskMessage};
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
pub struct MoveOutput {
    pub task_id: TaskId,
    pub task_name: TaskName,
    pub project_name: String,
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
    skip_permissions: bool,
    jwt_signer: JwtSigner,
    watch: OnceLock<crate::watch::WatchService>,
}

impl<R: Runtime> ClatApp<R> {
    pub async fn init(runtime: R, skip_permissions: bool) -> anyhow::Result<Self> {
        let paths = Paths::resolve()?;
        paths.ensure_dirs()?;
        let store = Store::open(&paths.db_path).await?;
        // If the CLI flag wasn't set, check whether the dashboard wrote a
        // breadcrumb — this lets `clat spawn` from a separate terminal
        // inherit the dashboard's --dangerously-skip-permissions.
        let skip_permissions =
            skip_permissions || crate::permission::read_skip_permissions_breadcrumb(&paths.root);
        let jwt_signer = JwtSigner::load_or_create(&paths.data_dir.join("jwt-secret"))?;
        Ok(Self {
            store,
            runtime,
            paths,
            skip_permissions,
            jwt_signer,
            watch: OnceLock::new(),
        })
    }

    pub fn watch(&self) -> &crate::watch::WatchService {
        self.watch.get().expect("watch service not initialized")
    }

    pub async fn init_watch(self: &Arc<Self>) -> anyhow::Result<()> {
        let config = job::JobSvcConfig::builder()
            .pool(self.store.pool().clone())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build job config: {e}"))?;
        let mut jobs = job::Jobs::init(config).await?;
        let timer_spawner = jobs.add_initializer(crate::watch::TimerJobInitializer {
            app: Arc::clone(self),
        });
        let command_spawner = jobs.add_initializer(crate::watch::CommandJobInitializer {
            app: Arc::clone(self),
        });
        jobs.start_poll().await?;
        self.watch
            .set(crate::watch::WatchService::new(
                timer_spawner,
                command_spawner,
                jobs,
            ))
            .map_err(|_| anyhow::anyhow!("watch service already initialized"))?;
        Ok(())
    }

    /// Look up a task's work_dir by id prefix. Used by watch runners to set cwd.
    pub async fn task_work_dir(&self, id_prefix: &str) -> anyhow::Result<Option<String>> {
        let task = self.resolve_task(id_prefix).await?;
        Ok(task.work_dir)
    }

    #[cfg(test)]
    pub fn new(store: Store, runtime: R, paths: Paths) -> Self {
        let jwt_signer = JwtSigner::load_or_create(&paths.data_dir.join("jwt-secret")).unwrap();
        Self {
            store,
            runtime,
            paths,
            skip_permissions: false,
            jwt_signer,
            watch: OnceLock::new(),
        }
    }

    pub fn skip_permissions(&self) -> bool {
        self.skip_permissions
    }

    pub fn project_root(&self) -> &std::path::Path {
        &self.paths.root
    }

    pub fn jwt_signer(&self) -> &JwtSigner {
        &self.jwt_signer
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
        std::fs::read_to_string(self.paths.project_session_file(&project_id.to_string()))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_project_session_id(&self, project_id: &ProjectId, session_id: &str) {
        let _ = std::fs::write(
            self.paths.project_session_file(&project_id.to_string()),
            session_id,
        );
    }

    pub async fn spawn(&self, req: SpawnRequest<'_>) -> anyhow::Result<SpawnOutput> {
        // 1. Load skill, validate params
        let skill = SkillFile::load(&self.paths.skills_dir, req.skill_name)?;

        let mut params_map: HashMap<String, String> = req.params.into_iter().collect();
        params_map
            .entry("task".to_string())
            .or_insert_with(|| req.task_name.to_string());
        skill.validate_params(&params_map)?;

        // 2. Render prompts
        let system_prompt = skill.render_system(&params_map, &self.paths.root)?;
        let user_prompt = match req.prompt_mode {
            PromptMode::Full => Some(skill.render_prompt(&params_map, &self.paths.root)?),
            PromptMode::Interactive => None,
        };

        // 3. Set up working directory
        // For Worktree mode, generate TaskId first since worktree name includes id.short()
        let id = TaskId::new();
        let perms = crate::runtime::SkillPermissions {
            allowed_tools: &skill.agent.allowed_tools,
            base_tools: &skill.agent.base_tools,
            bash_patterns: &skill.agent.allowed_bash_patterns,
        };
        // Sign a JWT for this task so the MCP server can identify the caller.
        let jwt_token = {
            let claims = crate::jwt::AgentClaims {
                sub: id.to_string(),
                role: req.skill_name.to_string(),
                project: req.project.clone(),
                iat: chrono::Utc::now().timestamp() as u64,
            };
            self.jwt_signer.sign(&claims)?
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
                    &jwt_token,
                )?
            }
            WorkDirMode::Scratch => {
                let scratch_dir = self.paths.data_dir.join("scratch").join(req.task_name);
                self.runtime.init_scratch_dir(&scratch_dir)?;
                self.runtime.setup_dir_config(
                    &self.paths.root,
                    &scratch_dir,
                    &perms,
                    &jwt_token,
                )?;
                scratch_dir
            }
            WorkDirMode::Existing { dir } => {
                self.runtime
                    .setup_dir_config(&self.paths.root, dir, &perms, &jwt_token)?;
                dir.to_path_buf()
            }
        };

        // 4. Resolve project name → ID (if given)
        //    Also write a breadcrumb so spawned sub-tasks can inherit the project.
        let project_id = match req.project.as_deref() {
            Some(name) => Some(self.resolve_project_id(name).await?),
            None => None,
        };
        if let Some(ref name) = req.project {
            let breadcrumb = work_dir.join(".claude").join("project");
            let _ = std::fs::write(&breadcrumb, name);
        }

        // 5. Insert task into DB
        let session_id = ClaudeSessionId::new();
        let new_task = NewTask {
            id,
            name: TaskName::from(req.task_name.to_string()),
            skill_name: req.skill_name.to_string(),
            params_json: serde_json::to_string(&params_map).unwrap_or_else(|_| "{}".to_string()),
            work_dir: Some(work_dir.display().to_string()),
            session_id,
            project_id,
        };
        let mut task = self.store.tasks.create(new_task).await?;

        // Store user prompt message only if Full mode
        if let Some(ref prompt) = user_prompt {
            let chat = ChatId::Task(task.id);
            self.store
                .insert_message(&chat, MessageRole::System, prompt)
                .await?;
        }

        // 6. Launch agent
        let session_id_str = session_id.to_string();
        let result = self.runtime.launch_agent(LaunchConfig {
            task_name: req.task_name,
            session_id: &session_id_str,
            system_prompt: system_prompt.as_deref(),
            work_dir: &work_dir,
            user_prompt: user_prompt.as_deref(),
            skip_permissions: self.skip_permissions,
        })?;
        if task
            .launch_agent(result.pane_id.clone(), result.window_id.clone())
            .did_execute()
        {
            self.store.tasks.update(&mut task).await?;
        }

        Ok(SpawnOutput {
            task_id: task.id,
            task_name: TaskName::from(req.task_name.to_string()),
            skill_name: req.skill_name.to_string(),
            window_id: result.window_id,
        })
    }

    pub async fn close(&self, id_prefix: &str) -> anyhow::Result<CloseOutput> {
        let mut task = self.resolve_task(id_prefix).await?;

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

        if task.close(output).did_execute() {
            self.store.tasks.update(&mut task).await?;
        }

        Ok(CloseOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub async fn delete(&self, task_id: &str) -> anyhow::Result<DeleteOutput> {
        let mut task = self.resolve_task(task_id).await?;

        if task.status.is_running()
            && let Some(ref window_id) = task.tmux_window
        {
            let _ = self.runtime.kill_tmux_window(window_id.as_str());
        }

        // Clean up the git worktree if the task used one.
        if let Some(ref work_dir) = task.work_dir
            && work_dir.contains(".claude/worktrees/")
        {
            let _ = self.runtime.remove_worktree(Path::new(work_dir));
        }

        let output_task_id = task.id;
        let output_task_name = task.name.clone();

        self.store.delete_task_messages(&output_task_id).await.ok();

        let _ = task.close(None);
        if task.delete().did_execute() {
            self.store.tasks.delete(task).await?;
        }

        Ok(DeleteOutput {
            task_id: output_task_id,
            task_name: output_task_name,
        })
    }

    pub async fn reopen(&self, task_id: &str) -> anyhow::Result<WindowId> {
        let mut task = self.resolve_task(task_id).await?;

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
            // Recreate it so the agent can resume. Re-sign a fresh JWT.
            let jwt_token = {
                let claims = crate::jwt::AgentClaims {
                    sub: task.id.to_string(),
                    role: task.skill_name.clone(),
                    project: task.project_id.as_ref().map(|p| p.to_string()),
                    iat: chrono::Utc::now().timestamp() as u64,
                };
                self.jwt_signer.sign(&claims)?
            };
            self.runtime
                .recreate_worktree(&self.paths.root, work_dir, &jwt_token)?;
        }

        let session_id = task.session_id.map(|s| s.to_string()).unwrap_or_default();
        let result = if session_id.is_empty() {
            // Legacy task without session_id — fall back to re-running launch.sh
            // which already has the correct flags baked in from launch_agent().
            self.runtime.relaunch_agent(task.name.as_str(), work_dir)?
        } else {
            self.runtime.resume_agent(
                task.name.as_str(),
                &session_id,
                work_dir,
                self.skip_permissions,
            )?
        };

        if task
            .reopen(result.pane_id.clone(), result.window_id.clone())
            .map_err(anyhow::Error::from)?
            .did_execute()
        {
            self.store.tasks.update(&mut task).await?;
        }

        Ok(result.window_id)
    }

    pub async fn move_task(
        &self,
        id_prefix: &str,
        project_name: &str,
    ) -> anyhow::Result<MoveOutput> {
        let mut task = self.resolve_task(id_prefix).await?;
        let project = self.resolve_project(project_name).await?;

        if task.move_to_project(Some(project.id)).did_execute() {
            self.store.tasks.update(&mut task).await?;
        }

        Ok(MoveOutput {
            task_id: task.id,
            task_name: task.name,
            project_name: project.name.to_string(),
        })
    }

    pub async fn send(&self, id_prefix: &str, message: &str) -> anyhow::Result<SendOutput> {
        let task = self.resolve_task(id_prefix).await?;

        let pane_id = task
            .tmux_pane
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux pane", task.id.short()))?;

        self.runtime.send_keys_to_pane(pane_id.as_str(), message)?;
        let chat = ChatId::Task(task.id);
        self.store
            .insert_message(&chat, MessageRole::User, message)
            .await?;

        Ok(SendOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    pub async fn goto(&self, id_prefix: &str) -> anyhow::Result<()> {
        let task = self.resolve_task(id_prefix).await?;

        let window_id = task
            .tmux_window
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task {} has no tmux window", task.id.short()))?;

        self.runtime.select_window(window_id.as_str())
    }

    pub fn goto_window(&self, window_id: &WindowId) {
        let _ = self.runtime.select_window(window_id.as_str());
    }

    pub async fn log(&self, id_prefix: &str) -> anyhow::Result<LogOutput> {
        let task = self.resolve_task(id_prefix).await?;
        let chat = ChatId::Task(task.id);
        let messages = self.store.list_messages(&chat).await?;

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

    pub async fn project_log(
        &self,
        name: &str,
        last: Option<u32>,
    ) -> anyhow::Result<(Project, Vec<TaskMessage>)> {
        let project = self.resolve_project(name).await?;
        let chat = ChatId::Project(project.id);
        let messages = match last {
            Some(n) => self.store.list_messages_last(&chat, n).await?,
            None => self.store.list_messages(&chat).await?,
        };
        Ok((project, messages))
    }

    pub async fn list_tasks(&self, all: bool, project: Option<&str>) -> anyhow::Result<Vec<Task>> {
        if all {
            self.store.tasks.list_all().await
        } else if let Some(name) = project {
            let project = self.resolve_project(name).await?;
            self.store
                .tasks
                .list_visible_for_project(Some(&project.id))
                .await
        } else {
            self.store.tasks.list_active().await
        }
    }

    /// Close any running tasks whose tmux pane no longer exists.
    pub async fn close_stale_tasks(&self) {
        let tasks = match self.store.tasks.list_active().await {
            Ok(t) => t,
            Err(_) => return,
        };
        // Collect all existing pane IDs in one tmux call.
        let existing: std::collections::HashSet<String> =
            match crate::runtime::tmux_cmd(&["list-panes", "-a", "-F", "#{pane_id}"]) {
                Ok(output) => output.lines().map(|l| l.trim().to_string()).collect(),
                Err(_) => return, // tmux not running — can't determine staleness
            };
        for mut task in tasks {
            if let Some(ref pane) = task.tmux_pane
                && !existing.contains(pane.as_str())
                && task.close(None).did_execute()
            {
                let _ = self.store.tasks.update(&mut task).await;
            }
        }
    }

    pub async fn list_active(&self) -> anyhow::Result<Vec<Task>> {
        self.store.tasks.list_active().await
    }

    pub async fn list_visible(&self, project_id: Option<&ProjectId>) -> anyhow::Result<Vec<Task>> {
        self.store.tasks.list_visible_for_project(project_id).await
    }

    pub async fn messages(&self, chat_id: &ChatId) -> anyhow::Result<Vec<TaskMessage>> {
        self.store.list_messages(chat_id).await
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

    pub async fn insert_session_message(
        &self,
        project_id: Option<&ProjectId>,
        role: MessageRole,
        content: &str,
    ) -> anyhow::Result<()> {
        let chat = Self::chat_id(project_id);
        self.store.insert_message(&chat, role, content).await
    }

    pub async fn session_messages(
        &self,
        project_id: Option<&ProjectId>,
    ) -> anyhow::Result<Vec<TaskMessage>> {
        let chat = Self::chat_id(project_id);
        self.store.list_messages(&chat).await
    }

    // -- Project methods --

    pub async fn create_project(&self, name: &str, description: &str) -> anyhow::Result<Project> {
        let new = NewProject {
            id: ProjectId::new(),
            name: ProjectName::from(name.to_string()),
            description: description.to_string(),
        };
        let project = self.store.projects.create(new).await?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> anyhow::Result<Vec<Project>> {
        self.store.projects.list_all().await
    }

    pub async fn delete_project(&self, name: &str) -> anyhow::Result<()> {
        let name = ProjectName::from(name.to_string());
        let mut project = self.store.projects.find_by_name(name).await?;

        // Cascade-delete all tasks belonging to this project.
        let tasks = self
            .store
            .tasks
            .list_visible_for_project(Some(&project.id))
            .await?;
        for mut task in tasks {
            if task.status.is_running()
                && let Some(ref window_id) = task.tmux_window
            {
                let _ = self.runtime.kill_tmux_window(window_id.as_str());
            }
            if let Some(ref work_dir) = task.work_dir
                && work_dir.contains(".claude/worktrees/")
            {
                let _ = self.runtime.remove_worktree(Path::new(work_dir));
            }
            self.store.delete_task_messages(&task.id).await.ok();
            let _ = task.close(None);
            if task.delete().did_execute() {
                self.store.tasks.delete(task).await?;
            }
        }

        let _ = project.delete();
        self.store.projects.delete(project).await?;
        Ok(())
    }

    pub async fn resolve_project(&self, name: &str) -> anyhow::Result<Project> {
        let name = ProjectName::from(name.to_string());
        Ok(self.store.projects.find_by_name(name).await?)
    }

    pub async fn resolve_project_id(&self, name: &str) -> anyhow::Result<ProjectId> {
        Ok(self.resolve_project(name).await?.id)
    }

    fn chat_id(project_id: Option<&ProjectId>) -> ChatId {
        match project_id {
            None => EXO_CHAT,
            Some(pid) => ChatId::Project(*pid),
        }
    }

    pub async fn complete(
        &self,
        id_prefix: &str,
        exit_code: i32,
        output: Option<&str>,
    ) -> anyhow::Result<CompleteOutput> {
        let mut task = self.resolve_task(id_prefix).await?;
        if task
            .complete(exit_code, output.map(|s| s.to_string()))
            .map_err(anyhow::Error::from)?
            .did_execute()
        {
            self.store.tasks.update(&mut task).await?;
        }
        Ok(CompleteOutput {
            task_id: task.id,
            task_name: task.name,
        })
    }

    async fn resolve_task(&self, id_prefix: &str) -> anyhow::Result<Task> {
        self.store
            .tasks
            .maybe_find_by_id_prefix(id_prefix)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no task found matching '{id_prefix}'"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use anyhow::bail;

    use crate::primitives::{ClaudeSessionId, PaneId, TaskId, TaskName, TaskStatus, WindowId};
    use crate::runtime::{LaunchConfig, Runtime, SpawnResult};
    use crate::store::Store;
    use crate::task::NewTask;

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
        calls: Mutex<Vec<Call>>,
        worktree_dir: PathBuf,
        spawn_window_id: WindowId,
        spawn_pane_id: PaneId,
        capture_result: Mutex<Option<String>>,
        kill_should_fail: Mutex<bool>,
    }

    impl FakeRuntime {
        fn new(worktree_dir: &Path) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                worktree_dir: worktree_dir.to_path_buf(),
                spawn_window_id: WindowId::from("@fake-win".to_string()),
                spawn_pane_id: PaneId::from("%fake-pane".to_string()),
                capture_result: Mutex::new(Some("captured output".to_string())),
                kill_should_fail: Mutex::new(false),
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
            _jwt_token: &str,
        ) -> anyhow::Result<PathBuf> {
            self.calls.lock().unwrap().push(Call::CreateWorktree {
                name: name.to_string(),
            });
            let path = self.worktree_dir.join(name);
            std::fs::create_dir_all(&path)?;
            Ok(path)
        }

        fn recreate_worktree(
            &self,
            _repo_root: &Path,
            work_dir: &Path,
            _jwt_token: &str,
        ) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::RecreateWorktree {
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
            _jwt_token: &str,
        ) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::SetupDirConfig {
                work_dir: work_dir.to_path_buf(),
            });
            Ok(())
        }

        fn init_scratch_dir(&self, scratch_dir: &Path) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::InitScratchDir {
                scratch_dir: scratch_dir.to_path_buf(),
            });
            std::fs::create_dir_all(scratch_dir)?;
            Ok(())
        }

        fn launch_agent(&self, config: LaunchConfig) -> anyhow::Result<SpawnResult> {
            self.calls.lock().unwrap().push(Call::LaunchAgent {
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
            _skip_permissions: bool,
        ) -> anyhow::Result<SpawnResult> {
            self.calls.lock().unwrap().push(Call::ResumeAgent {
                task_name: task_name.to_string(),
                work_dir: work_dir.to_path_buf(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn relaunch_agent(&self, task_name: &str, work_dir: &Path) -> anyhow::Result<SpawnResult> {
            self.calls.lock().unwrap().push(Call::ResumeAgent {
                task_name: task_name.to_string(),
                work_dir: work_dir.to_path_buf(),
            });
            Ok(SpawnResult {
                window_id: self.spawn_window_id.clone(),
                pane_id: self.spawn_pane_id.clone(),
            })
        }

        fn send_keys_to_pane(&self, pane_id: &str, message: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::SendKeys {
                pane_id: pane_id.to_string(),
                message: message.to_string(),
            });
            Ok(())
        }

        fn capture_pane_output(&self, pane_id: &str) -> anyhow::Result<String> {
            self.calls.lock().unwrap().push(Call::CaptureOutput {
                pane_id: pane_id.to_string(),
            });
            self.capture_result
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no capture result"))
        }

        fn remove_worktree(&self, path: &Path) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::RemoveWorktree {
                path: path.to_path_buf(),
            });
            Ok(())
        }

        fn kill_tmux_window(&self, window_id: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::KillWindow {
                window_id: window_id.to_string(),
            });
            if *self.kill_should_fail.lock().unwrap() {
                bail!("kill failed");
            }
            Ok(())
        }

        fn select_window(&self, window_id: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(Call::SelectWindow {
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

    async fn spawn_test_task(service: &ClatApp<impl Runtime>) -> SpawnOutput {
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
            .await
            .expect("spawn should succeed")
    }

    #[tokio::test]
    async fn spawn_creates_task_and_calls_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let output = spawn_test_task(&service).await;

        assert_eq!(output.task_name, "test-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        let tasks = service.store().tasks.list_active().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "test-task");
        assert_eq!(
            tasks[0].tmux_pane.as_ref().map(|p| p.as_str()),
            Some("%fake-pane")
        );

        let chat = ChatId::Task(tasks[0].id);
        let messages = service.store().list_messages(&chat).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::System);

        let calls = service.runtime().calls.lock().unwrap();
        assert!(matches!(calls[0], Call::CreateWorktree { .. }));
        assert!(matches!(
            calls[1],
            Call::LaunchAgent {
                has_user_prompt: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn close_captures_output_before_killing_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        let result = service.close(&spawned.task_id.to_string()).await.unwrap();

        assert_eq!(result.task_name, "test-task");

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&spawned.task_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.output.as_deref(), Some("captured output"));

        let calls = service.runtime().calls.lock().unwrap();
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

    #[tokio::test]
    async fn close_rejects_non_running_task() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        service
            .complete(&spawned.task_id.to_string(), 0, None)
            .await
            .unwrap();

        let err = service.close(&spawned.task_id.to_string()).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not 'running'"));
    }

    #[tokio::test]
    async fn close_succeeds_even_if_kill_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        *runtime.kill_should_fail.lock().unwrap() = true;
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        let result = service.close(&spawned.task_id.to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_dispatches_and_records_message() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        let result = service
            .send(&spawned.task_id.to_string(), "hello agent")
            .await
            .unwrap();

        assert_eq!(result.task_name, "test-task");

        {
            let calls = service.runtime().calls.lock().unwrap();
            assert!(calls.iter().any(|c| matches!(c,
                Call::SendKeys { pane_id, message }
                if pane_id == "%fake-pane" && message == "hello agent"
            )));
        }

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&spawned.task_id.to_string())
            .await
            .unwrap()
            .unwrap();
        let chat = ChatId::Task(task.id);
        let messages = service.store().list_messages(&chat).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == MessageRole::User && m.content == "hello agent")
        );
    }

    #[tokio::test]
    async fn goto_calls_select_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        service.goto(&spawned.task_id.to_string()).await.unwrap();

        let calls = service.runtime().calls.lock().unwrap();
        assert!(calls.iter().any(|c| matches!(c,
            Call::SelectWindow { window_id } if window_id == "@fake-win"
        )));
    }

    #[tokio::test]
    async fn goto_errors_on_missing_window() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        // Create a task without launching agent (no tmux_window)
        let new_task = NewTask {
            id: TaskId::new(),
            name: TaskName::from("no-window".to_string()),
            skill_name: "noop".to_string(),
            params_json: "{}".to_string(),
            work_dir: None,
            session_id: ClaudeSessionId::new(),
            project_id: None,
        };
        let task = service.store().tasks.create(new_task).await.unwrap();

        let err = service.goto(&task.id.to_string()).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("no tmux window"));
    }

    #[tokio::test]
    async fn log_returns_messages_and_live_output() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        service
            .send(&spawned.task_id.to_string(), "hello")
            .await
            .unwrap();

        let log = service.log(&spawned.task_id.to_string()).await.unwrap();
        assert_eq!(log.task.name, "test-task");
        assert_eq!(log.messages.len(), 2);
        assert!(log.live_output.is_some());
        assert_eq!(log.live_output.as_deref(), Some("captured output"));
    }

    #[tokio::test]
    async fn list_active_excludes_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned1 = spawn_test_task(&service).await;
        let _spawned2 = spawn_test_task(&service).await;

        service
            .complete(&spawned1.task_id.to_string(), 0, None)
            .await
            .unwrap();

        let active = service.list_active().await.unwrap();
        assert_eq!(active.len(), 1);

        let all = service.list_tasks(true, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn list_skills_returns_available_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
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

        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let skills = service.list_skills().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "deploy");
        assert_eq!(skills[0].description, "deploy to prod");
        assert_eq!(skills[0].params, vec!["env"]);
        assert_eq!(skills[1].name, "noop");
    }

    #[tokio::test]
    async fn reopen_passes_work_dir_to_resume_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        service
            .complete(&spawned.task_id.to_string(), 0, None)
            .await
            .unwrap();

        let window_id = service.reopen(&spawned.task_id.to_string()).await.unwrap();
        assert_eq!(window_id, "@fake-win");

        {
            let calls = service.runtime().calls.lock().unwrap();
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
        }

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&spawned.task_id.to_string())
            .await
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

    #[tokio::test]
    async fn reopen_recreates_missing_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;
        service
            .complete(&spawned.task_id.to_string(), 0, None)
            .await
            .unwrap();

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&spawned.task_id.to_string())
            .await
            .unwrap()
            .unwrap();
        let work_dir = task.work_dir.as_deref().unwrap();
        std::fs::remove_dir_all(work_dir).unwrap();

        let window_id = service.reopen(&spawned.task_id.to_string()).await.unwrap();
        assert_eq!(window_id, "@fake-win");

        {
            let calls = service.runtime().calls.lock().unwrap();
            assert!(
                calls
                    .iter()
                    .any(|c| matches!(c, Call::RecreateWorktree { .. })),
                "expected RecreateWorktree call"
            );
        }

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&spawned.task_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn reopen_rejects_already_running_task() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let spawned = spawn_test_task(&service).await;

        let err = service.reopen(&spawned.task_id.to_string()).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("already running"));
    }

    #[tokio::test]
    async fn spawn_existing_uses_setup_dir_config_and_interactive() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
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
            .await
            .unwrap();

        assert_eq!(output.task_name, "nw-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        let tasks = service.store().tasks.list_active().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "nw-task");

        let calls = service.runtime().calls.lock().unwrap();
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

    #[tokio::test]
    async fn spawn_existing_uses_custom_dir_path() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
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
            .await
            .unwrap();
        assert_eq!(output.task_name, "nw-task");

        let task = service
            .store()
            .tasks
            .maybe_find_by_id_prefix(&output.task_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            task.work_dir.as_deref(),
            Some(custom_repo.to_str().unwrap())
        );

        let calls = service.runtime().calls.lock().unwrap();
        let setup_call = calls
            .iter()
            .find(|c| matches!(c, Call::SetupDirConfig { .. }))
            .unwrap();
        if let Call::SetupDirConfig { work_dir } = setup_call {
            assert_eq!(work_dir, &custom_repo);
        }
    }

    #[tokio::test]
    async fn spawn_scratch_creates_scratch_dir_and_launches_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
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
            .await
            .unwrap();

        assert_eq!(output.task_name, "scratch-task");
        assert_eq!(output.skill_name, "noop");
        assert_eq!(output.window_id, "@fake-win");

        let tasks = service.store().tasks.list_active().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "scratch-task");

        let expected_scratch = tmp.path().join("data").join("scratch").join("scratch-task");
        assert_eq!(
            tasks[0].work_dir.as_deref(),
            Some(expected_scratch.to_str().unwrap())
        );

        let calls = service.runtime().calls.lock().unwrap();
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

    #[tokio::test]
    async fn create_and_list_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let project = service
            .create_project("web-app", "frontend project")
            .await
            .unwrap();
        assert_eq!(project.name, "web-app");
        assert_eq!(project.description, "frontend project");
        assert!(!project.id.to_string().is_empty());

        let projects = service.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "web-app");
    }

    #[tokio::test]
    async fn delete_project_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        service.create_project("temp-proj", "").await.unwrap();
        assert_eq!(service.list_projects().await.unwrap().len(), 1);

        service.delete_project("temp-proj").await.unwrap();
        assert!(service.list_projects().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_project_cascades_to_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let proj = service.create_project("doomed", "").await.unwrap();
        let proj_id = proj.id;

        // Spawn a task inside the project.
        service
            .spawn(SpawnRequest {
                task_name: "proj-task",
                skill_name: "noop",
                params: vec![],
                work_dir_mode: WorkDirMode::Worktree {
                    repo: service.project_root(),
                    branch: None,
                },
                prompt_mode: PromptMode::Full,
                project: Some("doomed".to_string()),
            })
            .await
            .unwrap();

        // Spawn an unscoped task that should survive.
        spawn_test_task(&service).await;

        assert_eq!(service.list_visible(Some(&proj_id)).await.unwrap().len(), 1);
        assert_eq!(service.list_visible(None).await.unwrap().len(), 1);

        // Delete the project — should cascade-delete the project task.
        service.delete_project("doomed").await.unwrap();

        assert!(service.list_projects().await.unwrap().is_empty());
        assert!(
            service
                .list_visible(Some(&proj_id))
                .await
                .unwrap()
                .is_empty(),
            "project task should be deleted"
        );
        assert_eq!(
            service.list_visible(None).await.unwrap().len(),
            1,
            "unscoped task should survive"
        );
    }

    #[tokio::test]
    async fn delete_project_errors_on_unknown_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let err = service.delete_project("ghost").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn list_visible_scoped_to_project() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let store = Store::open_in_memory().await.unwrap();
        let runtime = FakeRuntime::new(tmp.path());
        let service = ClatApp::new(store, runtime, paths);

        let proj = service.create_project("test-proj", "").await.unwrap();
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
            .await
            .unwrap();
        let out2 = spawn_test_task(&service).await;

        let unscoped = service.list_visible(None).await.unwrap();
        assert_eq!(unscoped.len(), 1);
        assert_eq!(unscoped[0].id, out2.task_id);

        let scoped = service.list_visible(Some(&proj_id)).await.unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, out1.task_id);
    }
}
