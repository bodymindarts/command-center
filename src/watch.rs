use std::sync::Arc;

use async_trait::async_trait;
use job::{Job, JobCompletion, JobInitializer, JobRunner, JobSpawner};

use crate::app::ClatApp;
use crate::runtime::Runtime;

// ── Config (serialized into the job row) ──────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimerConfig {
    pub task_id: String,
    pub label: String,
}

// ── WatchService ──────────────────────────────────────────────────────

pub struct WatchService {
    timer_spawner: JobSpawner<TimerConfig>,
    _jobs: job::Jobs,
}

impl WatchService {
    pub(crate) fn new(timer_spawner: JobSpawner<TimerConfig>, jobs: job::Jobs) -> Self {
        Self {
            timer_spawner,
            _jobs: jobs,
        }
    }

    pub async fn create_timer(
        &self,
        task_id: &str,
        label: &str,
        delay_seconds: i64,
    ) -> anyhow::Result<String> {
        let id = job::JobId::new();
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

// ── Runner ────────────────────────────────────────────────────────────

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
        let message = format!("[Watch: {}] Timer fired.", self.config.label);
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
