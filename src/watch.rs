use std::sync::Arc;

use async_trait::async_trait;
use job::{Job, JobCompletion, JobInitializer, JobRunner, JobSpawner};

use crate::app::ClatApp;
use crate::runtime::Runtime;

// ── Configs (serialized into the job row) ────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimerConfig {
    pub task_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommandConfig {
    pub task_id: String,
    pub label: String,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

// ── WatchService ──────────────────────────────────────────────────────

pub struct WatchService {
    timer_spawner: JobSpawner<TimerConfig>,
    command_spawner: JobSpawner<CommandConfig>,
    _jobs: job::Jobs,
}

impl WatchService {
    pub(crate) fn new(
        timer_spawner: JobSpawner<TimerConfig>,
        command_spawner: JobSpawner<CommandConfig>,
        jobs: job::Jobs,
    ) -> Self {
        Self {
            timer_spawner,
            command_spawner,
            _jobs: jobs,
        }
    }

    pub async fn create_timer(
        &self,
        task_id: &str,
        label: &str,
        delay_seconds: i64,
        context: Option<serde_json::Value>,
    ) -> anyhow::Result<String> {
        let id = job::JobId::new();
        let config = TimerConfig {
            task_id: task_id.to_string(),
            label: label.to_string(),
            context,
        };
        let scheduled_at = chrono::Utc::now() + chrono::Duration::seconds(delay_seconds);
        self.timer_spawner
            .spawn_at(id, config, scheduled_at)
            .await?;
        Ok(id.to_string())
    }

    pub async fn create_command(
        &self,
        task_id: &str,
        label: &str,
        cmd: &str,
        delay_seconds: i64,
        context: Option<serde_json::Value>,
    ) -> anyhow::Result<String> {
        let id = job::JobId::new();
        let config = CommandConfig {
            task_id: task_id.to_string(),
            label: label.to_string(),
            cmd: cmd.to_string(),
            context,
        };
        let scheduled_at = chrono::Utc::now() + chrono::Duration::seconds(delay_seconds);
        self.command_spawner
            .spawn_at(id, config, scheduled_at)
            .await?;
        Ok(id.to_string())
    }
}

// ── Timer Initializer ────────────────────────────────────────────────

pub(crate) struct TimerJobInitializer<R: Runtime> {
    pub app: Arc<ClatApp<R>>,
}

impl<R: Runtime> JobInitializer for TimerJobInitializer<R> {
    type Config = TimerConfig;

    fn job_type(&self) -> job::JobType {
        job::JobType::new("timer-notify")
    }

    fn init(
        &self,
        job: &Job,
        _spawner: JobSpawner<Self::Config>,
    ) -> Result<Box<dyn JobRunner>, Box<dyn std::error::Error>> {
        let config: TimerConfig = job.config()?;
        Ok(Box::new(TimerJobRunner {
            config,
            app: Arc::clone(&self.app),
        }))
    }
}

// ── Timer Runner ─────────────────────────────────────────────────────

struct TimerJobRunner<R: Runtime> {
    config: TimerConfig,
    app: Arc<ClatApp<R>>,
}

#[async_trait]
impl<R: Runtime> JobRunner for TimerJobRunner<R> {
    async fn run(
        &self,
        _current_job: job::CurrentJob,
    ) -> Result<JobCompletion, Box<dyn std::error::Error>> {
        let mut message = format!("[Watch: {}] Timer fired.", self.config.label);
        if let Some(ref ctx) = self.config.context {
            message.push_str(&format!("\nContext: {}", ctx));
        }

        if let Err(e) = self.app.send(&self.config.task_id, &message).await {
            tracing::warn!(
                task_id = %self.config.task_id,
                label = %self.config.label,
                error = %e,
                "failed to deliver timer notification"
            );
        }

        Ok(JobCompletion::Complete)
    }
}

// ── Command Initializer ──────────────────────────────────────────────

pub(crate) struct CommandJobInitializer<R: Runtime> {
    pub app: Arc<ClatApp<R>>,
}

impl<R: Runtime> JobInitializer for CommandJobInitializer<R> {
    type Config = CommandConfig;

    fn job_type(&self) -> job::JobType {
        job::JobType::new("command-notify")
    }

    fn init(
        &self,
        job: &Job,
        _spawner: JobSpawner<Self::Config>,
    ) -> Result<Box<dyn JobRunner>, Box<dyn std::error::Error>> {
        let config: CommandConfig = job.config()?;
        Ok(Box::new(CommandJobRunner {
            config,
            app: Arc::clone(&self.app),
        }))
    }
}

// ── Command Runner ───────────────────────────────────────────────────

struct CommandJobRunner<R: Runtime> {
    config: CommandConfig,
    app: Arc<ClatApp<R>>,
}

#[async_trait]
impl<R: Runtime> JobRunner for CommandJobRunner<R> {
    async fn run(
        &self,
        _current_job: job::CurrentJob,
    ) -> Result<JobCompletion, Box<dyn std::error::Error>> {
        // Resolve the task's work_dir for command cwd, if available.
        let work_dir = self
            .app
            .task_work_dir(&self.config.task_id)
            .await
            .ok()
            .flatten();

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&self.config.cmd);
        if let Some(ref dir) = work_dir {
            cmd.current_dir(dir);
        }

        let output = cmd.output().await;

        let message = match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if output.status.success() {
                    let mut msg = format!("[Watch: {}] Command output:", self.config.label);
                    if !stdout.is_empty() {
                        msg.push_str(&format!("\n{}", stdout.trim_end()));
                    }
                    if !stderr.is_empty() {
                        msg.push_str(&format!("\nstderr: {}", stderr.trim_end()));
                    }
                    msg
                } else {
                    let code = output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let mut msg = format!(
                        "[Watch: {}] Command failed (exit code {code}):",
                        self.config.label
                    );
                    if !stderr.is_empty() {
                        msg.push_str(&format!("\n{}", stderr.trim_end()));
                    }
                    if !stdout.is_empty() {
                        msg.push_str(&format!("\n{}", stdout.trim_end()));
                    }
                    msg
                }
            }
            Err(e) => {
                format!(
                    "[Watch: {}] Command failed to execute: {e}",
                    self.config.label
                )
            }
        };

        let full_message = if let Some(ref ctx) = self.config.context {
            format!("{message}\nContext: {ctx}")
        } else {
            message
        };

        if let Err(e) = self.app.send(&self.config.task_id, &full_message).await {
            tracing::warn!(
                task_id = %self.config.task_id,
                label = %self.config.label,
                error = %e,
                "failed to deliver command notification"
            );
        }

        Ok(JobCompletion::Complete)
    }
}
