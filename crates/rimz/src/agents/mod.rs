//! Agent adapter interface.
//!
//! Each adapter classifies an incoming hook event, observes lifecycle
//! transitions, and renders the agent-native neutral no-op. Adapters never
//! touch the store directly;
//! they're called by `rimz hooks <agent>` which owns the store writes.
//!
//! Adapters also own hook install and uninstall — translating the trait
//! defaults into whatever per-agent config file the upstream agent reads.
//!
//! Per-agent data — identity, branding, capabilities, tool tables — lives in
//! each adapter's [`AgentDescriptor`]; [`registry::all_adapters`] chains the
//! built-in [`registry::ADAPTERS`] table with validated process plugins.

pub mod account;
pub mod amp;
pub mod antigravity;
pub mod claude;
pub mod codex;
#[cfg(test)]
pub(crate) mod conformance;
pub mod context;
pub mod copilot;
pub mod credits;
pub mod cursor;
pub mod descriptor;
pub mod droid;
mod emblems;
pub(crate) mod hook_types;
pub(crate) mod identity;
pub(crate) mod jsonc;
pub mod kimi;
pub mod kiro;
pub mod lifecycle;
pub(crate) mod locate;
pub(crate) mod managed_source;
pub mod model_display;
mod observation;
pub mod opencode;
pub(crate) mod payload;
pub mod pi;
pub mod plugin;
pub mod pricing;
pub mod qwen;
pub mod registry;
pub mod spending;
pub mod state;
#[cfg(test)]
pub(crate) mod testkit;
pub mod transcript;
pub(crate) mod transcript_fs;
pub mod turns;
pub mod version;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::run::PermissionMode;
use crate::mux::NamedKey;
use crate::transcript::{AskAnswer, AskOption, AskQuestion};

pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits,
    AgentTokenUsage, AgentTurnError, RateLimitWindow, SubagentContext, SubagentObservation,
    TurnErrorClass,
};
pub(crate) use credits::HttpErrKind;
pub use credits::{AccountUsageSnapshot, ExtraCredits, OauthUsageProbe, ResetCredits};
pub use descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
    program_names_kind,
};
pub use emblems::{emblem_lines, fallback_emblem};
pub(crate) use identity::{
    RootIdentity, SubagentIdentity, resolve_root_identity, resolve_subagent_identity,
};
pub use lifecycle::{
    AskKind, LifecycleSignal, LifecycleSignalKind, LifecycleState, Transition, TransitionKind,
    TurnPhase, step,
};
pub use locate::locate_binary;
pub(crate) use locate::{agent_config_path, probe_descriptor_version, read_optional_file};
pub use observation::{AgentLifecycleObservation, LaunchParams, SessionOrigin};
pub(crate) use payload::{
    CONTROL_TAG_PREFIXES, classify_agent_hook, non_empty_trimmed, optional_payload_string,
    sanitize_user_prompt, stop_payload_errored,
};
pub use pricing::{PriceBook, Pricing};
pub use registry::{
    ADAPTERS, adapter_by_kind, all_adapters, descriptor_by_kind, find_adapter, known_kinds,
    resumed_session_id_for_root, resumed_session_id_from_cmdline,
};
pub use spending::{HeadlineSpec, SpendTally, SpendWindow, SpendWindowMode, Spending};
pub(crate) use state::WindowSurplus;
pub use state::{
    ATTENTION_AGE_CEILING_SECS, AgentSignal, AgentState, AgentStatus, COMPACTING_WINDOW_SECS,
    ContextSeverity, DEFAULT_ARCHIVE_AFTER_SECS, DEFAULT_INACTIVE_AFTER_SECS,
    DEFAULT_STALL_AFTER_SECS, OpenAsk, is_native_permission_wait, is_stalled, is_turn_complete,
    is_turn_dead, is_turn_interrupted, looks_like_control_text, single_line_description,
    usable_description,
};
pub(crate) use state::{
    AccountBudget, ResumeArm, account_budgets_from_caches, display_turn_error,
    effective_turn_error_class, longest_window_reset_at, longest_window_running,
    longest_window_surplus, rate_limit_window_kinds, read_rate_limits_cache, resume_gate_recovered,
    resume_park, shortest_window_running,
};
pub use state::{PendingRefill, RateLimitsCache};
pub use transcript::{TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole};
pub use transcript_fs::read_transcript_lines;
pub(crate) use transcript_fs::read_transcript_tail;

pub use amp::AmpAdapter;
pub use antigravity::AntigravityAdapter;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use copilot::CopilotAdapter;
pub use cursor::CursorAdapter;
pub use droid::DroidAdapter;
pub use kimi::KimiAdapter;
pub use kiro::KiroAdapter;
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;
pub use qwen::QwenAdapter;

#[derive(Debug, thiserror::Error)]
pub enum AgentErr {
    #[error("unknown agent integration `{0}`")]
    Unknown(String),
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
    WriteHookConfig(#[from] crate::store::atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, AgentErr>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPreset {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Absolute path to a file whose contents replace the agent's base system
    /// prompt. Resolved and existence-checked by the launcher before render.
    pub system_prompt_file: Option<PathBuf>,
    /// Absolute path to a file whose contents are appended to the agent's base
    /// system prompt. Resolved and existence-checked by the launcher before render.
    pub append_system_prompt_file: Option<PathBuf>,
}

impl LaunchPreset {
    pub fn is_empty(&self) -> bool {
        self.model.as_deref().is_none_or(str::is_empty)
            && self.effort.as_deref().is_none_or(str::is_empty)
            && self.system_prompt_file.is_none()
            && self.append_system_prompt_file.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetField {
    Model,
    Effort,
    SystemPromptFile,
    AppendSystemPromptFile,
}

/// How a launch-preset field appears in the agent's own CLI argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetArgMatcher {
    /// A single-use named flag: `--model VALUE` or `--model=VALUE`.
    Flag(Vec<String>),
    /// A repeatable config override carrying the field as `<flag> <key>=VALUE`.
    ConfigKey { flags: Vec<String>, key: String },
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
    /// `docs/internals/agents/model.md`, lifecycle is the durable state channel.
    /// Whether a lifecycle event records anything is decided by
    /// [`AgentAdapter::observe_lifecycle`] returning `Some`.
    Lifecycle,
    AwaitingUser,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassifiedHook {
    pub class: AgentHookClass,
    pub ask_kind: Option<AskKind>,
    pub event_name: String,
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
pub struct DerivedAskFixture {
    pub event_name: &'static str,
    pub payload: Value,
    pub transcript_file_name: &'static str,
    pub transcript_body: &'static str,
    pub expected_kind: AskKind,
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
        ask_kind: Option<AskKind>,
    ) -> Self {
        Self {
            event_name,
            payload,
            expected: ClassifiedHook {
                class,
                ask_kind,
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
    /// Additional provider config files written by the same install. Most
    /// adapters keep hooks and statusline settings in one file; providers that
    /// split those surfaces list every secondary path here so the security
    /// surface stays visible in JSON and human output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_config_paths: Vec<PathBuf>,
}

/// One additional config-file change in a hook-install preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookConfigPreview {
    pub config_path: PathBuf,
    pub original_config: Option<String>,
    pub candidate_config: String,
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
    /// is in `candidate_config` or the owning `additional_configs` diff. `None`
    /// for agents that manage no statusline (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line_change: Option<StatusLineChange>,
    /// How the install changes the agent's `subagentStatusLine` (the per-child
    /// render command), same consent-surface discipline as `status_line_change`.
    /// `None` for agents that manage no subagent statusline (Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_status_line_change: Option<StatusLineChange>,
    /// Additional provider files changed by this install. Their complete diffs
    /// render under the primary hook file before consent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_configs: Vec<HookConfigPreview>,
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
    /// Additional provider config files inspected and, when Rimz-owned state
    /// existed, restored by this uninstall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_config_paths: Vec<PathBuf>,
}

/// Trigger path for adapter-owned enrichment refreshes.
#[derive(Clone, Copy, Debug)]
pub enum RefreshTrigger<'a> {
    /// Native event name on the hook path.
    Hook(&'a str),
    /// Sidebar producer periodic pass over live root sessions.
    Tick,
    /// Filesystem watcher response to transcript growth.
    Watch,
}

/// Context for [`AgentAdapter::context_refresh_spawn`]: the session and
/// workspace to refresh, plus the model hint its latest observation resolved.
/// On [`RefreshTrigger::Tick`] and [`RefreshTrigger::Watch`], callers pass
/// `server_url: None`.
pub struct LifecycleRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub workspace_id: &'a str,
    pub model_hint: Option<&'a str>,
    pub server_url: Option<&'a str>,
}

/// File identity for a bounded transcript, rollout, or telemetry tail read. Producers persist it
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

/// Context for [`AgentAdapter::local_context_refresh`]: the session to refresh,
/// its current model hint, and the local-source gate state from the latest
/// sidecar.
pub struct LocalContextRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub model_hint: Option<&'a str>,
    /// Transcript path carried by the current hook payload, if any.
    pub current_transcript_path: Option<&'a str>,
    pub prior_transcript_path: Option<&'a str>,
    pub prior_transcript_stat: Option<&'a TranscriptStat>,
    /// Persistent shared `pricing-cache.json`, for adapters that price token
    /// counts into card dollars. The spending producer owns writes.
    pub shared_pricing_cache_path: &'a Path,
}

/// Display-only context derived from a local transcript, rollout, or telemetry read. The
/// adapter owns the provider mapping; the CLI owns merging and writing the
/// sidecar.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalContextRefresh {
    pub model_id: Option<String>,
    pub effort: Option<String>,
    pub tokens: Option<AgentTokenUsage>,
    pub cost: Option<AgentCost>,
    /// Provider turn-death marker derived from a local transcript tail. Codex
    /// uses it for failures that write no hook-owned error; non-detector local
    /// refreshes leave sidecar error state untouched at merge time.
    pub turn_error: Option<AgentTurnError>,
    /// Timestamp of a cleanly-completed turn read from the rollout tail
    /// (`detect_turn_complete`), set when the session is at rest on a
    /// `task_complete` that fired no `Stop` hook (a `/review` turn). The
    /// projection reads it to settle a falsely-`running` row to `success`.
    pub turn_complete: Option<Timestamp>,
    /// Timestamp of a cleanly-completed planning turn whose rollout carries a
    /// `Plan` item. The projection settles a falsely-`running` row to `waiting`.
    pub plan_proposed: Option<Timestamp>,
    /// Timestamp of an interrupted turn read from the rollout tail
    /// (`turn_aborted`), set when the session is at rest after an abort that
    /// fired no `Stop` hook (Codex `/clear` mid-turn or Esc). The projection
    /// reads it to settle a falsely-`running` row to `idle`.
    pub turn_interrupted: Option<Timestamp>,
    pub transcript_path: Option<String>,
    pub transcript_stat: Option<TranscriptStat>,
}

/// Provider-owned local session truth normalized for transient sidebar
/// projection. Adapters validate native paths and payloads before constructing
/// this shape; snapshot code never sees provider wire records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSessionObservation {
    pub kind: crate::ids::AgentKind,
    pub session_id: crate::ids::AgentSessionId,
    pub workspace: PathBuf,
    pub transcript_path: PathBuf,
    pub created_at: Timestamp,
    pub first_event_at: Option<Timestamp>,
    pub last_activity: Timestamp,
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub latest_prompt: Option<String>,
    pub native_prompt_detail: Option<String>,
    pub waiting_since: Option<Timestamp>,
    pub context_pct: Option<u8>,
}

/// A detached `rimz` helper an adapter requests after a lifecycle event lands
/// — just the argv. The CLI owns the spawn discipline (fresh, fully-nulled
/// stdio; fire-and-forget), so adapters stay pure mappers.
pub struct RefreshSpawn {
    /// Arguments to the `rimz` binary itself.
    pub args: Vec<String>,
}

/// Account usage read from a provider-owned realtime account channel.
pub struct RealtimeAccountUsage {
    pub rate_limits: Option<AgentRateLimits>,
    pub extra_credits: Option<ExtraCredits>,
    pub reset_credits: Option<ResetCredits>,
}

/// Dynamic remote-control state read from the agent's own machine-local
/// settings and the already-published account cache. Static capability still
/// lives in [`AgentDescriptor`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteControlStatus {
    /// Existing pane sessions auto-enable their own remote-control surface.
    pub pane_auto: bool,
}

/// One validated answer to one native question. `picks` are zero-based option
/// positions after the CLI resolves numeric and label selectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskReply {
    pub picks: Vec<usize>,
    pub text: Option<String>,
}

/// Backend-neutral pane action emitted by an adapter's answer planner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerStep {
    Text(String),
    Key(NamedKey),
    Paste(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AnswerPlanErr {
    #[error("{0} does not support structured answers")]
    Unsupported(&'static str),
    #[error("{0}")]
    Invalid(String),
}

/// Assemble a fresh-launch argv with the startup prompt protected as the
/// agent's positional argument. The `--` terminator keeps a trailing variadic
/// or optional-value profile flag from consuming the prompt.
pub(crate) fn positional_prompt_argv(
    bin: &str,
    extra_args: &[String],
    prompt: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![bin.to_owned()];
    argv.extend(extra_args.iter().cloned());
    if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
        argv.push("--".to_owned());
        argv.push(prompt.to_owned());
    }
    argv
}

pub trait AgentAdapter: Send + Sync {
    /// The adapter's static identity, branding, capabilities, and
    /// classification tables. Everything `const` about an agent lives here;
    /// the trait methods own everything behavioral.
    fn descriptor(&self) -> &'static AgentDescriptor;

    /// Model slug to use when `rimz agents` launches without a configured
    /// model, and before a lazy-registering agent reports a real session
    /// model. Defaults to the descriptor's provider fallback; adapters with
    /// user-configured launch defaults override it.
    fn default_launch_model(&self) -> Option<String> {
        self.descriptor().default_model.map(ToOwned::to_owned)
    }

    /// The agent's configured launch model and reasoning effort, used only as
    /// the lowest-priority card-identity fallback after native payloads and the
    /// launcher-selected preset env.
    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (None, None)
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

    /// Test-only transcript-backed ask fixture for native prompts whose hook
    /// payload remains lifecycle-only. Conformance materializes the transcript
    /// and feeds the event through [`Self::observe_lifecycle`].
    #[cfg(test)]
    fn derived_ask_fixture(&self) -> Option<DerivedAskFixture> {
        None
    }

    /// Test-only representative provider-owned local session observation.
    /// Conformance keeps this evidence separate from executable-hook corpus.
    #[cfg(test)]
    fn local_session_fixture(&self) -> Option<LocalSessionObservation> {
        None
    }

    /// Render the neutral no-op — the "agent's own UI is the answer" fallback
    /// path. `None` means the hook should print nothing on this event.
    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>>;

    /// Observe a lifecycle event payload and translate it into the
    /// [`LifecycleSignal`](lifecycle::LifecycleSignal) (plus enrichment) the
    /// store should record. The adapter names the intent; the
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

    /// Detect a provider turn-interruption marker from local transcript or
    /// rollout evidence. The marker is display-only enrichment: it rides the
    /// context sidecar and settles a falsely active row when it postdates the
    /// latest lifecycle activity.
    fn observe_turn_interrupted(&self, _payload: &Value) -> Option<Timestamp> {
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

    /// Parse main-thread transcript JSONL text into normalized conversation
    /// messages, newest last. Adapters own native event shapes and keep
    /// sidechain/subagent replay out of this stream. Defaults to no transcript
    /// surface.
    fn parse_transcript_messages(&self, _lines: &str) -> Vec<transcript::TranscriptMessage> {
        Vec::new()
    }

    /// Read and normalize one complete provider-native transcript source.
    ///
    /// JSONL adapters inherit the text-file implementation. Adapters backed by
    /// a row store override this method, so history callers never need to know
    /// whether a recorded transcript path names text or a database. The typed
    /// session id selects one conversation when a source contains many.
    fn read_transcript_messages(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
    ) -> std::io::Result<Vec<transcript::TranscriptMessage>> {
        std::fs::read_to_string(path).map(|lines| self.parse_transcript_messages(&lines))
    }

    /// Extract newly appended main-thread assistant messages from transcript
    /// JSONL text. The CLI owns the cursor and output transport; adapters own
    /// their native transcript event shapes. Defaults to filtering the
    /// normalized transcript parser.
    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        self.parse_transcript_messages(new_lines)
            .into_iter()
            .filter(|message| message.role == transcript::TranscriptRole::Assistant)
            .map(|message| message.text)
            .collect()
    }

    /// Return the current monotonic end position for a transcript source.
    /// JSONL uses bytes; row-backed adapters can use their highest row id. The
    /// position belongs to the selected session within a shared row store.
    fn transcript_position(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
    ) -> Option<transcript::TranscriptPosition> {
        std::fs::metadata(path)
            .ok()
            .map(|meta| transcript::TranscriptPosition::new(meta.len()))
    }

    /// Read assistant output after `position`, returning the next source-owned
    /// cursor. The default implements torn-write-safe JSONL byte reads.
    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        _session_id: Option<&crate::ids::AgentSessionId>,
        position: transcript::TranscriptPosition,
    ) -> Option<transcript::TranscriptPage> {
        let (bytes, next) = read_transcript_lines(path, position.get())?;
        let lines = String::from_utf8_lossy(&bytes);
        Some(transcript::TranscriptPage {
            next: transcript::TranscriptPosition::new(next),
            messages: self.stream_assistant_messages(&lines),
        })
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

    /// Whether this event ends an agent session. Defaults to `false`; adapters
    /// override for their session-exit event.
    fn ends_session(&self, _event_name: &str) -> bool {
        false
    }

    /// Whether this event means a still-live session moved on from a native
    /// prompt — a new prompt or the end of its turn. Defaults to `false`;
    /// adapters override for their turn-boundary events.
    fn moves_on(&self, _event_name: &str) -> bool {
        false
    }

    /// Structured question/options for a blocking ask hook, parsed from the
    /// agent-native payload. `None` means the hook carries no native question
    /// text.
    fn ask_question_detail(&self, _event_name: &str, _payload: &Value) -> Option<Vec<AskQuestion>> {
        None
    }

    /// Short summary carried directly on an open ask. Structured questions
    /// remain in the transcript; this covers prompts such as permissions that
    /// intentionally do not create transcript ask entries.
    fn ask_detail(&self, event_name: &str, payload: &Value) -> Option<String> {
        self.ask_question_detail(event_name, payload)
            .and_then(|questions| questions.into_iter().next())
            .map(|question| {
                question
                    .question
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .filter(|detail| !detail.is_empty())
    }

    /// Canonical options for an ask whose native event does not carry a
    /// structured question list.
    fn ask_options(&self, _kind: AskKind) -> Option<Vec<AskOption>> {
        None
    }

    /// Map validated semantic answers to this agent's native TUI choreography.
    fn answer_plan(
        &self,
        _kind: AskKind,
        _questions: &[AskQuestion],
        _answers: &[AskReply],
    ) -> std::result::Result<Vec<AnswerStep>, AnswerPlanErr> {
        Err(AnswerPlanErr::Unsupported(self.descriptor().kind))
    }

    /// Structured answer choices reported when a native ask completes in the
    /// agent's own UI. `Some` drives both the transcript answer entry and
    /// pending native ask expiry; `None` means this event carries no native ask
    /// answer.
    fn native_ask_answer(&self, _event_name: &str, _payload: &Value) -> Option<Vec<AskAnswer>> {
        None
    }

    /// Extract provider-declared final visible assistant output from a native
    /// content event. Implementations accept only a provider's dedicated final
    /// response field, never transcript text, reasoning, or partial deltas.
    fn observe_assistant_message(&self, _event_name: &str, _payload: &Value) -> Option<String> {
        None
    }

    /// A detached `rimz` helper to spawn after a lifecycle event or producer
    /// tick — the out-of-band enrichment lane. The caller spawns it with fresh,
    /// fully-nulled stdio and never waits, so it adds no latency to the
    /// agent's turn. Display-only enrichment, never correctness. Defaults to
    /// `None` for an agent with no out-of-band refresh.
    fn context_refresh_spawn(
        &self,
        _trigger: RefreshTrigger<'_>,
        _ctx: &LifecycleRefreshCtx<'_>,
    ) -> Option<RefreshSpawn> {
        None
    }

    /// A cheap, synchronous local enrichment read to run inline after a
    /// progress-proving hook event. This is for bounded file reads that are
    /// lighter than the store write already performed by the hook or cheap
    /// enough for a producer tick; network, subprocess, broker, or app-server
    /// work belongs in [`context_refresh_spawn`](Self::context_refresh_spawn).
    /// The adapter returns mapped fields only and never writes the sidecar
    /// itself.
    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        _ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        None
    }

    /// Discover validated sessions for one absolute workspace from the
    /// provider's machine-local store. The result is pulled display truth;
    /// callers bind it only to currently live panes and never append it to the
    /// Rimz event log.
    fn discover_local_sessions(&self, _workspace: &Path) -> Vec<LocalSessionObservation> {
        Vec::new()
    }

    /// Parse a provider-native resumed-session command line. Implementations
    /// accept only their actual interactive launcher/engine forms and return a
    /// typed, non-empty session identity.
    fn resumed_session_id_from_cmdline(
        &self,
        _cmdline: &str,
    ) -> Option<crate::ids::AgentSessionId> {
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

    /// Modification stamp (unix ms) of the credential source behind
    /// [`probe_oauth_usage`](Self::probe_oauth_usage), so the refresh driver
    /// can retry a settled auth failure the moment credentials change. `None`
    /// when the source is not a local file.
    fn oauth_credentials_stamp(&self) -> Option<u64> {
        None
    }

    /// Stable identifier of the current local OAuth login behind
    /// [`probe_oauth_usage`](Self::probe_oauth_usage), used to detect an
    /// account switch and drop account-scoped caches. `None` when the provider
    /// has no cheap local identity.
    fn oauth_account_key(&self) -> Option<String> {
        None
    }

    /// Probe the provider's own realtime account channel while idle.
    /// Producer-only, best-effort, and read-only: no store writes happen in the
    /// adapter, and the caller owns every cache merge. `RuntimePaths` lets the
    /// adapter locate its local sockets.
    fn probe_realtime_account_usage(
        &self,
        _runtime: &crate::RuntimePaths,
    ) -> Option<RealtimeAccountUsage> {
        None
    }

    /// Dynamic remote-control state from this provider's own settings and
    /// account facts.
    /// Best-effort and read-only: failures return the default "off/unknown"
    /// state. The sidebar uses this only to light a capability-gated flag.
    fn remote_control_status(&self, _account: Option<&AgentAccount>) -> RemoteControlStatus {
        RemoteControlStatus::default()
    }

    /// Probe the agent binary's version out-of-band. Producer-only and
    /// display-only: a failure leaves the provider header without a version,
    /// never affecting account truth, cache freshness, or store correctness.
    fn probe_version(&self) -> Option<String> {
        probe_descriptor_version(self.descriptor())
    }

    /// Every conversation/spend JSONL this agent has on disk, fleet-wide — the
    /// discovery walk for the full-history spending pass
    /// ([`spending::SpendingWalker`]). Distinct from the bounded tail read in
    /// [`observe_lifecycle`](Self::observe_lifecycle): this walks the whole
    /// history for spend. Defaults to none for an agent with no transcript
    /// surface.
    fn transcript_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Resolve the local conversation/store that carries a live session's spend.
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
    /// Read-only and sidebar-safe — spend parsing never writes the store or
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
    /// the agent has no resume CLI, so [`crate::harness::resume::plan_resume`] skips it.
    /// Default `None` keeps the contract "implement nothing else" for an agent
    /// that cannot resume yet.
    fn resume_command(&self, _session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        None
    }

    /// The argv that forks a prior session of this agent by `session_id`:
    /// resume the full conversation history under a provider-assigned new
    /// session id, leaving the source session untouched. Launched fresh in
    /// `cwd` (the source agent's worktree). `None` when the agent has no native
    /// fork CLI, so `rimz agents fork` refuses with the reason.
    fn fork_command(&self, _session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
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

    /// Extra launch argv for a supervised print-mode agentic-turn cap. Returns
    /// `None` when the agent exposes no native turn limit.
    fn max_turns_args(&self, _limit: u32) -> Option<Vec<String>> {
        None
    }

    /// The interactive slash command that triggers a manual context compaction
    /// in the agent's own composer. Typed as keystrokes ahead of a steered or
    /// queued message under `--smart-compact`, never a bracketed paste — agents
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
        if preset.append_system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: self.descriptor().kind,
                field: "append-system-prompt-file",
            });
        }
        Ok(Vec::new())
    }

    /// Describe the provider-native argv spelling rendered for a preset field.
    fn preset_arg_matcher(&self, _field: PresetField) -> Option<PresetArgMatcher> {
        None
    }

    /// The argv that launches a fresh interactive session of this agent in the
    /// pane's cwd. `extra_args` are direct agent CLI arguments from the chosen
    /// tab layout; `prompt`, when present, is passed as the agent's positional
    /// startup prompt after a `--` terminator. An agent with no launch CLI
    /// returns `None`.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnLifecycleNeed {
    None,
    NotUnsupported,
    Wired,
}

#[derive(Debug, thiserror::Error)]
pub enum HookPreflightErr {
    #[error("turn lifecycle unsupported: {reason}")]
    TurnLifecycleUnsupported { reason: String },
    #[error("hooks missing")]
    HooksMissing,
    #[error("hooks untrusted: {hooks}")]
    HooksUntrusted { hooks: String, fix: String },
}

/// Check the hook installation, trust, and requested turn-lifecycle coverage
/// needed before a hook-driven operation starts.
pub fn preflight_hooks(
    adapter: &dyn AgentAdapter,
    lifecycle: TurnLifecycleNeed,
) -> std::result::Result<(), HookPreflightErr> {
    let coverage = adapter
        .descriptor()
        .concern_coverage(IntegrationConcern::TurnLifecycle);
    if let Some(reason) = turn_lifecycle_gap(coverage, lifecycle) {
        return Err(HookPreflightErr::TurnLifecycleUnsupported {
            reason: reason.to_owned(),
        });
    }
    if !adapter.hooks_installed() {
        return Err(HookPreflightErr::HooksMissing);
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        return Err(HookPreflightErr::HooksUntrusted {
            hooks: untrusted.join(", "),
            fix: hook_trust_fix(adapter.descriptor().kind),
        });
    }
    Ok(())
}

fn turn_lifecycle_gap(
    coverage: Option<ConcernCoverage>,
    need: TurnLifecycleNeed,
) -> Option<&'static str> {
    match need {
        TurnLifecycleNeed::None => None,
        TurnLifecycleNeed::NotUnsupported => match coverage {
            Some(ConcernCoverage::Unsupported { reason }) => Some(reason),
            None | Some(ConcernCoverage::Wired { .. } | ConcernCoverage::Partial { .. }) => None,
        },
        TurnLifecycleNeed::Wired => coverage
            .filter(|coverage| !coverage.is_wired())
            .map(ConcernCoverage::detail),
    }
}

/// One-line fix for an installed-but-untrusted hook set, shared by hook
/// preflights, the `rimz start` notice, and `rimz doctor`.
pub fn hook_trust_fix(kind: &str) -> String {
    format!("run /hooks inside {kind} and trust the Rimz hooks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_preflight_preserves_partial_coverage_strictness() {
        let partial = Some(ConcernCoverage::Partial {
            via: "derived",
            gap: "not executable",
        });
        let unsupported = Some(ConcernCoverage::Unsupported {
            reason: "no lifecycle signal",
        });

        assert_eq!(
            turn_lifecycle_gap(partial, TurnLifecycleNeed::NotUnsupported),
            None
        );
        assert_eq!(
            turn_lifecycle_gap(partial, TurnLifecycleNeed::Wired),
            Some("not executable")
        );
        assert_eq!(
            turn_lifecycle_gap(unsupported, TurnLifecycleNeed::NotUnsupported),
            Some("no lifecycle signal")
        );
        assert_eq!(
            turn_lifecycle_gap(unsupported, TurnLifecycleNeed::None),
            None
        );
    }
}
