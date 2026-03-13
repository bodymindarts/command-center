use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskError {
    #[error("task is not running")]
    NotRunning,
    #[error("task is already running")]
    AlreadyRunning,
}
