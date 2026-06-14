//! The `rimz doctor` report as pure data: one [`DoctorReport`] assembled by the
//! `collect_*` functions in the sibling modules, then either serialized to JSON
//! or handed to [`super::render`] for the human report. Typed states (a `false`
//! `fits`, a `TrustState::Stale`, a `HookStatus::NotInstalled`) carry the verdict;
//! the glyph-and-color mapping lives entirely in the renderer.

use jiff::Timestamp;
use serde::Serialize;

use rimz::feed::AgentStatus;
use rimz::ids::MuxName;
use rimz::schema::diag::DiagSeverity;
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
    pub(super) workspace: Probe<Workspace>,
    pub(super) mux: Probe<Mux>,
    pub(super) sidebar_renderer: &'static str,
    pub(super) hooks: Vec<HookRow>,
    pub(super) coverage: Vec<AgentCoverage>,
    pub(super) remote_control: RemoteControl,
    pub(super) rooms: Probe<Rooms>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) protocols: Option<Protocols>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) trust: Option<Probe<Trust>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) resolver_heartbeats: Option<Probe<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) agents: Option<AgentRollup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) diagnostics: Option<Diagnostics>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) zellij_socket: Option<ZellijSocket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) session_health: Option<Probe<SessionHealth>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) duplicate_sessions: Option<Probe<DuplicateSessions>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) presence: Option<Presence>,
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

/// One adapter's integration-concern coverage: the wired concerns and, for each
/// gap, its reason.
#[derive(Debug, Serialize)]
pub(super) struct AgentCoverage {
    pub(super) kind: String,
    pub(super) wired: usize,
    pub(super) total: usize,
    pub(super) supported: Vec<String>,
    pub(super) unsupported: Vec<UnsupportedConcern>,
}

#[derive(Debug, Serialize)]
pub(super) struct UnsupportedConcern {
    pub(super) concern: String,
    pub(super) reason: String,
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
        refusals: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct RemoteAgent {
    pub(super) label: String,
    pub(super) ready: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Rooms {
    pub(super) recorded: usize,
    pub(super) live: usize,
    pub(super) rooms: Vec<Room>,
    pub(super) overlaps: Vec<RoomOverlap>,
}

#[derive(Debug, Serialize)]
pub(super) struct Room {
    pub(super) session_name: String,
    pub(super) project_root: String,
    pub(super) root_class: RootClass,
    pub(super) live: bool,
    pub(super) is_current: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct RoomOverlap {
    pub(super) a: String,
    pub(super) b: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Protocols {
    pub(super) event: &'static str,
    pub(super) sidebar: &'static str,
    pub(super) resolver: &'static str,
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
    Unavailable { error: String },
    None,
    Observed { groups: Vec<AgentKindGroup> },
}

#[derive(Debug, Serialize)]
pub(super) struct AgentKindGroup {
    pub(super) kind: String,
    pub(super) agents: Vec<AgentRow>,
}

#[derive(Debug, Serialize)]
pub(super) struct AgentRow {
    pub(super) agent_id: String,
    pub(super) branch: Option<String>,
    pub(super) status: AgentStatus,
    pub(super) phase: rimz::agents::TurnPhase,
    pub(super) last_seen: Timestamp,
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
