//! Machine-shared, checkout-scoped language-server admission and query domain.

pub mod admission;
pub mod attach;
pub mod broker;
pub mod check;
pub mod history;
pub mod memory;
pub mod protocol;
pub mod query;
pub mod registry;
mod server;

#[derive(Debug)]
pub struct NamedItem {
    kind: &'static str,
    name: String,
    column: u64,
}

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
    #[error(
        "{path}:{line} has no column; {hint}",
        path = path.display(),
        hint = item.as_ref().map(|item| format!(
            "line {line} names {} {} at {}:{line}:{}",
            item.kind, item.name, path.display(), item.column
        )).unwrap_or_else(|| "use path:line:col with 1-based line and column".into())
    )]
    MissingColumn {
        path: std::path::PathBuf,
        line: u32,
        item: Option<NamedItem>,
    },
    #[error("language server error {code}: {message}")]
    Server { code: i64, message: String },
}

type Result<T> = std::result::Result<T, LspErr>;
