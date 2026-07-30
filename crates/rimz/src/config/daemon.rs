use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The rimzd daemon view's middle column. Unset or empty keeps the built-in
/// held live-stats pane (`rimz stats --refresh --hold`); listing panes replaces
/// or extends it. A running room reloads command/cwd edits on save; pane-count
/// changes take effect on room restart. Per-machine personal policy, outside
/// the project trust hash.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DaemonConfig {
    /// Middle-column panes, top to bottom. Empty means the built-in stats pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane: Vec<DaemonPane>,
}

/// One middle-column pane. `command = "stats"` is the reserved token for the
/// live-stats pane; any other command is split into argv and run directly.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonPane {
    pub command: String,
    /// Working directory. Absolute paths are used as-is; relative paths are
    /// joined onto the worktree root; absent runs from the worktree root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}
