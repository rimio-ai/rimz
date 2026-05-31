//! Agent integration interface.
//!
//! Each adapter classifies an incoming hook event, observes lifecycle
//! transitions (status + mode), renders the agent-native neutral stdout
//! payload, and (when a resolver answer is available) renders the
//! agent-native decision JSON. Adapters never touch the ledger directly;
//! they're called by `rimz hooks <agent>` which owns the ledger writes.
//!
//! Adapters also own hook install and uninstall — translating the trait
//! defaults into whatever per-agent config file the upstream agent reads.

pub mod account;
pub mod claude;
pub mod codex;
pub(crate) mod codex_app_server;
pub mod codex_broker;
pub mod context;
pub mod statusline;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::feed::{AgentStatus, FeedItem, FeedKind, PermissionPosture, Resolution, RuntimeOwner};
use crate::ids::PaneId;

pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits,
    AgentTokenUsage, RateLimitWindow,
};

/// Conservative fallback for adapters that don't override. Claude overrides
/// to 120s (see `claude::CLAUDE_HOOK_CAP`); Codex overrides to its own cap
/// (see `codex::CODEX_HOOK_CAP`). New adapters should set their own cap
/// based on the upstream's published hook deadline.
pub const DEFAULT_HOOK_CAP: Duration = Duration::from_secs(300);

pub use claude::ClaudeIntegration;
pub use codex::CodexIntegration;

#[derive(Debug, thiserror::Error)]
pub enum AgentErr {
    #[error("unknown agent integration `{0}`")]
    Unknown(String),
    #[error("cannot render decision for {agent}: {reason}")]
    Render { agent: &'static str, reason: String },
    #[error("missing required field `{field}` in {agent} decision")]
    MissingField {
        agent: &'static str,
        field: &'static str,
    },
    #[error("install failed for {agent}: {reason}")]
    Install { agent: &'static str, reason: String },
    #[error("io error installing {agent} hooks at {path}: {source}")]
    InstallIo {
        agent: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serializing {agent} hook config: {source}")]
    InstallSerialize {
        agent: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("parsing existing {agent} hook config at {path}: {source}")]
    InstallParse {
        agent: &'static str,
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("writing hook config: {0}")]
    WriteHookConfig(#[from] crate::ledger::atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, AgentErr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentHookClass {
    /// Non-blocking event that may carry a status/mode/task transition for
    /// the agent rollup (`SessionStart`, `UserPromptSubmit`, `Stop`, …). Per
    /// `docs/internals/agent.md` there are two runtime channels — lifecycle
    /// and feed. Whether a lifecycle event records anything is decided by
    /// [`AgentIntegration::observe_lifecycle`] returning `Some`.
    Lifecycle,
    BlockingFeed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedHook {
    pub class: AgentHookClass,
    pub feed_kind: Option<FeedKind>,
    pub event_name: String,
}

/// Status + mode transition observed from a lifecycle hook. Returned by
/// [`AgentIntegration::observe_lifecycle`] so the CLI layer can record an
/// `agent.lifecycle` event without each adapter touching the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentLifecycleObservation {
    /// Agent-supplied session/process identifier (e.g. Claude `session_id`,
    /// Codex root `session_id`, or Codex subagent `agent_id`). The CLI uses
    /// this as the `agent_id`.
    pub agent_id: Option<String>,
    pub status: AgentStatus,
    /// Process identity observed by the hook runner. The sidebar uses this
    /// best-effort liveness marker to suppress stale ledger overlays when the
    /// process disappears.
    pub agent_pid: Option<u32>,
    pub agent_process_start: Option<String>,
    pub runtime_owner: Option<RuntimeOwner>,
    /// Permission posture pill the event establishes. `None` means "this
    /// event does not report a posture" — the snapshot reducer carries the
    /// prior posture forward rather than resetting it, so a `UserPromptSubmit`
    /// can never demote a `yolo` agent to default (a security surface must
    /// stay visible).
    pub permission_posture: Option<PermissionPosture>,
    /// Optional absolute worktree path observed from the agent payload or
    /// filled by the CLI from the current Rimz workspace.
    pub worktree_path: Option<String>,
    /// Optional worktree branch label observed from the payload, surfaced in
    /// the sidebar's worktree grouping.
    pub worktree_branch: Option<String>,
    /// Display-only task descriptor. It never drives routing or decisions.
    pub task: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window utilization in percent reported by the agent (0..=100).
    /// Enrich-only / privacy-gated — the no-transcript-correctness rule.
    pub context_pct: Option<u8>,
    /// Cumulative token usage for this agent session.
    pub total_tokens: Option<u64>,
    /// Completed / total todos for the agent's current plan or task list.
    pub todo_done: Option<u32>,
    pub todo_total: Option<u32>,
    /// Normalized multiplexer pane id the agent process is running inside,
    /// read from the per-pane env var the mux exports (`TMUX_PANE` or
    /// `ZELLIJ_PANE_ID`). Lets the sidebar bind each agent row to its actual
    /// pane when two agents of the same kind share one worktree.
    pub pane_id: Option<PaneId>,
    /// The root session id this observation's agent is a *child* of, set only
    /// on `SubagentStart`/`SubagentStop` (the payload `session_id`, which both
    /// adapters report as the parent for a subagent event). `None` for root
    /// agents. Identity lifetime in the reducer, so a child row links to its
    /// parent row by `(kind, parent_agent_id)` for the whole child's life.
    pub parent_agent_id: Option<String>,
}

/// Result of installing hooks. Surfaced to the CLI so the user sees which
/// files were touched. Serialized verbatim as the `rimz hooks install` JSON
/// output — fields are part of the user-visible report contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookInstallReport {
    pub agent: &'static str,
    /// Absolute path of the config file the installer wrote.
    pub config_path: PathBuf,
    /// Event names installed (e.g. `SessionStart`, `PermissionRequest`).
    pub installed_events: Vec<String>,
    /// True when the installer wrote into an existing config (merge), false
    /// when the file was created fresh.
    pub merged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookInstallPreview {
    pub agent: &'static str,
    pub config_path: PathBuf,
    pub planned_events: Vec<String>,
    pub original_config: Option<String>,
    pub candidate_config: String,
    pub merged: bool,
    /// How the install changes the agent's statusline, for the one-line consent
    /// summary that keeps the wrap a visible security surface. The full change
    /// is also in `candidate_config`'s diff. `None` for agents that manage no
    /// statusline (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line_change: Option<StatusLineChange>,
}

/// What `rimz hooks install` does to the agent's statusline command, surfaced
/// in the consent gate alongside the hook diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StatusLineChange {
    /// No prior statusline; install adds Rimz's reader.
    Added,
    /// Wraps the user's existing statusline command, restored on uninstall.
    /// `original` is the user's command, shown verbatim in the summary.
    Wrapping { original: String },
    /// Re-install over an identical Rimz wrap — no change.
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookUninstallReport {
    pub agent: &'static str,
    pub config_path: PathBuf,
    pub removed_events: Vec<String>,
    /// True when the config file existed before uninstall.
    pub existed: bool,
}

pub trait AgentIntegration: Send + Sync {
    fn name(&self) -> &'static str;
    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook;
    /// Render the agent-native decision JSON for this resolution. Called
    /// only when the hook is on the bridge and a resolver has answered.
    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value>;
    /// Render the neutral stdout payload — the "agent's own UI is the answer"
    /// fallback path. `None` means the hook should print nothing on this event.
    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>>;
    /// Maximum time the hook may block on the bridge before falling back to
    /// the neutral payload. Defaults to [`DEFAULT_HOOK_CAP`].
    fn hook_cap(&self) -> Duration {
        DEFAULT_HOOK_CAP
    }

    /// Observe a lifecycle event payload and translate it into the
    /// status/mode transition the ledger should record. Returns `None` when
    /// the event is not a lifecycle event the adapter recognises.
    fn observe_lifecycle(
        &self,
        _event_name: &str,
        _payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        None
    }

    /// Sample the permission-posture slider from a hook payload. `None` when the
    /// payload names no slider field, so the snapshot carries the prior posture
    /// forward. The per-tool activity heartbeat calls this directly to keep the
    /// agent's sticky posture current between lifecycle events — a `PostToolUse`
    /// does not flow through [`Self::observe_lifecycle`], so it is the only
    /// channel that catches a mid-turn slider move (shift-tab out of `plan`).
    /// Defaults to `None`; adapters override with their slider mapping.
    fn posture_from_payload(&self, _payload: &Value) -> Option<PermissionPosture> {
        None
    }

    /// Translate a raw out-of-band context payload into the normalized
    /// [`AgentContext`]. The transport is the adapter's business: Claude parses
    /// the statusline JSON it is handed on stdin. Returns `None` when the
    /// adapter has no payload-driven rich-context source (Codex — it ingests
    /// out-of-band via the app-server, see [`codex::refresh_context`], not from
    /// a payload) or the payload is unusable. `source` is the ingest `--source`
    /// tag, stamped onto the record so downstream knows the provenance.
    /// Display-only enrichment — it never reaches the event log or a decision.
    fn observe_context(&self, _source: &str, _payload: &Value) -> Option<AgentContext> {
        None
    }

    /// Whether this event ends an agent session. When true the CLI expires the
    /// session's pending asks: a permission prompt whose agent has exited is
    /// no longer answerable, so it must not linger as attention. Defaults to
    /// `false`; adapters override for their session-exit event.
    fn ends_session(&self, _event_name: &str) -> bool {
        false
    }

    /// Whether this event means a still-live session moved on from any pending
    /// native_ui ask — a new prompt or the end of its turn. When true the CLI
    /// expires the session's pending native_ui asks: the agent answered (or
    /// dismissed) them in its own UI and never reports back, so they would
    /// otherwise pile up as duplicate attention. Bridge asks are untouched.
    /// Defaults to `false`; adapters override for their turn-boundary events.
    fn moves_on(&self, _event_name: &str) -> bool {
        false
    }

    /// Write or merge the adapter's hook config into the agent's per-user
    /// config file. Defaults to an explicit "not implemented" error until an
    /// adapter owns installation.
    fn install_hooks(&self) -> Result<HookInstallReport> {
        Err(AgentErr::Install {
            agent: self.name(),
            reason: "install not implemented for this adapter".to_owned(),
        })
    }

    /// Preview the exact per-user config write the installer would make,
    /// without touching disk. Used by the first-run consent gate.
    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        Err(AgentErr::Install {
            agent: self.name(),
            reason: "install preview not implemented for this adapter".to_owned(),
        })
    }

    /// Remove the adapter's hook entries from the agent's per-user config
    /// file. Defaults to an explicit "not implemented" error.
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        Err(AgentErr::Install {
            agent: self.name(),
            reason: "uninstall not implemented for this adapter".to_owned(),
        })
    }

    /// The user's original statusline command this agent currently wraps, if
    /// any. `None` when the agent manages no statusline (Codex), or when no
    /// wrap is configured. The `rimz statusline feed` CLI calls this to find
    /// its pass-through target. Best-effort: a read/parse failure reads as
    /// `None`.
    fn wrapped_status_line_command(&self) -> Option<String> {
        None
    }

    /// Whether Rimz can currently install a hook configuration that the agent
    /// actually executes. Adapters return `false` when the upstream hook
    /// contract is known but not implemented here yet.
    fn supports_hook_install(&self) -> bool {
        false
    }

    /// User-facing reason shown by doctor/start when hook install is not
    /// supported yet.
    fn hook_install_unavailable_reason(&self) -> Option<&'static str> {
        None
    }

    /// Whether this agent's per-user config currently carries Rimz-managed
    /// hooks — i.e. the user ran `rimz hooks install`. Best-effort: a missing
    /// file or any read/parse failure reads as "not installed". An agent only
    /// ever fires `rimz hooks feed` when this holds, so `rimz doctor` and the
    /// sidebar's first-run hint surface it — an un-wired agent is invisible,
    /// never silently broken.
    fn hooks_installed(&self) -> bool {
        false
    }
}

/// Every agent Rimz can wire, in display order. The single source of truth
/// for "which agents exist" — `integration_by_name` resolves each entry, and
/// `rimz doctor` walks this list to report hook-install status.
pub const KNOWN_AGENTS: &[&str] = &["claude", "codex"];

/// Lookup table for `--source <agent>` on the hook CLI.
pub fn integration_by_name(name: &str) -> Result<Box<dyn AgentIntegration>> {
    match name {
        "claude" => Ok(Box::new(ClaudeIntegration)),
        "codex" => Ok(Box::new(CodexIntegration)),
        other => Err(AgentErr::Unknown(other.to_owned())),
    }
}

/// Resolve an agent's per-user config file path. An explicit `override_env`
/// value wins (so tests and tooling can point at a tempdir); otherwise the path
/// is `$HOME` joined with `rel`. Returns an `Install` error naming the agent
/// when `$HOME` is unset.
pub(crate) fn agent_config_path(
    agent: &'static str,
    override_env: &str,
    rel: &Path,
) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(override_env).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AgentErr::Install {
            agent,
            reason: format!("$HOME is not set; cannot resolve ~/{}", rel.display()),
        })?;
    Ok(home.join(rel))
}

/// Read an agent config file's current contents for install preview and
/// uninstall. A missing file reads as `None`; any other IO error propagates
/// with agent + path context so the user sees which adapter failed and where.
pub(crate) fn read_optional_file(agent: &'static str, path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent,
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Read the trailing window of a transcript/rollout JSONL as lossy UTF-8, for
/// tail-scanning the most recent records newest-first. Returns `None` on any IO
/// error — context enrichment is best-effort, never correctness. A truncated
/// leading line from the seek simply fails to parse in the caller's walk.
pub(crate) fn read_transcript_tail(path: &Path) -> Option<String> {
    const TAIL_BYTES: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub(crate) fn optional_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Common helper: does the resolver decision read as an "allow"?
pub(crate) fn choice_is_allow(resolution: &Resolution) -> bool {
    resolution
        .decision
        .get("choice")
        .and_then(Value::as_str)
        .map(|v| matches!(v, "allow" | "yes" | "approve"))
        .unwrap_or(false)
}

/// The agent-native `PermissionRequest` decision envelope, shared by every
/// adapter whose permission hook speaks the `hookSpecificOutput.decision`
/// shape. `allow`/`deny` is projected from the resolver's choice.
pub(crate) fn permission_decision(resolution: &Resolution) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": if choice_is_allow(resolution) { "allow" } else { "deny" }
            }
        }
    })
}

/// Status a `Stop`-style turn-end event records. A `Stop` only fires after a
/// turn ran, so a clean end is `success`; an explicit error signal in the
/// payload demotes to `failed`. `idle` stays owned by `SessionStart` (wired in,
/// nothing asked) — it is never a `Stop` outcome.
pub(crate) fn stop_status_from_payload(payload: &Value) -> AgentStatus {
    let errored = payload
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload.get("error").is_some_and(|v| !v.is_null())
        || matches!(
            payload.get("status").and_then(Value::as_str),
            Some("error" | "failed" | "failure")
        )
        || matches!(
            payload.get("subtype").and_then(Value::as_str),
            Some("error" | "error_during_execution" | "error_max_turns")
        );
    if errored {
        AgentStatus::Failed
    } else {
        AgentStatus::Success
    }
}
