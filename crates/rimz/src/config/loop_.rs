use std::collections::BTreeMap;
use std::ops::Not;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `[agents.loop]`: scheduled and automated agent-loop helpers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoopConfig {
    pub tasks: Tasks,
}

/// Named loop tasks, ordered by name. A map keeps `rimz loop
/// add/remove/install` addressing one task by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Tasks(pub BTreeMap<String, TaskEntry>);

/// One scheduled supervised run. The firing time is either a calendar time, an
/// interval, or a raw cron escape hatch; the spec resolves to one agent cell.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TaskEntry {
    pub spec: String,
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
