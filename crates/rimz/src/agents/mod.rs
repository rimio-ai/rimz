//! Agent adapter interface.
//!
//! Each adapter classifies an incoming hook event, observes lifecycle
//! transitions, renders the agent-native neutral no-op, and
//! (when a resolver answer is available) renders the agent-native decision
//! JSON. Adapters never touch the ledger directly;
//! they're called by `rimz hooks <agent>` which owns the ledger writes.
//!
//! Adapters also own hook install and uninstall — translating the trait
//! defaults into whatever per-agent config file the upstream agent reads.
//!
//! Static per-agent data — identity, branding, capabilities, tool tables —
//! lives in each adapter's [`AgentDescriptor`]; [`registry::ADAPTERS`] is the
//! single registration table every dispatch site resolves through.

pub mod account;
pub mod claude;
pub mod codex;
#[cfg(test)]
pub(crate) mod conformance;
pub mod context;
pub mod credits;
pub mod descriptor;
pub(crate) mod hook_types;
pub mod lifecycle;
mod observation;
pub mod opencode;
pub mod pi;
pub mod pricing;
pub mod registry;
pub mod spending;
#[cfg(test)]
pub(crate) mod testkit;
pub(crate) mod transcript_fs;
pub mod version;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, error};

use crate::feed::{FeedItem, FeedKind, Resolution};
use crate::ids::AgentSessionId;
use crate::run::PermissionMode;

pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits,
    AgentTokenUsage, AgentTurnError, RateLimitWindow, SubagentContext, SubagentObservation,
    TurnErrorClass,
};
pub use credits::{AccountUsageSnapshot, ExtraCredits, OauthUsageProbe};
pub use descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, IntegrationConcern, PlanLabel,
    RemoteControlCapability, ThreadKey, ToolClassification,
};
pub use lifecycle::{LifecycleSignal, LifecycleState, Transition, TransitionKind, TurnPhase, step};
pub use observation::AgentLifecycleObservation;
pub use pricing::{PriceBook, Pricing};
pub use registry::{ADAPTERS, adapter_by_kind, descriptor_by_kind, find_adapter, known_kinds};
pub use spending::{SpendTally, SpendWindow, Spending};

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPreset {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Absolute path to a file whose contents replace the agent's base system
    /// prompt. Resolved and existence-checked by the launcher before render.
    pub system_prompt_file: Option<PathBuf>,
}

impl LaunchPreset {
    pub fn is_empty(&self) -> bool {
        self.model.as_deref().is_none_or(str::is_empty)
            && self.effort.as_deref().is_none_or(str::is_empty)
            && self.system_prompt_file.is_none()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PresetErr {
    #[error(
        "agent `{agent}` does not support profile field `{field}`; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedField {
        agent: &'static str,
        field: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentHookClass {
    /// Non-blocking event that may carry a status/mode/task transition for
    /// the agent rollup (`SessionStart`, `UserPromptSubmit`, `Stop`, …). Per
    /// `docs/internals/agents/agent.md` there are two runtime channels — lifecycle
    /// and feed. Whether a lifecycle event records anything is decided by
    /// [`AgentAdapter::observe_lifecycle`] returning `Some`.
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

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct ClassificationSample {
    pub event_name: &'static str,
    pub payload: Value,
    pub expected: ClassifiedHook,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct SpendFixture {
    pub session_id: &'static str,
    pub file_name: &'static str,
    pub body: SpendFixtureBody,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub enum SpendFixtureBody {
    Jsonl(&'static str),
    OpencodeSqlite { data: &'static str },
}

#[cfg(test)]
impl ClassificationSample {
    pub(crate) fn new(
        event_name: &'static str,
        payload: Value,
        class: AgentHookClass,
        feed_kind: Option<FeedKind>,
    ) -> Self {
        Self {
            event_name,
            payload,
            expected: ClassifiedHook {
                class,
                feed_kind,
                event_name: event_name.to_owned(),
            },
        }
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
    /// How the install changes the agent's `subagentStatusLine` (the per-child
    /// render command), same consent-surface discipline as `status_line_change`.
    /// `None` for agents that manage no subagent statusline (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_status_line_change: Option<StatusLineChange>,
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

/// Context for [`AgentAdapter::post_lifecycle_refresh`]: the session and
/// workspace a lifecycle event just landed for, plus the model hint its
/// observation resolved.
pub struct LifecycleRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub workspace_id: &'a str,
    pub model_hint: Option<&'a str>,
}

/// File identity for a bounded transcript/rollout tail read. Producers persist it
/// beside the sidecar so a high-frequency hook can stat-gate local enrichment
/// before reading the tail again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptStat {
    pub mtime_secs: i64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub mtime_nanos: u32,
    pub len: u64,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Context for [`AgentAdapter::local_context_refresh`]: the session that just
/// proved progress, its current model hint, and the transcript gate state from
/// the latest sidecar.
pub struct LocalContextRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub model_hint: Option<&'a str>,
    pub prior_effort: Option<&'a str>,
    pub prior_transcript_path: Option<&'a str>,
    pub prior_transcript_stat: Option<&'a TranscriptStat>,
}

/// Display-only context derived from a local transcript/rollout read. The
/// adapter owns the provider mapping; the CLI owns merging and writing the
/// sidecar.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalContextRefresh {
    pub model_id: Option<String>,
    pub effort: Option<String>,
    pub tokens: Option<AgentTokenUsage>,
    pub cost: Option<AgentCost>,
    /// Timestamp of a cleanly-completed turn read from the rollout tail
    /// (`detect_turn_complete`), set when the session is at rest on a
    /// `task_complete` that fired no `Stop` hook (a `/review` turn). The
    /// projection reads it to settle a falsely-`running` row to `success`.
    pub turn_complete: Option<Timestamp>,
    pub transcript_path: Option<String>,
    pub transcript_stat: Option<TranscriptStat>,
}

/// A detached `rimz` helper an adapter requests after a lifecycle event lands
/// — just the argv. The CLI owns the spawn discipline (fresh, fully-nulled
/// stdio; fire-and-forget), so adapters stay pure mappers.
pub struct RefreshSpawn {
    /// Arguments to the `rimz` binary itself.
    pub args: Vec<String>,
}

/// Dynamic remote-control state read from the agent's own machine-local
/// settings and the already-published account cache. Static capability still
/// lives in [`AgentDescriptor`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteControlStatus {
    /// Existing pane sessions auto-enable their own remote-control surface.
    pub pane_auto: bool,
}

pub trait AgentAdapter: Send + Sync {
    /// The adapter's static identity, branding, capabilities, and
    /// classification tables. Everything `const` about an agent lives here;
    /// the trait methods own everything behavioral.
    fn descriptor(&self) -> &'static AgentDescriptor;

    /// Display model to use before a lazy-registering agent reports a real
    /// session model. Defaults to the descriptor's provider fallback; adapters
    /// with user-configured launch defaults override it.
    fn default_launch_model(&self) -> Option<String> {
        self.descriptor().default_model.map(ToOwned::to_owned)
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook;

    /// Test-only native payload corpus for registry-wide adapter conformance.
    /// Keeping it on the adapter avoids a parallel per-agent registry. The
    /// corpus must cover every event name in
    /// [`installed_hook_events`](Self::installed_hook_events), and may include
    /// multiple payload variants for broad hooks such as `PreToolUse`.
    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<ClassificationSample> {
        Vec::new()
    }

    /// Test-only list of native event names the installer wires for this
    /// adapter. Conformance uses it to prove the hand-authored corpus covers the
    /// installed surface instead of only proving the samples it happens to name.
    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Test-only representative transcript/store fixture that must produce a
    /// positive session cost through the same spend parser the live-card fallback
    /// uses. Adapters with declared realtime-cost coverage provide one so the
    /// registry-wide conformance suite can prove the claim is backed by behavior.
    #[cfg(test)]
    fn spend_fixture(&self) -> Option<SpendFixture> {
        None
    }

    /// Render the agent-native decision JSON for this resolution. Called
    /// only when the hook is on the bridge and a resolver has answered.
    fn render_decision(&self, item: &FeedItem, resolution: &Resolution) -> Result<Value>;
    /// Render the neutral no-op — the "agent's own UI is the answer" fallback
    /// path. `None` means the hook should print nothing on this event.
    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>>;

    /// Observe a lifecycle event payload and translate it into the
    /// [`LifecycleSignal`](lifecycle::LifecycleSignal) (plus enrichment) the
    /// ledger should record. The adapter names the intent; the
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

    /// Detect a provider turn-death marker the adapter can recover from local
    /// transcript/rollout evidence. Claude uses it for API-error aborts that
    /// emit no `Stop`; Codex uses the same marker on `Stop` when the native
    /// payload itself looks clean. Returns `None` when the newest turn is alive
    /// or recovered, the transcript is unreadable, or the adapter has no local
    /// marker shape. The marker itself is display-only enrichment: it rides the
    /// context sidecar and refines the displayed row. An adapter may also use the
    /// same evidence inside [`observe_lifecycle`](Self::observe_lifecycle) to set
    /// a lifecycle error bit when the native turn-end payload lacks one.
    fn observe_turn_error(&self, _payload: &Value) -> Option<AgentTurnError> {
        None
    }

    /// Detect a turn-error marker directly from a hook payload that carries the
    /// provider's native failure certificate. This is the precise sibling of
    /// [`observe_turn_error`](Self::observe_turn_error), which recovers the same
    /// shape from local transcripts when the hook was absent or installed late.
    /// The adapter returns the display-only marker; the CLI owns merging it
    /// into the context sidecar.
    fn observe_turn_error_from_hook(
        &self,
        _event_name: &str,
        _payload: &Value,
    ) -> Option<AgentTurnError> {
        None
    }

    /// Extract the user-facing final assistant text for a completed supervised
    /// `rimz agents -p`. The adapter owns native payload and transcript shapes; the
    /// run store receives only this normalized string.
    fn last_assistant_message(
        &self,
        _event_name: &str,
        _payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        None
    }

    /// Extract newly appended main-thread assistant messages from transcript
    /// JSONL text. The CLI owns the cursor and output transport; adapters own
    /// their native transcript event shapes. Defaults to no stream surface.
    fn stream_assistant_messages(&self, _new_lines: &str) -> Vec<String> {
        Vec::new()
    }

    /// Harvest per-subagent enrichment from an out-of-band render payload —
    /// Claude's `subagentStatusLine` tasks today. One payload renders many child
    /// rows, so this returns one [`SubagentObservation`] per attributable task
    /// (every task carrying an `agent_id`). Empty when the adapter has no such
    /// transport (Codex) or the payload is unusable. Display-only enrichment,
    /// like [`observe_context`](Self::observe_context) — it never reaches the
    /// event log or a decision.
    fn observe_subagent_context(&self, _payload: &Value) -> Vec<SubagentObservation> {
        Vec::new()
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

    /// A detached `rimz` helper to spawn after this lifecycle event is
    /// recorded — the out-of-band enrichment lane (Codex refreshes its
    /// app-server context on turn boundaries). The CLI spawns it with fresh,
    /// fully-nulled stdio and never waits, so it adds no latency to the
    /// agent's turn. Display-only enrichment, never correctness. Defaults to
    /// `None` for an agent with no out-of-band refresh.
    fn post_lifecycle_refresh(
        &self,
        _event_name: &str,
        _ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        None
    }

    /// A cheap, synchronous local enrichment read to run inline after a
    /// progress-proving hook event. This is for bounded file reads that are
    /// lighter than the ledger write already performed by the hook; network,
    /// subprocess, broker, or app-server work belongs in
    /// [`post_lifecycle_refresh`](Self::post_lifecycle_refresh). The adapter
    /// returns mapped fields only and never writes the sidecar itself.
    fn local_context_refresh(
        &self,
        _event_name: &str,
        _ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        None
    }

    /// Probe this provider's account/plan login out-of-band — a `claude auth
    /// status` fork, an auth-file read. Producer-only and best-effort: the
    /// elected sidebar producer single-flights it behind a TTL'd cache, so it
    /// never runs on the per-tick hot path (see [`account`]). Defaults to
    /// [`account::AccountProbe::LoggedOut`] for an agent with no out-of-band
    /// login surface.
    fn probe_account(&self) -> account::AccountProbe {
        account::AccountProbe::LoggedOut
    }

    /// Query this provider's account usage (included rate-limit windows + paid
    /// extra credits) directly from its own local OAuth credentials. This is the
    /// uniform *API-query channel*: every adapter reads its own auth file with
    /// its own token and normalizes the provider's quota surface into an
    /// [`AccountUsageSnapshot`]. Producer-only and best-effort — the shared
    /// refresh driver single-flights it behind the credits cache and keys the
    /// cache TTL on the returned arm. Defaults to
    /// [`OauthUsageProbe::Unsupported`] for an agent with no OAuth usage surface.
    fn probe_oauth_usage(&self) -> OauthUsageProbe {
        OauthUsageProbe::Unsupported
    }

    /// Dynamic remote-control state from this provider's own settings and
    /// account facts.
    /// Best-effort and read-only: failures return the default "off/unknown"
    /// state. The sidebar uses this only to light a capability-gated flag.
    fn remote_control_status(&self, _account: Option<&AgentAccount>) -> RemoteControlStatus {
        RemoteControlStatus::default()
    }

    /// Whether this adapter exposes a cheap out-of-band binary version probe.
    /// The sidebar producer uses this during account refresh to fill a
    /// display-only provider header when no live context has a fresher version.
    fn probes_version(&self) -> bool {
        true
    }

    /// Probe the agent binary's version out-of-band. Producer-only and
    /// display-only: a failure leaves the provider header without a version,
    /// never affecting account truth, cache freshness, or ledger correctness.
    fn probe_version(&self) -> Option<String> {
        probe_descriptor_version(self.descriptor())
    }

    /// Every transcript/rollout JSONL this agent has on disk, fleet-wide — the
    /// discovery walk for the full-history spending pass
    /// ([`spending::compute_spending`]). Distinct from the bounded tail read in
    /// [`observe_lifecycle`](Self::observe_lifecycle): this walks the whole
    /// history for spend. Defaults to none for an agent with no transcript
    /// surface.
    fn transcript_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Resolve the local transcript/store that carries a live session's spend.
    /// `prior_path` is the path already published in the context sidecar, so a
    /// steady session pays one stat before falling back to provider discovery.
    /// Providers with one-file-per-session stores usually need no override; stores
    /// whose file name does not contain the session id provide their own mapping.
    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| path.is_file()) {
            return Some(path.to_path_buf());
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        self.transcript_files().into_iter().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id))
        })
    }

    /// Parse one transcript file into cost entries for the spending pass,
    /// resuming from `resume` when given: read only past its offset, restore
    /// any cross-line state it carries, and return entries the cache appends
    /// to the file's set. `None` parses the whole file cold. An adapter whose
    /// transcripts log dollars reads them verbatim and ignores `prices`; a
    /// token-only adapter (Codex) multiplies its counts through the book.
    /// Read-only and sidebar-safe — spend parsing never writes the ledger or
    /// blocks on a socket (CI grep on the adapter `spend.rs` files).
    fn parse_spend(
        &self,
        _path: &Path,
        _resume: Option<&spending::SpendCursor>,
        _prices: &PriceBook,
    ) -> spending::SpendParse {
        spending::SpendParse::default()
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

    /// Extra launch argv for a supervised agent permission posture. The
    /// adapter owns provider-specific CLI flags; the CLI only chooses
    /// the posture.
    fn permission_args(&self, _mode: PermissionMode) -> Vec<String> {
        Vec::new()
    }

    /// Extra launch argv for the built-in `-ping` virtual profile: lowest
    /// effort setting plus the word `"ping"` as the initial prompt. Returns
    /// `None` when the adapter does not support ping.
    fn ping_args(&self) -> Option<Vec<String>> {
        None
    }

    /// The interactive slash command that triggers a manual context compaction
    /// in the agent's own composer. Typed as keystrokes ahead of a steered or
    /// queued message under `--auto-compact`, never a bracketed paste — agents
    /// treat pasted text as literal content and would not run a pasted command.
    /// `None` when the agent exposes no such command.
    fn compact_command(&self) -> Option<&'static str> {
        None
    }

    /// Render typed launch profile presets into provider-native argv.
    /// Unsupported fields fail at launch so config cannot silently drop intent.
    fn render_preset(&self, preset: &LaunchPreset) -> std::result::Result<Vec<String>, PresetErr> {
        if preset
            .model
            .as_deref()
            .is_some_and(|model| !model.is_empty())
        {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "model",
            });
        }
        if preset
            .effort
            .as_deref()
            .is_some_and(|effort| !effort.is_empty())
        {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "effort",
            });
        }
        if preset.system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "system-prompt-file",
            });
        }
        Ok(Vec::new())
    }

    /// The argv that launches a fresh interactive session of this agent in the
    /// pane's cwd. `extra_args` are direct agent CLI arguments from the chosen
    /// tab layout; `prompt`, when present, is passed as the agent's positional
    /// startup prompt after them. An agent with no launch CLI returns `None`.
    fn launch_command(&self, _extra_args: &[String], _prompt: Option<&str>) -> Option<Vec<String>> {
        None
    }

    /// Env vars pinned onto every spawn of this agent — the launch contract
    /// the integration depends on. Applied last at spawn, over any configured
    /// env, so configuration cannot switch the agent into a mode the
    /// integration cannot drive.
    fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    /// Write or merge the adapter's hook config into the agent's per-user
    /// config file. Defaults to an explicit "not implemented" error until an
    /// adapter owns installation.
    fn install_hooks(&self) -> Result<HookInstallReport> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: "install not implemented for this adapter".to_owned(),
        })
    }

    /// Preview the exact per-user config write the installer would make,
    /// without touching disk. Used by the first-run consent gate.
    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: "install preview not implemented for this adapter".to_owned(),
        })
    }

    /// Remove the adapter's hook entries from the agent's per-user config
    /// file. Defaults to an explicit "not implemented" error.
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: "uninstall not implemented for this adapter".to_owned(),
        })
    }

    /// Whether the user's config carries any Rimz-managed hook artifact, including
    /// partial or legacy installs that are not complete enough to be considered
    /// usable by [`Self::hooks_installed`]. No-arg uninstall uses this so
    /// "ensure absent" cleans damaged configs without rewriting untouched ones.
    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }

    /// The user's original statusline command this agent currently wraps, if
    /// any. `None` when the agent manages no statusline (Codex), or when no
    /// wrap is configured. The `rimz statusline feed` CLI calls this to find
    /// its pass-through target. Best-effort: a read/parse failure reads as
    /// `None`.
    fn wrapped_status_line_command(&self) -> Option<String> {
        None
    }

    /// The user's original `subagentStatusLine` command this agent currently
    /// wraps, if any — the pass-through target for `rimz statusline feed
    /// --subagent`. `None` when the agent manages no subagent statusline (Codex)
    /// or no wrap is configured. Best-effort: a read/parse failure reads as
    /// `None`.
    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        None
    }

    /// Whether this agent's per-user config currently carries Rimz-managed
    /// hooks — i.e. the user ran `rimz hooks install`. Best-effort: a missing
    /// file or any read/parse failure reads as "not installed". An agent only
    /// ever fires `rimz hooks feed` when this holds, so `rimz doctor` surfaces
    /// it — an un-wired agent is invisible, never silently broken.
    fn hooks_installed(&self) -> bool {
        false
    }

    /// Rimz-installed hook events this agent will silently skip until the
    /// user trusts them in the agent's own UI. Empty for agents without a
    /// trust gate; Codex overrides it from `[hooks.state]` in its config.
    /// Rimz cannot trust on the user's behalf, so `rimz start` and
    /// `rimz doctor` surface the fix ([`hook_trust_fix`]) instead.
    fn untrusted_installed_hooks(&self) -> Vec<String> {
        Vec::new()
    }
}

/// One-line fix for an installed-but-untrusted hook set, shared by the
/// `rimz start` notice and `rimz doctor`.
pub fn hook_trust_fix(kind: &str) -> String {
    format!("run /hooks inside {kind} and trust the Rimz hooks")
}

fn probe_descriptor_version(descriptor: &AgentDescriptor) -> Option<String> {
    probe_descriptor_version_with_locator(descriptor, locate_binary)
}

fn probe_descriptor_version_with_locator(
    descriptor: &AgentDescriptor,
    locate: impl FnOnce(&AgentDescriptor) -> Option<PathBuf>,
) -> Option<String> {
    let binary = locate(descriptor)?;
    version::probe_cli_version(binary)
}

/// Resolve an agent's binary on this machine: `$PATH` first, then the
/// descriptor's [`extra_bin_dirs`](AgentDescriptor::extra_bin_dirs) joined under
/// `$HOME`. An installer that drops its binary in a private dir (OpenCode's
/// `~/.opencode/bin`) and edits a shell rc the running environment never sourced
/// leaves the agent off `$PATH` yet present; this finds it. Returns the absolute
/// path, or `None` when the binary is nowhere Rimz knows to look.
pub fn locate_binary(descriptor: &AgentDescriptor) -> Option<PathBuf> {
    if let Ok(path) = which::which(descriptor.kind) {
        return Some(path);
    }
    let home = PathBuf::from(std::env::var_os("HOME").filter(|value| !value.is_empty())?);
    binary_in_install_dirs(descriptor, &home)
}

/// The `$PATH`-miss branch of [`locate_binary`], split out so it tests without
/// touching process env: the first existing `<home>/<dir>/<kind>` file across
/// the descriptor's [`extra_bin_dirs`](AgentDescriptor::extra_bin_dirs).
fn binary_in_install_dirs(descriptor: &AgentDescriptor, home: &Path) -> Option<PathBuf> {
    descriptor.extra_bin_dirs.iter().find_map(|dir| {
        let candidate = home.join(dir).join(descriptor.kind);
        candidate.is_file().then_some(candidate)
    })
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

/// Read a torn-write-safe JSONL suffix from a transcript path, returning the
/// consumed bytes and next cursor offset. Same cursor discipline as spending,
/// exposed for `rimz agents wait --stream` without making the helper module public.
pub fn read_transcript_lines(path: &Path, offset: u64) -> Option<(Vec<u8>, u64)> {
    transcript_fs::read_spend_lines(path, offset)
}

pub(crate) fn optional_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn non_empty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Why an OAuth usage HTTP probe failed, carried structured (so a status code is
/// a Sentry facet) without the request URL's path or query.
#[derive(Debug, Clone, Copy)]
pub(crate) enum HttpErrKind {
    /// A response with this non-200 status code.
    Status(u16),
    /// The request never completed (DNS, connect, TLS, or timeout).
    Transport,
    /// The response arrived but its body could not be read.
    Body,
}

impl std::fmt::Display for HttpErrKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code) => write!(f, "status {code}"),
            Self::Transport => f.write_str("transport"),
            Self::Body => f.write_str("body"),
        }
    }
}

/// The host authority of `url` — scheme stripped, path/query/fragment and any
/// `userinfo@` removed — the only part of a request URL safe to attach to an
/// off-box error. Std-only; never pulls in a URL parser.
pub(crate) fn url_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
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

/// The outcome of resolving a subagent event's identity.
pub(crate) enum SubagentIdentity {
    /// A usable child id distinct from its parent — the only case that yields a
    /// child entity.
    Resolved {
        agent_id: AgentSessionId,
        parent_agent_id: AgentSessionId,
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
            agent_id: AgentSessionId::from(child),
            parent_agent_id: AgentSessionId::from(parent),
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

/// The outcome of resolving a non-subagent (root-arm) event's identity.
pub(crate) enum RootIdentity {
    /// A normal root event: key on the session id, no parent link.
    Root { agent_id: Option<AgentSessionId> },
    /// The event is stamped with a distinct child `agent_id` — it fired inside
    /// a subagent. The caller drops it: the lifecycle channel is bracket-grained
    /// for children (only `Subagent*` folds to the child's rollup), per-tool
    /// child activity rides the child-keyed heartbeat, and folding the event
    /// onto the parent would advance the parent's `last_activity` past a
    /// pending ask — un-folding its `waiting` row while it is still blocked.
    ForeignChild,
}

/// Resolve a non-subagent event's identity. A payload whose `agent_id` is
/// present, non-empty, and distinct from its `session_id` fired inside a
/// subagent and is the child's, never the root's — the one place the rule
/// lives, shared by the adapters whose providers stamp `agent_id` on every
/// in-subagent payload. A missing or session-equal `agent_id` is a normal root;
/// quarantine stays `Subagent*`-only.
pub(crate) fn resolve_root_identity(
    kind: &str,
    event_name: &str,
    agent_id: Option<&str>,
    session_id: Option<&str>,
) -> RootIdentity {
    let agent = agent_id.map(str::trim).filter(|value| !value.is_empty());
    let session = session_id.map(str::trim).filter(|value| !value.is_empty());
    match (agent, session) {
        (Some(agent), session) if session != Some(agent) => {
            debug!(
                target: "rimz::agent::lifecycle",
                kind,
                event = event_name,
                agent_id = agent,
                session_id = session.unwrap_or(""),
                "foreign-child lifecycle event dropped (rides the child-keyed heartbeat)",
            );
            RootIdentity::ForeignChild
        }
        _ => RootIdentity::Root {
            agent_id: session.map(AgentSessionId::from),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_user_prompt_accepts_real_text_and_rejects_control_payloads() {
        for tag in CONTROL_TAG_PREFIXES {
            let injected = format!("{tag}<task-id>afdc639e18e7ebdb9</...");
            assert_eq!(sanitize_user_prompt(Some(&injected)), None, "tag {tag}");
        }
        assert_eq!(
            sanitize_user_prompt(Some("please fix <system-reminder>noise</system-reminder>")),
            None,
        );
        assert_eq!(
            sanitize_user_prompt(Some("  add a dark mode toggle  ")),
            Some("add a dark mode toggle".to_owned()),
        );
        assert_eq!(sanitize_user_prompt(None), None);
        assert_eq!(sanitize_user_prompt(Some("   ")), None);
    }

    #[test]
    fn binary_resolves_from_a_known_install_dir_off_path() {
        let home = tempfile::tempdir().unwrap();
        let opencode = descriptor_by_kind("opencode").unwrap();
        // Off PATH and not yet installed: nowhere under HOME to find it.
        assert_eq!(binary_in_install_dirs(opencode, home.path()), None);
        // OpenCode's installer drops the binary here without editing PATH.
        let bin_dir = home.path().join(".opencode/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("opencode");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        assert_eq!(binary_in_install_dirs(opencode, home.path()), Some(bin));
        // An agent declaring no install dirs is never found this way.
        let claude = descriptor_by_kind("claude").unwrap();
        assert_eq!(binary_in_install_dirs(claude, home.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_uses_the_located_install_dir_binary() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let opencode = descriptor_by_kind("opencode").unwrap();
        let bin_dir = home.path().join(".opencode/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("opencode");
        std::fs::write(&bin, b"#!/bin/sh\nprintf 'opencode 1.17.7\\n'\n").unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        let version = probe_descriptor_version_with_locator(opencode, |descriptor| {
            binary_in_install_dirs(descriptor, home.path())
        });

        assert_eq!(version.as_deref(), Some("1.17.7"));
    }

    #[test]
    fn subagent_identity_needs_a_distinct_child_and_parent() {
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
        // A missing child or parent, equal ids, or a blank child all quarantine —
        // a malformed subagent event can never fold onto its parent's row.
        for (child, parent) in [
            (None, Some("root")),
            (Some("child"), None),
            (Some("same"), Some("same")),
            (Some("  "), Some("root")),
        ] {
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
