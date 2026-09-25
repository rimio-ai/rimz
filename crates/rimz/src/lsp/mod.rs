//! Machine-shared, checkout-scoped language-server admission and query domain.

pub mod admission;
pub mod history;
pub mod lease;
pub mod memory;
pub mod protocol;
pub mod query;
pub mod registry;

#[derive(Debug, thiserror::Error)]
pub enum LspErr {
    #[error(transparent)]
    Path(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    QueueTimeout(Box<admission::QueueTimeout>),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    SocketPath(#[from] crate::sock::SocketPathTooLong),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid language-server configuration: {0}")]
    Configuration(String),
    #[error("language-server protocol: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, LspErr>;
