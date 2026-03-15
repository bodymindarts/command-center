use thiserror::Error;

#[derive(Error, Debug)]
pub enum WatchError {
    #[error("watch is not active")]
    NotActive,
}
