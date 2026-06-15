use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Scheduled window-priming pings. Each entry fires a lowest-effort `<kind>-ping`
/// turn at a chosen time, so the provider's sliding budget window starts on your
/// schedule instead of whenever you first sit down. Per-machine and outside the
/// trust hash: the only thing an entry runs is the rimz-owned `autoping run`,
/// never arbitrary shell, so a clone never inherits it and project trust never
/// gates it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoPingConfig {
    pub schedules: Schedules,
}

/// Named schedule entries, ordered by name. A map keeps `rimz autoping
/// add/remove/install` addressing one entry by a stable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Schedules(pub BTreeMap<String, ScheduleEntry>);

/// One scheduled ping. The firing time is either the friendly `at` + optional
/// `days`, or the raw `cron` escape hatch; the kind, root, and worktree name the
/// ping the OS scheduler drives through `rimz autoping run <name>`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ScheduleEntry {
    /// Agent kind to prime; must support a ping turn (e.g. `claude`, `codex`).
    pub kind: String,
    /// Absolute project root whose room hosts the ping. Resolved at add time so
    /// the OS scheduler entry routes to the room deterministically, with no mux
    /// environment pin to read.
    pub root: PathBuf,
    /// Optional channel/worktree to host the transient ping pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Daily firing time in 24h `HH:MM`, local wall-clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Day mask: `daily`, `weekdays`, `weekends`, or a comma list `mon,wed,fri`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<String>,
    /// Raw 5-field cron expression escape hatch (cron backend only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
}
