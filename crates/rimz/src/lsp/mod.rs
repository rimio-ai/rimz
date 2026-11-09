//! Machine-shared, checkout-scoped language-server admission and query domain.

pub mod admission;
pub mod attach;
pub mod broker;
pub mod check;
pub mod history;
mod installed;
pub mod memory;
pub mod protocol;
pub mod query;
pub mod registry;
pub(crate) mod retire;
mod server;
pub mod usage;

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
        "this command ran inside the agent's command sandbox, which blocks the socket connection \
         to RimZ's language-server broker: Operation not permitted. Rerun it bare, with no \
         redirect, pipe, `;` or `&&` chain, `$?`, or `VAR=` prefix, so a command with a \
         routine-command rule runs outside the sandbox. `rimz lsp stop` and `rimz lsp attach` \
         have no such rule, nor does any command when `[agents] allow-routine-rimz = false`: \
         approve running those outside the sandbox."
    )]
    CommandSandbox,
    #[error(
        "file not found: {path}; looked in {readings}{rule}",
        path = path.display(),
        rule = if path.is_absolute() { "" } else {
            "; relative paths are read from the current folder when it is inside the checkout, \
             then the checkout root"
        }
    )]
    MissingFile {
        path: std::path::PathBuf,
        readings: String,
    },
    #[error(
        "{path}:{line} has no column; {hint}",
        path = path.display(),
        hint = item.as_ref().map(|item| format!(
            "line {line} names {} {} at {}:{line}:{}",
            item.kind, item.name, path.display(), item.column
        )).unwrap_or_else(|| {
            "use path:line:col with 1-based line and column, or path:line:name".into()
        })
    )]
    MissingColumn {
        path: std::path::PathBuf,
        line: u32,
        item: Option<NamedItem>,
    },
    #[error(
        "{path}:{line}:{name} names a qualified symbol; path:line:name takes one identifier: \
         use {path}:{line}:{last}",
        path = path.display()
    )]
    QualifiedLineName {
        path: std::path::PathBuf,
        line: u32,
        name: String,
        last: String,
    },
    #[error(
        "{path}:{line} {}",
        if *line == 0 {
            format!("is not a line; lines count from 1 and {} has {lines}", path.display())
        } else {
            format!(
                "is past the end of {}, which has {lines} line{}",
                path.display(),
                if *lines == 1 { "" } else { "s" }
            )
        },
        path = path.display()
    )]
    LineOutside {
        path: std::path::PathBuf,
        line: u32,
        lines: usize,
    },
    #[error("not found: {name} on {path}:{line}, which reads: {text}", path = path.display())]
    NameNotOnLine {
        path: std::path::PathBuf,
        line: u32,
        name: String,
        text: String,
    },
    #[error(
        "ambiguous: {name} appears {count} times on {path}:{line}; \
         rerun with one of these targets:{targets}",
        count = columns.len(),
        path = path.display(),
        targets = columns.iter().map(|column| format!("\n{}:{line}:{column}", path.display()))
            .collect::<String>()
    )]
    NameRepeatsOnLine {
        path: std::path::PathBuf,
        line: u32,
        name: String,
        columns: Vec<u64>,
    },
    #[error("language server error {code}: {message}")]
    Server { code: i64, message: String },
}

type Result<T> = std::result::Result<T, LspErr>;
