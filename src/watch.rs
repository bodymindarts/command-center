use std::sync::Arc;

use async_trait::async_trait;
use job::{Job, JobCompletion, JobId, JobInitializer, JobRunner, JobSpawner, JobSvcConfig, Jobs};

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
    _jobs: Jobs,
    timer_spawner: JobSpawner<TimerConfig>,
}

impl WatchService {
    pub async fn init<R: Runtime + Send + Sync + 'static>(
        app: Arc<ClatApp<R>>,
    ) -> anyhow::Result<Self> {
        let config = JobSvcConfig::builder()
            .pool(app.store().pool().clone())
            .exec_migrations(true)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build job config: {e}"))?;
        let mut jobs = Jobs::init(config).await?;
        let timer_spawner = jobs.add_initializer(TimerJobInitializer { app });
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

struct TimerJobInitializer<R: Runtime> {
    app: Arc<ClatApp<R>>,
}

impl<R: Runtime + Send + Sync + 'static> JobInitializer for TimerJobInitializer<R> {
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
impl<R: Runtime + Send + Sync + 'static> JobRunner for TimerJobRunner<R> {
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
