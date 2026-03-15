mod entity;
mod error;
mod repo;

pub use entity::*;
pub use repo::*;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use job::{Job, JobCompletion, JobInitializer, JobRunner, JobSpawner};

use crate::app::ClatApp;
use crate::primitives::{CheckType, TaskId, WatchId};
use crate::runtime::Runtime;

/// Commands allowed in the `command` check variant.
///
/// The first token of the shell command (before any args, pipes, or
/// redirections) must be one of these binaries.
const ALLOWED_COMMANDS: &[&str] = &["gh", "curl", "git"];

fn validate_command(cmd: &str) -> anyhow::Result<()> {
    let bin = cmd
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    if ALLOWED_COMMANDS.contains(&bin) {
        Ok(())
    } else {
        anyhow::bail!(
            "command '{bin}' is not allowed. Permitted commands: {}",
            ALLOWED_COMMANDS.join(", ")
        )
    }
}

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

// ── WatchSummary (returned by list_watches) ──────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct WatchSummary {
    pub id: String,
    pub name: String,
    pub label: String,
    pub check_type: CheckType,
    pub fires_at: String,
    pub status: String,
}

// ── WatchService ──────────────────────────────────────────────────────

pub struct WatchService {
    timer_spawner: JobSpawner<TimerConfig>,
    command_spawner: JobSpawner<CommandConfig>,
    repo: WatchRepo,
    jobs: job::Jobs,
}

impl WatchService {
    pub(crate) fn new(
        timer_spawner: JobSpawner<TimerConfig>,
        command_spawner: JobSpawner<CommandConfig>,
        watches: WatchRepo,
        jobs: job::Jobs,
    ) -> Self {
        Self {
            timer_spawner,
            command_spawner,
            repo: watches,
            jobs,
        }
    }

    pub async fn create_timer(
        &self,
        task_id: TaskId,
        label: &str,
        delay_seconds: i64,
        context: Option<serde_json::Value>,
        name: &str,
    ) -> anyhow::Result<String> {
        let job_id = job::JobId::new();
        let scheduled_at = Utc::now() + chrono::Duration::seconds(delay_seconds);

        let watch_id = if let Some(mut existing) = self
            .repo
            .find_active_by_task_and_name(task_id, name)
            .await?
        {
            let old_job_id = existing.job_id.clone();
            existing.reschedule(label, &job_id.to_string(), CheckType::Timer, scheduled_at);
            self.repo.update(&mut existing).await?;
            self.try_cancel_job(&old_job_id).await;
            existing.id
        } else {
            let id = WatchId::new();
            let new_watch = NewWatch {
                id,
                task_id,
                name: name.to_string(),
                label: label.to_string(),
                job_id: job_id.to_string(),
                check_type: CheckType::Timer,
                fires_at: scheduled_at,
            };
            self.repo.create(new_watch).await?;
            id
        };

        let config = TimerConfig {
            task_id: task_id.to_string(),
            label: label.to_string(),
            context,
        };
        self.timer_spawner
            .spawn_at(job_id, config, scheduled_at)
            .await?;

        Ok(watch_id.to_string())
    }

    pub async fn create_command(
        &self,
        task_id: TaskId,
        label: &str,
        cmd: &str,
        delay_seconds: i64,
        context: Option<serde_json::Value>,
        name: &str,
    ) -> anyhow::Result<String> {
        validate_command(cmd)?;

        let job_id = job::JobId::new();
        let scheduled_at = Utc::now() + chrono::Duration::seconds(delay_seconds);

        let watch_id = if let Some(mut existing) = self
            .repo
            .find_active_by_task_and_name(task_id, name)
            .await?
        {
            let old_job_id = existing.job_id.clone();
            existing.reschedule(label, &job_id.to_string(), CheckType::Command, scheduled_at);
            self.repo.update(&mut existing).await?;
            self.try_cancel_job(&old_job_id).await;
            existing.id
        } else {
            let id = WatchId::new();
            let new_watch = NewWatch {
                id,
                task_id,
                name: name.to_string(),
                label: label.to_string(),
                job_id: job_id.to_string(),
                check_type: CheckType::Command,
                fires_at: scheduled_at,
            };
            self.repo.create(new_watch).await?;
            id
        };

        let config = CommandConfig {
            task_id: task_id.to_string(),
            label: label.to_string(),
            cmd: cmd.to_string(),
            context,
        };
        self.command_spawner
            .spawn_at(job_id, config, scheduled_at)
            .await?;

        Ok(watch_id.to_string())
    }

    /// Cancel a specific watch by ID prefix (scoped to the calling task).
    pub async fn cancel_watch(&self, task_id: TaskId, watch_id_prefix: &str) -> anyhow::Result<()> {
        let mut watch = self
            .repo
            .maybe_find_by_id_prefix(watch_id_prefix)
            .await?
            .ok_or_else(|| anyhow::anyhow!("watch not found: {watch_id_prefix}"))?;

        if watch.task_id != task_id {
            anyhow::bail!("watch does not belong to this task");
        }

        if !watch.status.is_active() {
            anyhow::bail!("watch is not active (status: {})", watch.status);
        }

        let _ = watch.cancel("cancelled by agent");
        self.repo.update(&mut watch).await?;

        // Best-effort cancel the underlying job.
        self.try_cancel_job(&watch.job_id).await;
        Ok(())
    }

    /// List all active watches for a task.
    pub async fn list_watches(&self, task_id: TaskId) -> anyhow::Result<Vec<WatchSummary>> {
        let watches = self.repo.list_active_for_task(task_id).await?;
        Ok(watches
            .into_iter()
            .map(|w| WatchSummary {
                id: w.id.short(),
                name: w.name,
                label: w.label,
                check_type: w.check_type,
                fires_at: w.fires_at.to_rfc3339(),
                status: w.status.as_str().to_string(),
            })
            .collect())
    }

    /// Check if a watch (by job_id) is still active. Used by runners before delivery.
    pub async fn check_and_fire(&self, job_id: &str) -> anyhow::Result<bool> {
        let watch = self.repo.find_by_job_id(job_id.to_string()).await.ok();
        match watch {
            Some(mut w) if w.status.is_active() => match w.fire() {
                Ok(_) => {
                    self.repo.update(&mut w).await?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            },
            Some(_) => Ok(false), // Watch exists but not active — skip delivery
            None => {
                // No watch entity found — legacy job or data inconsistency. Deliver anyway.
                tracing::warn!(job_id, "no watch entity found for job, delivering anyway");
                Ok(true)
            }
        }
    }

    /// Best-effort cancel a job by its string ID.
    async fn try_cancel_job(&self, job_id: &str) {
        if let Ok(id) = job_id.parse::<job::JobId>()
            && let Err(e) = self.jobs.cancel_job(id).await
        {
            tracing::debug!(job_id, error = %e, "could not cancel job (may already be running/completed)");
        }
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
            job_id: job.id.to_string(),
            config,
            app: Arc::clone(&self.app),
        }))
    }
}

// ── Timer Runner ─────────────────────────────────────────────────────

struct TimerJobRunner<R: Runtime> {
    job_id: String,
    config: TimerConfig,
    app: Arc<ClatApp<R>>,
}

#[async_trait]
impl<R: Runtime> JobRunner for TimerJobRunner<R> {
    async fn run(
        &self,
        _current_job: job::CurrentJob,
    ) -> Result<JobCompletion, Box<dyn std::error::Error>> {
        // Check if the watch is still active before delivering.
        let should_deliver = self.app.watch().check_and_fire(&self.job_id).await?;
        if !should_deliver {
            tracing::info!(
                job_id = %self.job_id,
                label = %self.config.label,
                "watch no longer active, skipping delivery"
            );
            return Ok(JobCompletion::Complete);
        }

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
            job_id: job.id.to_string(),
            config,
            app: Arc::clone(&self.app),
        }))
    }
}

// ── Command Runner ───────────────────────────────────────────────────

struct CommandJobRunner<R: Runtime> {
    job_id: String,
    config: CommandConfig,
    app: Arc<ClatApp<R>>,
}

#[async_trait]
impl<R: Runtime> JobRunner for CommandJobRunner<R> {
    async fn run(
        &self,
        _current_job: job::CurrentJob,
    ) -> Result<JobCompletion, Box<dyn std::error::Error>> {
        // Check if the watch is still active before delivering.
        let should_deliver = self.app.watch().check_and_fire(&self.job_id).await?;
        if !should_deliver {
            tracing::info!(
                job_id = %self.job_id,
                label = %self.config.label,
                "watch no longer active, skipping delivery"
            );
            return Ok(JobCompletion::Complete);
        }

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
