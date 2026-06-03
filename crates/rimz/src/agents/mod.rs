//! Agent integration interface.
//!
//! Each adapter classifies an incoming hook event, observes lifecycle
//! transitions (status + mode), renders the agent-native neutral no-op, and
//! (when a resolver answer is available) renders the agent-native decision
//! JSON. Adapters never touch the ledger directly;
//! they're called by `rimz hooks <agent>` which owns the ledger writes.
//!
//! Adapters also own hook install and uninstall — translating the trait
//! defaults into whatever per-agent config file the upstream agent reads.

pub mod account;
pub mod claude;
pub mod codex;
pub mod context;
pub(crate) mod hook_types;
pub mod lifecycle;
mod observation;
pub mod pricing;
pub mod spending;
pub mod transcript;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tracing::error;

use crate::feed::{FeedItem, FeedKind, PermissionPosture, Resolution};
use hook_types::PermissionMode;

pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits,
    AgentTokenUsage, RateLimitWindow,
};
pub use lifecycle::{LifecycleSignal, LifecycleState, Transition, TransitionKind, step};
pub use observation::AgentLifecycleObservation;
pub use pricing::{PriceBook, Pricing};
pub use spending::{ProviderKind, SpendTally, SpendWindow, Spending};

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
    /// `docs/internals/hooks.md` there are two runtime channels — lifecycle
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

pub(crate) fn classify_agent_hook(
    event_name: &str,
    feed_kind: Option<FeedKind>,
    lifecycle_events: &[&str],
) -> ClassifiedHook {
    let class = if feed_kind.is_some() {
        AgentHookClass::BlockingFeed
    } else if lifecycle_events.contains(&event_name) {
        AgentHookClass::Lifecycle
    } else {
        AgentHookClass::Unknown
    };
    ClassifiedHook {
        class,
        feed_kind,
        event_name: event_name.to_owned(),
    }
}

/// Sample an agent's permission mode field onto the unified posture enum. The
/// input is agent-reported mode only; Rimz never infers plan/thinking from
/// prompt text or transcripts.
pub(crate) fn permission_posture_from_payload(
    payload: &Value,
    keys: &[&str],
) -> Option<PermissionPosture> {
    let raw = keys
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str));
    raw.map(permission_posture_from_str)
}

fn permission_posture_from_str(raw: &str) -> PermissionPosture {
    match raw {
        "never" | "bypass" | "bypassPermissions" | "dontAsk" => PermissionPosture::Yolo,
        "acceptEdits" | "auto" | "auto-edit" | "on-failure" => PermissionPosture::Auto,
        "plan" => PermissionPosture::Plan,
        "default" | "interactive" | "untrusted" | "on-request" | "ask" => {
            PermissionPosture::Default
        }
        _ => PermissionPosture::Unknown,
    }
}

/// Map a typed permission slider onto the unified posture enum, falling back to
/// raw string parsing for an absent or unknown value. The typed path canonicalizes
/// every documented `permission_mode` value; the fallback covers an absent slider
/// and the per-agent alternate keys (`mode`, Codex's `approval_policy`) that the
/// typed `permission_mode` field never captures. Shared by both adapters so the
/// enum→posture mapping lives in one place.
pub(crate) fn posture_from_mode(
    mode: Option<&PermissionMode>,
    payload: &Value,
    fallback_keys: &[&str],
) -> Option<PermissionPosture> {
    mode.and_then(permission_mode_posture)
        .or_else(|| permission_posture_from_payload(payload, fallback_keys))
}

fn permission_mode_posture(mode: &PermissionMode) -> Option<PermissionPosture> {
    match mode {
        PermissionMode::DontAsk | PermissionMode::BypassPermissions => {
            Some(PermissionPosture::Yolo)
        }
        PermissionMode::Auto | PermissionMode::AcceptEdits => Some(PermissionPosture::Auto),
        PermissionMode::Plan => Some(PermissionPosture::Plan),
        PermissionMode::Default => Some(PermissionPosture::Default),
        PermissionMode::Unknown => None,
    }
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
    /// Render the neutral no-op — the "agent's own UI is the answer" fallback
    /// path. `None` means the hook should print nothing on this event.
    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>>;
    /// Maximum time the hook may block on the bridge before falling back to
    /// the neutral no-op. Defaults to [`DEFAULT_HOOK_CAP`].
    fn hook_cap(&self) -> Duration {
        DEFAULT_HOOK_CAP
    }

    /// Observe a lifecycle event payload and translate it into the
    /// [`LifecycleSignal`](lifecycle::LifecycleSignal) (plus posture sample and
    /// enrichment) the ledger should record. The adapter names the intent; the
    /// shared [`step`](lifecycle::step) table derives the status. Returns `None`
    /// when the event carries no transition the adapter recognises (a read-only
    /// tool, a quarantined subagent with no distinct child id).
    fn observe_lifecycle(
        &self,
        _event_name: &str,
        _payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
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

    /// Whether this agent's instances can be present without a stamped session —
    /// the agent registers its session lazily (no `SessionStart` at launch) and/or
    /// routes its hooks through a daemon (so the hook stamps no pane). For such an
    /// agent an instance exists before any session id binds, so the sidebar binds
    /// a session to its pane by cwd and renders a wired-but-unbound pane as an idle
    /// instance. Defaults to `false` — an agent that stamps a live pane on every
    /// session (Claude) is genuinely gone when its pane has no session, never idle-
    /// synthesized or cwd-rescued. Codex overrides; see
    /// [docs/internals/agent.md → The instance lifecycle](../../../docs/internals/agent.md).
    fn registers_session_lazily(&self) -> bool {
        false
    }

    /// The argv that resumes a prior session of this agent by `session_id`,
    /// launched fresh in `cwd` (the agent's worktree). The launcher seeds a
    /// reborn pane with it so a rebirth restores the conversation idle rather
    /// than coming up empty; the agent's own hooks re-fire on its
    /// `SessionStart` with `source: "resume"`, coalescing back onto the same
    /// `(kind, agent_id)` rollup row and re-stamping the new pane. `None` when
    /// the agent has no resume CLI, so [`crate::resume::plan_resume`] skips it.
    /// Default `None` keeps the contract "implement nothing else" for an agent
    /// that cannot resume yet.
    fn resume_command(&self, _session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        None
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

/// Whether `kind` registers its session lazily / routes hooks through a daemon, so
/// its instances can be present without a stamped session
/// ([`AgentIntegration::registers_session_lazily`]). The sidebar reducer reads this
/// to bind an unstamped session to its pane by cwd and to synthesize an idle row
/// for a wired-but-unbound pane. An unknown kind is not lazy.
pub(crate) fn registers_session_lazily(kind: &str) -> bool {
    integration_by_name(kind)
        .map(|agent| agent.registers_session_lazily())
        .unwrap_or(false)
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

/// Whether a `Stop`-style turn-end payload carries an explicit error signal. A
/// `Stop` only fires after a turn ran, so a clean end is a success and an error
/// signal demotes it to a failure — but that status decision now lives in the
/// lifecycle [`step`](lifecycle::step) table, so this helper reports only the
/// raw `errored` bit the adapter folds into [`LifecycleSignal::TurnEnded`].
pub(crate) fn stop_payload_errored(payload: &Value) -> bool {
    payload
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
        )
}

/// Tags the Claude/Codex harness injects as synthetic "user" turns — a
/// completed background task, a system reminder, a slash-command echo. Their
/// text is not user-authored, so it must never become an agent's description
/// line (the `<task-notification>…` leak). Presence of any of these rejects the
/// whole string.
const CONTROL_TAG_PREFIXES: &[&str] = &[
    "<task-notification>",
    "<system-reminder>",
    "<command-message>",
    "<command-name>",
    "<local-command-stdout>",
];

/// Sanitize a raw prompt/task string before it can label a sidebar row. Trims;
/// returns `None` for an empty string, or for any text carrying a harness
/// control tag (a synthetic, non-user-authored turn). KISS: a single substring
/// scan, no partial parsing — a control tag anywhere means the whole string is
/// rejected, so a raw `<task-notification>…` can never reach the description.
pub(crate) fn sanitize_user_prompt(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty())?;
    if CONTROL_TAG_PREFIXES.iter().any(|tag| trimmed.contains(tag)) {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Whether a tool-use payload names a tool that mutates the workspace — writes
/// files or runs commands. Drives [`LifecycleSignal::ToolUsed`]'s `mutates`
/// bit: a mutating tool is proof of real work, so its `PostToolUse` is the only
/// tool event worth recording on the lifecycle channel (read-only tools stay
/// silent), and it is what reconciles a stale `plan` posture. The tool name
/// rides `tool_name` in both providers' payloads.
pub(crate) fn tool_mutates(payload: &Value) -> bool {
    payload
        .get("tool_name")
        .and_then(Value::as_str)
        .is_some_and(|name| {
            matches!(
                name,
                // Claude
                "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Bash"
                // Codex
                | "shell" | "apply_patch" | "exec_command" | "local_shell"
            )
        })
}

/// The outcome of resolving a subagent event's identity.
pub(crate) enum SubagentIdentity {
    /// A usable child id distinct from its parent — the only case that yields a
    /// child entity.
    Resolved {
        agent_id: String,
        parent_agent_id: String,
    },
    /// Unusable identity (missing child or parent id, or child == parent). The
    /// caller emits no observation, so a malformed subagent event can never
    /// fold onto — and corrupt — its parent's row.
    Quarantined,
}

/// Resolve a subagent event's identity, requiring a non-empty child id, a
/// non-empty parent id, and `child != parent`. This is the one place the rule
/// lives, shared by both adapters; it replaces the unsafe per-adapter
/// `child_id.or_else(|| parent_id)` fallback that silently keyed a child onto
/// its parent. A quarantined identity is logged once with the raw payload so
/// the anomaly is traceable.
pub(crate) fn resolve_subagent_identity(
    kind: &str,
    event_name: &str,
    child_id: Option<&str>,
    parent_id: Option<&str>,
    payload: &Value,
) -> SubagentIdentity {
    let child = child_id.map(str::trim).filter(|value| !value.is_empty());
    let parent = parent_id.map(str::trim).filter(|value| !value.is_empty());
    match (child, parent) {
        (Some(child), Some(parent)) if child != parent => SubagentIdentity::Resolved {
            agent_id: child.to_owned(),
            parent_agent_id: parent.to_owned(),
        },
        _ => {
            error!(
                target: "rimz::agent::lifecycle",
                kind,
                event = event_name,
                child_id = child.unwrap_or(""),
                parent_id = parent.unwrap_or(""),
                payload = %payload,
                "subagent identity unusable — quarantined (need a distinct child and parent id)",
            );
            SubagentIdentity::Quarantined
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_rejects_each_control_tag() {
        for tag in CONTROL_TAG_PREFIXES {
            let injected = format!("{tag}<task-id>afdc639e18e7ebdb9</...");
            assert_eq!(sanitize_user_prompt(Some(&injected)), None, "tag {tag}");
        }
    }

    #[test]
    fn sanitize_rejects_embedded_control_tag() {
        assert_eq!(
            sanitize_user_prompt(Some("please fix <system-reminder>noise</system-reminder>")),
            None,
        );
    }

    #[test]
    fn sanitize_passes_a_real_prompt_trimmed() {
        assert_eq!(
            sanitize_user_prompt(Some("  add a dark mode toggle  ")),
            Some("add a dark mode toggle".to_owned()),
        );
    }

    #[test]
    fn sanitize_rejects_empty_and_whitespace() {
        assert_eq!(sanitize_user_prompt(None), None);
        assert_eq!(sanitize_user_prompt(Some("   ")), None);
    }

    #[test]
    fn tool_mutates_reads_the_tool_name() {
        assert!(tool_mutates(&json!({ "tool_name": "Edit" })));
        assert!(tool_mutates(&json!({ "tool_name": "Bash" })));
        assert!(tool_mutates(&json!({ "tool_name": "apply_patch" })));
        assert!(!tool_mutates(&json!({ "tool_name": "Read" })));
        assert!(!tool_mutates(&json!({ "tool_name": "Grep" })));
        assert!(!tool_mutates(&json!({})));
    }

    #[test]
    fn subagent_identity_resolves_distinct_child_and_parent() {
        match resolve_subagent_identity(
            "claude",
            "SubagentStart",
            Some("child"),
            Some("root"),
            &json!({}),
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => {
                assert_eq!(agent_id, "child");
                assert_eq!(parent_agent_id, "root");
            }
            SubagentIdentity::Quarantined => panic!("expected resolved"),
        }
    }

    #[test]
    fn subagent_identity_quarantines_missing_or_equal_ids() {
        let cases = [
            (None, Some("root")),
            (Some("child"), None),
            (Some("same"), Some("same")),
            (Some("  "), Some("root")),
        ];
        for (child, parent) in cases {
            assert!(
                matches!(
                    resolve_subagent_identity("claude", "SubagentStart", child, parent, &json!({})),
                    SubagentIdentity::Quarantined
                ),
                "child={child:?} parent={parent:?}",
            );
        }
    }
}
