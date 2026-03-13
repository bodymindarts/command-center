use std::sync::Arc;

use async_trait::async_trait;
use job::{Job, JobCompletion, JobId, JobInitializer, JobRunner, JobSpawner, JobSvcConfig, Jobs};
use sqlx::SqlitePool;

use crate::task::TaskRepo;

// ── Config (serialized into the job row) ──────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimerConfig {
    pub task_id: String,
    pub label: String,
}

// ── WatchService ──────────────────────────────────────────────────────

pub struct WatchService {
    _jobs: Jobs,
    timer_spawner: JobSpawner<TimerConfig>,
}

impl WatchService {
    pub async fn init(pool: &SqlitePool) -> anyhow::Result<Self> {
        let config = JobSvcConfig::builder()
            .pool(pool.clone())
            .exec_migrations(true)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build job config: {e}"))?;
        let mut jobs = Jobs::init(config).await?;
        let timer_spawner = jobs.add_initializer(TimerJobInitializer::new(TaskRepo::new(pool)));
        jobs.start_poll().await?;
        Ok(Self {
            _jobs: jobs,
            timer_spawner,
        })
    }

    pub async fn create_timer(
        &self,
        task_id: &str,
        label: &str,
        delay_seconds: i64,
    ) -> anyhow::Result<String> {
        let id = JobId::new();
        let config = TimerConfig {
            task_id: task_id.to_string(),
            label: label.to_string(),
        };
        let scheduled_at = chrono::Utc::now() + chrono::Duration::seconds(delay_seconds);
        self.timer_spawner
            .spawn_at(id, config, scheduled_at)
            .await?;
        Ok(id.to_string())
    }
}

// ── Initializer ───────────────────────────────────────────────────────

struct TimerJobInitializer {
    tasks: Arc<TaskRepo>,
}

impl TimerJobInitializer {
    fn new(tasks: TaskRepo) -> Self {
        Self {
            tasks: Arc::new(tasks),
        }
    }
}

impl JobInitializer for TimerJobInitializer {
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
            tasks: Arc::clone(&self.tasks),
        }))
    }
}

// ── Runner ────────────────────────────────────────────────────────────

struct TimerJobRunner {
    config: TimerConfig,
    tasks: Arc<TaskRepo>,
}

#[async_trait]
impl JobRunner for TimerJobRunner {
    async fn run(
        &self,
        _current_job: job::CurrentJob,
    ) -> Result<JobCompletion, Box<dyn std::error::Error>> {
        let task = self
            .tasks
            .find_by_id_prefix(&self.config.task_id)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

        let Some(task) = task else {
            tracing::warn!(
                task_id = %self.config.task_id,
                label = %self.config.label,
                "timer fired but task not found, completing anyway"
            );
            return Ok(JobCompletion::Complete);
        };

        let Some(ref pane_id) = task.tmux_pane else {
            tracing::warn!(
                task_id = %self.config.task_id,
                label = %self.config.label,
                "timer fired but task has no pane, completing anyway"
            );
            return Ok(JobCompletion::Complete);
        };

        let message = format!("[Watch: {}] Timer fired.", self.config.label);
        if let Err(e) = deliver_to_pane(pane_id.as_str(), &message) {
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

// ── Delivery helper ───────────────────────────────────────────────────

/// Send a message to a tmux pane by shelling out directly.
fn deliver_to_pane(pane_id: &str, message: &str) -> anyhow::Result<()> {
    use std::process::Command;

    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", message])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux send-keys failed: {stderr}");
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    let output = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "Enter"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux send-keys Enter failed: {stderr}");
    }

    Ok(())
}
