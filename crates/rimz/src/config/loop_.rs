use std::collections::BTreeMap;
use std::ops::Not;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `[agents.loop]`: scheduled and automated agent-loop helpers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoopConfig {
    pub tasks: Tasks,
}

/// Named loop tasks, ordered by name. A map keeps `rimz loop add/remove/run`
/// addressing one task by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Tasks(pub BTreeMap<String, TaskEntry>);

/// One scheduled loop wake-up. The firing time is either a calendar time, an
/// interval, or a raw cron escape hatch; `spec` spawns a supervised turn and
/// `bind` delivers to a pinned session.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<TaskTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(rename = "prompt-file", skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    pub root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(rename = "system-prompt-file", skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Not::not")]
    pub once: bool,
}

impl TaskEntry {
    /// Root normalized for workspace identity and execution. CLI-added tasks
    /// already store this shape; hand-edited tasks may use `~` or a relative
    /// path.
    pub fn resolved_root(&self) -> PathBuf {
        resolve_root_with(&self.root, home_dir())
    }
}

/// A loop delivery target pinned to the exact live agent session that scheduled
/// it. The handle is display-only; `session` is the durable address.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskTarget {
    pub kind: String,
    pub session: String,
    pub handle: String,
}

fn resolve_root_with(root: &Path, home: PathBuf) -> PathBuf {
    let raw = root.to_string_lossy();
    let expanded = if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        root.to_path_buf()
    };
    expanded.canonicalize().unwrap_or(expanded)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_root_expands_tilde_prefix() {
        let home = PathBuf::from("/home/dev");
        assert_eq!(
            resolve_root_with(Path::new("~/workspace/app"), home.clone()),
            home.join("workspace/app")
        );
        assert_eq!(resolve_root_with(Path::new("~"), home.clone()), home);
        assert_eq!(
            resolve_root_with(Path::new("~other/app"), PathBuf::from("/home/dev")),
            PathBuf::from("~other/app")
        );
    }

    #[test]
    fn resolve_root_canonicalizes_existing_absolute_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("mkdir nested");
        let dotted = nested.join(".");

        assert_eq!(
            resolve_root_with(&dotted, PathBuf::from("/home/dev")),
            nested.canonicalize().expect("canonical nested")
        );
    }
}
