//! The `rimz doctor` report as pure data: one [`DoctorReport`] assembled by the
//! `collect_*` functions in the sibling modules, then either serialized to JSON
//! or handed to [`super::render`] for the human report. Typed states (a `false`
//! `fits`, a `TrustState::Stale`, a `HookStatus::NotInstalled`) carry the verdict;
//! the glyph-and-color mapping lives entirely in the renderer.

use jiff::Timestamp;
use serde::Serialize;

use rimz::agents::AgentStatus;
use rimz::diag::record::DiagSeverity;
use rimz::ids::MuxName;
use rimz::trust::TrustState;
use rimz::workspace::RootClass;

/// A section that reads live state: the gathered value, or why it was
/// unavailable. Externally tagged, so JSON reads `{"ready": …}` or
/// `{"unavailable": {"error": "…"}}`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Probe<T> {
    Ready(T),
    Unavailable { error: String },
}

/// The whole report. Workspace-scoped sections are `None` when the workspace
/// could not be resolved at all.
#[derive(Debug, Serialize)]
pub(super) struct DoctorReport {
    pub(super) version: &'static str,
    pub(super) host: Host,
    pub(super) workspace: Probe<Workspace>,
    pub(super) mux: Probe<Mux>,
    pub(super) terminal: Terminal,
    pub(super) hooks: Vec<HookRow>,
    pub(super) loop_tasks: LoopTasks,
    pub(super) remote_control: RemoteControl,
    pub(super) storage: Storage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) protocols: Option<Protocols>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) trust: Option<Probe<Trust>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) agents: Option<AgentRollup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) messages: Option<Probe<Messages>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) diagnostics: Option<Diagnostics>,
}

/// Host/process identity: who is running doctor and from which binary — the
/// facts that pin a workspace to an OS user and a rimz install.
#[derive(Debug, Serialize)]
pub(super) struct Host {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) user: Option<String>,
    pub(super) uid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) binary: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Terminal {
    pub(super) theme_mode: rimz::config::ThemeMode,
    pub(super) truecolor_advertised: bool,
    pub(super) resolved_depth: &'static str,
    pub(super) colorterm: Option<String>,
    pub(super) term: Option<String>,
    pub(super) terminfo_truecolor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Workspace {
    pub(super) workspace_id: String,
    pub(super) project_root: String,
    pub(super) root_class: RootClass,
    pub(super) worktree_root: String,
    pub(super) worktree_branch: Option<String>,
    pub(super) session_name: String,
    pub(super) sock_headroom: Probe<SockBudget>,
}

/// Per-request socket-path budget; `fits == false` is the alarm.
#[derive(Debug, Serialize)]
pub(super) struct SockBudget {
    pub(super) fits: bool,
    pub(super) used: usize,
    pub(super) limit: usize,
    pub(super) dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) remedy: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Mux {
    pub(super) name: MuxName,
    pub(super) version: Version,
    pub(super) capabilities: Capabilities,
    pub(super) binaries: MuxBinaries,
    pub(super) log: MuxLog,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) zellij_socket: Option<ZellijSocket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) socket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) session_health: Option<Probe<SessionHealth>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) duplicate_sessions: Option<Probe<DuplicateSessions>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) presence: Option<Presence>,
}

#[derive(Debug, Serialize)]
pub(super) struct MuxBinaries {
    pub(super) active: Option<MuxBinaryRow>,
    pub(super) duplicates: Vec<MuxBinaryRow>,
    pub(super) server_mismatches: Vec<ServerMismatchRow>,
}

#[derive(Debug, Serialize)]
pub(super) struct MuxBinaryRow {
    pub(super) path: String,
    pub(super) version: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ServerMismatchRow {
    pub(super) pid: u32,
    pub(super) exe: String,
    pub(super) deleted: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum MuxLog {
    Ready {
        path: String,
        size_bytes: u64,
        scanned_bytes: u64,
        matched: usize,
        entries: Vec<MuxLogEntry>,
    },
    Missing {
        path: String,
    },
    Disabled {
        hint: String,
    },
    Unavailable {
        error: String,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct MuxLogEntry {
    pub(super) severity: String,
    pub(super) line: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Version {
    Reported { version: String },
    Unknown,
    Unavailable { error: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub(super) enum Capabilities {
    Zellij(Probe<ZellijCaps>),
    Tmux(Probe<TmuxCaps>),
}

#[derive(Debug, Serialize)]
pub(super) struct ZellijCaps {
    pub(super) meets_min_version: bool,
    pub(super) min_version: (u32, u32, u32),
}

#[derive(Debug, Serialize)]
pub(super) struct TmuxCaps {
    pub(super) meets_min_version: bool,
    pub(super) min_version: (u32, u32, u32),
    pub(super) popup_supported: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct ZellijSocket {
    pub(super) fits: bool,
    pub(super) len: usize,
    pub(super) limit: usize,
    pub(super) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fix: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum SessionHealth {
    Ok,
    Stuck { fix: String },
}

/// Live sidebar sessions that share this workspace. More than one risks a
/// stale producer holding pane updates, so a non-empty `groups` is the warning.
#[derive(Debug, Serialize)]
pub(super) struct DuplicateSessions {
    pub(super) groups: Vec<SidebarGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) advice: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct SidebarGroup {
    pub(super) session_name: String,
    pub(super) is_current: bool,
    pub(super) sidebar_count: usize,
    pub(super) pane_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(super) enum Presence {
    Event { poked_secs: u64 },
    Poll { reason: String },
    Unavailable { error: String },
}

/// One adapter's Rimz-hook wiring state, with the fix to advance it.
#[derive(Debug, Serialize)]
pub(super) struct HookRow {
    pub(super) kind: String,
    pub(super) status: HookStatus,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum HookStatus {
    Installed,
    InstalledUntrusted { events: Vec<String>, fix: String },
    NotInstalled { fix: String },
    Unsupported { reason: String },
}

impl HookStatus {
    /// The short status label shared by the human report and the tests.
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::InstalledUntrusted { .. } => "installed, untrusted",
            Self::NotInstalled { .. } => "not installed",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

/// Loop tasks from config plus transient state: the scheduled-execution surface
/// this machine carries, surfaced so it is visible. Room-open state lives in
/// `rimz loop list`.
#[derive(Debug, Serialize)]
pub(super) struct LoopTasks {
    pub(super) tasks: Vec<LoopTaskRow>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoopTaskRow {
    pub(super) name: String,
    pub(super) spec: String,
    pub(super) when: String,
    pub(super) root: String,
    pub(super) valid: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum RemoteControl {
    Unavailable {
        error: String,
    },
    Off,
    On {
        agents: Vec<RemoteAgent>,
        /// Fixable misconfigurations on installed agents — `rimz start` aborts
        /// on these.
        refusals: Vec<String>,
        /// Enabled hosts whose agent is not installed — `rimz start` skips them.
        skipped: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct RemoteAgent {
    pub(super) label: String,
    pub(super) ready: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Storage {
    pub(super) total_bytes: u64,
    pub(super) roots: Vec<StorageRootView>,
}

#[derive(Debug, Serialize)]
pub(super) struct StorageRootView {
    pub(super) label: &'static str,
    pub(super) path: String,
    pub(super) bytes: u64,
    pub(super) present: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Protocols {
    pub(super) event: &'static str,
    pub(super) sidebar: &'static str,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Trust {
    pub(super) state: TrustState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) granted_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum AgentRollup {
    Unavailable {
        error: String,
    },
    None,
    Observed {
        counts: AgentCounts,
        rows: Vec<AgentRow>,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct AgentRow {
    pub(super) kind: String,
    pub(super) agent_id: String,
    pub(super) branch: Option<String>,
    pub(super) status: AgentStatus,
    pub(super) phase: rimz::agents::TurnPhase,
    pub(super) last_seen: Timestamp,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct AgentCounts {
    pub(super) running: usize,
    pub(super) waiting: usize,
    pub(super) idle: usize,
    pub(super) success: usize,
    pub(super) failed: usize,
    pub(super) paused: usize,
}

impl AgentCounts {
    pub(super) fn add(&mut self, status: AgentStatus) {
        match status {
            AgentStatus::Running => self.running += 1,
            AgentStatus::Waiting => self.waiting += 1,
            AgentStatus::Idle => self.idle += 1,
            AgentStatus::Success => self.success += 1,
            AgentStatus::Failed => self.failed += 1,
            AgentStatus::Paused => self.paused += 1,
        }
    }

    pub(super) fn total(&self) -> usize {
        self.running + self.waiting + self.idle + self.success + self.failed + self.paused
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Messages {
    pub(super) open: OpenCounts,
    pub(super) stuck: Vec<MessageProblemRow>,
    pub(super) recent_failures: Vec<MessageProblemRow>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct OpenCounts {
    pub(super) queued: usize,
    pub(super) claimed: usize,
    pub(super) sent: usize,
}

impl OpenCounts {
    pub(super) fn total(&self) -> usize {
        self.queued + self.claimed + self.sent
    }
}

#[derive(Debug, Serialize)]
pub(super) struct MessageProblemRow {
    pub(super) message_id: String,
    pub(super) status: String,
    pub(super) target: String,
    pub(super) at: Timestamp,
    pub(super) problem: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Diagnostics {
    Unavailable,
    Ready { path: String, records: Vec<DiagRow> },
}

#[derive(Debug, Serialize)]
pub(super) struct DiagRow {
    pub(super) severity: DiagSeverity,
    pub(super) kind: String,
    pub(super) at_ms: u64,
    pub(super) summary: String,
}
