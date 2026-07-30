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
    pub(super) schema: &'static str,
    pub(super) version: &'static str,
    pub(super) host: Host,
    pub(super) workspace: Probe<Workspace>,
    pub(super) mux: Probe<Mux>,
    pub(super) terminal: Terminal,
    pub(super) machine_config: MachineConfigHealth,
    pub(super) hooks: Vec<HookRow>,
    pub(super) plugins: Vec<PluginRow>,
    pub(super) loop_tasks: LoopTasks,
    pub(super) remote_control: RemoteControl,
    pub(super) disk_usage: Storage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) protocols: Option<Protocols>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) trust: Option<Probe<Trust>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) agents: Option<AgentRollup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) history_cleared_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) messages: Option<Probe<Messages>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) diagnostics: Option<Diagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_incident: Option<LastIncident>,
}

#[derive(Debug, Serialize)]
pub(super) struct MachineConfigHealth {
    pub(super) broken_files: Vec<MachineConfigProblem>,
}

#[derive(Debug, Serialize)]
pub(super) struct MachineConfigProblem {
    pub(super) path: String,
    pub(super) error: String,
    #[serde(skip)]
    pub(super) kind: MachineConfigProblemKind,
}

#[derive(Debug)]
pub(super) enum MachineConfigProblemKind {
    Fragment,
    Parse,
    Semantic,
}

#[derive(Debug, Serialize)]
pub(super) struct LastIncident {
    pub(super) cause: &'static str,
    pub(super) at: Timestamp,
    pub(super) lost_agents: Vec<IncidentAgent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) recovered: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) forensics: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct IncidentAgent {
    pub(super) kind: String,
    pub(super) name: Option<String>,
    pub(super) agent_id: String,
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
pub(super) struct LegacySession {
    pub(super) session: String,
    pub(super) socket: String,
    /// Session-scoped, so unrelated sessions on that server are untouched.
    pub(super) fix: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Mux {
    pub(super) name: MuxName,
    pub(super) version: Version,
    pub(super) capabilities: Capabilities,
    pub(super) binaries: MuxBinaries,
    pub(super) log: MuxLog,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) room: Option<Room>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) presence_plugins: Option<Probe<PresencePlugins>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) zellij_socket: Option<ZellijSocket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) socket: Option<String>,
    /// A same-named RimZ session stranded on the user's default tmux server by
    /// a release predating the managed endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) legacy_session: Option<LegacySession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) session_health: Option<Probe<SessionHealth>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) duplicate_sessions: Option<Probe<DuplicateSessions>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) presence: Option<Presence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) topology_writer: Option<TopologyWriterHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ttyd: Option<Probe<TtydWeb>>,
}

#[derive(Debug, Serialize)]
pub(super) struct Room {
    pub(super) session_name: String,
    pub(super) selected_state: RoomState,
    pub(super) live_on: Vec<MuxName>,
    pub(super) conflict: bool,
    pub(super) zellij: RoomState,
    pub(super) tmux: RoomState,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum RoomState {
    Live,
    Exited,
    Absent,
    Unavailable { error: String },
}

#[derive(Debug, Serialize)]
pub(super) struct TtydWeb {
    pub(super) path: String,
    pub(super) version: String,
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
        scope: LogScope,
        size_bytes: u64,
        scanned_bytes: u64,
        logical_records: usize,
        /// Records the `rimz doctor --clear` watermark held out of the verdict.
        records_before_cutoff: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        since: Option<Timestamp>,
        problem_records: usize,
        omitted_issue_groups: usize,
        issues: Vec<MuxLogIssue>,
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum LogScope {
    HostUser { uid: u32 },
    Server,
}

#[derive(Debug, Serialize)]
pub(super) struct MuxLogIssue {
    pub(super) source_severity: String,
    pub(super) state: DoctorState,
    pub(super) impact: DoctorImpact,
    pub(super) summary: String,
    pub(super) occurrences: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) first_occurrence: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_occurrence: Option<Timestamp>,
    pub(super) samples: Vec<String>,
    pub(super) evidence_truncated: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct PresencePlugins {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) desired_build: Option<String>,
    pub(super) rows: Vec<PresencePluginRow>,
    pub(super) history: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PresencePluginRow {
    pub(super) plugin_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) loaded_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) build: Option<String>,
    pub(super) status: PresencePluginStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rejected_count: Option<u64>,
    pub(super) outdated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) telemetry: Option<PresencePluginTelemetry>,
}

#[derive(Debug, Serialize)]
pub(super) struct PresencePluginTelemetry {
    pub(super) sample_count: usize,
    pub(super) first_at_ms: u64,
    pub(super) last_at_ms: u64,
    pub(super) last_seen_age_secs: u64,
    pub(super) zellij_version: Option<String>,
    pub(super) page_growth: i64,
    pub(super) byte_growth: i64,
    pub(super) commands_completed_delta: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) commands_succeeded_delta: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stale_writer_rejections_delta: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) topology_failures_delta: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) other_failures_delta: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_failure: Option<PresenceCommandFailure>,
}

/// What the host said on stderr the last time a presence wake failed, and when.
/// The time is absent for a plugin loaded before the stamp shipped.
#[derive(Debug, Serialize)]
pub(super) struct PresenceCommandFailure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) exit_code: Option<i32>,
    pub(super) detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PresencePluginStatus {
    Active,
    Rejected,
    Inactive,
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
    pub(super) mux: MuxName,
    pub(super) session_name: String,
    pub(super) is_current: bool,
    pub(super) sidebar_count: usize,
    pub(super) pane_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(super) enum Presence {
    Event { poked_secs: u64 },
    Poll { reason: String, expected: bool },
    NotApplicable { reason: String },
    Unavailable { error: String },
}

#[derive(Debug, Serialize)]
pub(super) struct TopologyWriterHealth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) recorded_bin: Option<RecordedRoomBin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) conflict: Option<TopologyWriterConflict>,
}

#[derive(Debug, Serialize)]
pub(super) struct RecordedRoomBin {
    pub(super) path: String,
    pub(super) exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fix: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct TopologyWriterConflict {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stale: Option<TopologyWriterId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) accepted: Option<TopologyWriterId>,
    pub(super) rejected_count: u64,
    pub(super) age_secs: u64,
    pub(super) fix: String,
}

#[derive(Debug, Serialize)]
pub(super) struct TopologyWriterId {
    pub(super) plugin_id: u32,
    pub(super) loaded_at_ms: u64,
}

/// One adapter's RimZ-hook wiring state, with the fix to advance it.
#[derive(Debug, Serialize)]
pub(super) struct HookRow {
    pub(super) kind: String,
    pub(super) detected: bool,
    pub(super) status: HookStatus,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginRow {
    pub(super) kind: String,
    pub(super) manifest: String,
    pub(super) valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) setup_doc: Option<String>,
    pub(super) probes: Vec<PluginProbeRow>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginProbeRow {
    pub(super) name: &'static str,
    pub(super) command: String,
    pub(super) present: bool,
    pub(super) executable: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum HookStatus {
    Installed,
    InstalledUntrusted { events: Vec<String>, fix: String },
    NotInstalled { fix: String },
    NotDetected,
    Unsupported { reason: String },
}

impl HookStatus {
    /// The short status label shared by the human report and the tests.
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::InstalledUntrusted { .. } => "installed, untrusted",
            Self::NotInstalled { .. } => "not installed",
            Self::NotDetected => "not detected",
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
        /// Provider daemon findings that have no effect on `rimz start`.
        advisories: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct RemoteAgent {
    pub(super) kind: &'static str,
    pub(super) detail: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) build_drift: Option<BuildDrift>,
}

/// More than one rimz build is writing this workspace: the distinct builds seen
/// among fresh sidebar heartbeats plus the binary that produced this report.
#[derive(Debug, Serialize)]
pub(super) struct BuildDrift {
    pub(super) writers: Vec<BuildWriter>,
}

#[derive(Debug, Serialize)]
pub(super) struct BuildWriter {
    pub(super) build: String,
    /// This is the build id of the binary running `rimz doctor`.
    pub(super) is_running: bool,
    pub(super) sidebar_count: usize,
    pub(super) pane_ids: Vec<String>,
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
    Ready {
        path: String,
        incidents: Vec<DiagIncident>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DoctorState {
    Investigate,
    Contained,
    Recovered,
    Expected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DoctorImpact {
    Alarm,
    Warn,
    Info,
}

#[derive(Debug, Serialize)]
pub(super) struct DiagIncident {
    pub(super) kind: String,
    pub(super) source_severity: DiagSeverity,
    pub(super) state: DoctorState,
    pub(super) impact: DoctorImpact,
    pub(super) first_at_ms: u64,
    pub(super) last_at_ms: u64,
    pub(super) record_count: usize,
    pub(super) distinct_observer_count: usize,
    pub(super) observer_ids: Vec<String>,
    pub(super) sink_suppressed: u64,
    pub(super) dropped_messages: u64,
    pub(super) summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) build: Option<String>,
    pub(super) stale_build: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) evidence_refs: Vec<String>,
}
