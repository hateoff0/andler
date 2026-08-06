use andler_core::InstanceId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("instance {0:?} not found in store")]
    NotFound(InstanceId),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("failed to (de)serialize stored instance data: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("blocking store task failed to complete: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}
