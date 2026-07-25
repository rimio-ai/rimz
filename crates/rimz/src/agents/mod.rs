//! Provider-neutral agent catalog, domain types, and workflow services.
//!
//! [`AgentDefinition`] composes immutable [`AgentSpec`] facts with optional
//! caller-aligned capabilities. Private provider implementations parse native
//! inputs and return neutral results; CLI, sidebar, harness, and store callers
//! resolve definitions without importing provider modules. The registry joins
//! built-ins with validated process plugins.

pub mod account;
mod adapters;
pub mod attribution;
pub mod capabilities;
#[cfg(test)]
pub(crate) mod conformance;
pub mod context;
pub mod credits;
pub mod definition;
pub(crate) mod delegated_account;
mod emblems;
pub(crate) mod hook_types;
pub(crate) mod identity;
pub(crate) mod jsonc;
pub mod lifecycle;
mod local_session_cache;
pub(crate) mod locate;
pub(crate) mod managed_json_hooks;
mod managed_source;
pub(crate) mod managed_statusline;
pub mod model_display;
mod observation;
mod open_ask;
pub(crate) mod payload;
pub mod pricing;
pub(crate) mod question;
pub mod registry;
pub mod runtime_control;
pub mod session;
pub(crate) mod settings_json;
pub mod spending;
pub mod state;
#[cfg(test)]
pub(crate) mod testkit;
pub mod transcript;
pub(crate) mod transcript_fs;
pub mod turns;
pub mod version;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mux::NamedKey;
use crate::pane::RuntimeOwnerKind;
use crate::transcript::{AskOption, AskQuestion};

pub(crate) use account::WindowSurplus;
#[doc(hidden)]
pub use account::provider_budget_gate;
pub use account::{
    AccountUsageIdentity, ManagedLaunchState, PendingRefill, ProviderAccountBinding,
    ProviderCapacity, RateLimitCacheEntry, RateLimitsCache,
};
pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentPullRequest, AgentRateLimits,
    AgentSessionUsage, AgentTokenUsage, AgentTurnError, CacheHealth, ContextObservation,
    CostCoverage, FieldPatch, LocalContextPatch, LocalTokenPatch, ProviderAccountScope,
    RateLimitWindow, RateLimitWindowScope, SessionContextInput, SessionContextRefresh,
    SubagentContext, SubagentObservation, TurnErrorClass, TurnSettle, TurnSettleOutcome,
};
pub(crate) use credits::HttpErrKind;
pub use credits::{AccountUsageProbe, AccountUsageSnapshot, ExtraCredits, ResetCredits};
pub use definition::{
    AgentDefinition, AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage,
    DefinitionValidationError, HookCoverage, IntegrationConcern, LaunchPermissionArgs, LaunchSpec,
    PlanLabel, PresetMatchers, PromptStyle, RealtimeUsageChannel, RemoteControlCapability,
    SamePaneSessionPolicy, SessionCommand, StaticPresetMatcher, ThreadKey, ToolClassification,
    UserCapability, UserCoverage, program_names_kind,
};
pub use emblems::{Emblem, EmblemTint, emblem_for};
pub use hook_types::{
    CanonicalHookEvent, CanonicalHookFact, CanonicalHookMeaning, HookOutput, HookReply, HookRouting,
};
pub(crate) use identity::{
    RootIdentity, SubagentIdentity, resolve_root_identity, resolve_subagent_identity,
};
pub use lifecycle::{
    AskKind, CONDITION_CHECKPOINT, DELIVERY_CHECKPOINT, LIFECYCLE_EVENT_VERSION, LifecycleEvent,
    LifecycleFollowBatch, LifecycleFollowErr, LifecycleFollower, LifecycleSignal,
    LifecycleSignalKind, LifecycleState, LifecycleTransition, SignalSet, Transition,
    TransitionKind, TurnPhase, step,
};
pub use locate::locate_binary;
pub(crate) use locate::{agent_config_path, probe_descriptor_version, read_optional_file};
pub use managed_source::{ManagedIntegration, ManagedSource};
pub use observation::{
    AgentLifecycleObservation, AgentUsageSummary, LaunchParams, SessionOrigin, SpawnedSubagent,
    SubagentCorrelation, SubagentCorrelationInput, SubagentSpawnInput,
};
pub use open_ask::{OpenAskDetail, OpenAskReadErr, read_open_ask};
pub(crate) use payload::{
    CONTROL_TAG_PREFIXES, non_empty_trimmed, optional_payload_string, sanitize_user_prompt,
    stop_payload_errored,
};
pub use pricing::{PriceBook, Pricing, TokenSplit};
pub use registry::{
    all_definitions, definition_by_kind, find_definition, known_kinds, resumed_session_id_for_root,
    resumed_session_id_from_cmdline, spec_by_kind,
};
pub use spending::{HeadlineSpec, SpendTally, SpendWindow, SpendWindowMode, Spending};
pub use state::{
    ATTENTION_AGE_CEILING_SECS, AgentCardRef, AgentState, AgentStatus, COMPACTING_WINDOW_SECS,
    ContextSeverity, DEFAULT_ACTIVE_GRACE_SECS, DEFAULT_ARCHIVE_AFTER_SECS,
    DEFAULT_INACTIVE_AFTER_SECS, DEFAULT_STALL_AFTER_SECS, DEFAULT_TOOL_REPEAT_ATTENTION_AFTER,
    DEFAULT_TOOL_REPEAT_WARN_AFTER, OpenAsk, is_stalled, is_tool_looping, is_turn_dead,
    settled_outcome, single_line_description, usable_description,
};
pub(crate) use state::{display_turn_error, effective_turn_error_class};
pub use transcript::{TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole};
pub use transcript_fs::read_transcript_lines;
pub(crate) use transcript_fs::{read_transcript_tail, read_transcript_tail_with_status};

pub mod plugins;

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

impl PresetField {
    pub(crate) const fn flag_name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::SystemPromptFile => "system-prompt-file",
            Self::AppendSystemPromptFile => "append-system-prompt-file",
        }
    }

    pub(crate) fn launch_preset(self, value: String) -> LaunchPreset {
        let mut preset = LaunchPreset::default();
        match self {
            Self::Model => preset.model = Some(value),
            Self::Effort => preset.effort = Some(value),
            Self::SystemPromptFile => preset.system_prompt_file = Some(value.into()),
            Self::AppendSystemPromptFile => preset.append_system_prompt_file = Some(value.into()),
        }
        preset
    }
}

/// How a launch-preset field appears in the agent's own CLI argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetArgMatcher {
    /// A single-use named flag: `--model VALUE` or `--model=VALUE`.
    Flag(Vec<String>),
    /// A repeatable config override carrying the field as `<flag> <key>=VALUE`.
    ConfigKey { flags: Vec<String>, key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetArgOccurrence {
    pub(crate) argv_range: std::ops::Range<usize>,
    pub(crate) value: String,
}

impl PresetArgMatcher {
    pub(crate) fn occurrences(&self, argv: &[String]) -> Vec<PresetArgOccurrence> {
        let mut occurrences = Vec::new();
        let mut index = 0;
        while index < argv.len() {
            let occurrence = match self {
                Self::Flag(flags) => flags.iter().find_map(|flag| {
                    if argv[index] == *flag {
                        argv.get(index + 1).map(|value| PresetArgOccurrence {
                            argv_range: index..index + 2,
                            value: value.clone(),
                        })
                    } else {
                        argv[index]
                            .strip_prefix(flag)
                            .and_then(|suffix| suffix.strip_prefix('='))
                            .map(|value| PresetArgOccurrence {
                                argv_range: index..index + 1,
                                value: value.to_owned(),
                            })
                    }
                }),
                Self::ConfigKey { flags, key } => flags.iter().find_map(|flag| {
                    let prefix = format!("{key}=");
                    if argv[index] == *flag {
                        argv.get(index + 1)
                            .and_then(|value| value.strip_prefix(&prefix))
                            .map(|value| PresetArgOccurrence {
                                argv_range: index..index + 2,
                                value: value.to_owned(),
                            })
                    } else {
                        argv[index]
                            .strip_prefix(flag)
                            .and_then(|suffix| suffix.strip_prefix('='))
                            .and_then(|value| value.strip_prefix(&prefix))
                            .map(|value| PresetArgOccurrence {
                                argv_range: index..index + 1,
                                value: value.to_owned(),
                            })
                    }
                }),
            };
            if let Some(occurrence) = occurrence {
                index = occurrence.argv_range.end;
                occurrences.push(occurrence);
            } else {
                index += 1;
            }
        }
        occurrences
    }

    pub(crate) fn display_setting(&self, value: &str) -> String {
        match self {
            Self::Flag(flags) => flags
                .first()
                .map_or_else(|| value.to_owned(), |flag| format!("{flag} {value}")),
            Self::ConfigKey { key, .. } => format!("{key} {value}"),
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentHookClass {
    /// Non-blocking event that may carry a status/mode/task transition for
    /// the agent rollup (`SessionStart`, `UserPromptSubmit`, `Stop`, …). Per
    /// `docs/internals/agents/model.md`, lifecycle is the durable state channel.
    /// Whether a lifecycle event records anything is decided by
    /// [`HookOutput::lifecycle`] carrying an observation.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookIngressIgnoreReason {
    ClaudeRemoteControl,
    CodexInternalAppServer,
    DroidStockTui,
}

impl HookIngressIgnoreReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeRemoteControl => "claude_remote_control",
            Self::CodexInternalAppServer => "codex_internal_app_server",
            Self::DroidStockTui => "droid_stock_tui",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookIngressOwner {
    pub pid: Option<u32>,
    pub kind: RuntimeOwnerKind,
}

impl HookIngressOwner {
    pub const fn agent(pid: Option<u32>) -> Self {
        Self {
            pid,
            kind: RuntimeOwnerKind::Agent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookIngressAcceptance {
    pub owner: HookIngressOwner,
    pub participant_start: Option<PathBuf>,
}

impl HookIngressAcceptance {
    pub fn agent(pid: Option<u32>) -> Self {
        Self {
            owner: HookIngressOwner::agent(pid),
            participant_start: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookIngressDecision {
    Ignore(HookIngressIgnoreReason),
    Accept(HookIngressAcceptance),
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
pub struct TurnCostFixture {
    pub event_name: &'static str,
    pub payload: Value,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct ContextCostFixture {
    pub payload: Value,
}

/// One provider turn priced for a live-session accumulator.
#[derive(Clone, Debug, PartialEq)]
pub struct LocallyPricedTurnCost {
    pub turn_id: String,
    pub cost_usd: f64,
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

/// One adapter-owned fixture set for registry-wide conformance checks.
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub struct AdapterConformance {
    pub classification: Vec<ClassificationSample>,
    pub spend: Option<SpendFixture>,
    pub hook_turn_cost: Option<TurnCostFixture>,
    pub context_cost: Option<ContextCostFixture>,
    pub derived_ask: Option<DerivedAskFixture>,
    pub local_session: Option<LocalSessionObservation>,
}

#[cfg(test)]
impl AdapterConformance {
    /// Deduplicated native event surface represented by the full corpus.
    pub(crate) fn native_event_names(&self) -> Vec<&'static str> {
        let mut events = Vec::new();
        for sample in &self.classification {
            if !events.contains(&sample.event_name) {
                events.push(sample.event_name);
            }
        }
        events
    }
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

/// Result of installing hooks. The CLI uses these fields to render the
/// per-agent event count and every config file touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInstallReport {
    pub agent: &'static str,
    /// Config files the installer wrote.
    pub files: Vec<HookInstallFileReport>,
    /// Event names installed (e.g. `SessionStart`, `PermissionRequest`).
    pub installed_events: Vec<String>,
}

/// One config file written by a completed hook install or uninstall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInstallFileReport {
    pub path: PathBuf,
    /// True when the file existed before the operation.
    pub existed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInstallPreview {
    pub agent: &'static str,
    pub files: Vec<HookInstallFilePreview>,
    pub planned_events: Vec<String>,
    /// How the install changes the agent's statusline, for the one-line consent
    /// summary that keeps the wrap a visible security surface. The full change
    /// is also in the matching file artifact's diff. `None` for agents that manage no
    /// statusline (Codex).
    pub status_line_change: Option<StatusLineChange>,
    /// How the install changes the agent's `subagentStatusLine` (the per-child
    /// render command), same consent-surface discipline as `status_line_change`.
    /// `None` for agents that manage no subagent statusline (Codex).
    pub subagent_status_line_change: Option<StatusLineChange>,
}

/// One exact config-file change in a hook-install preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInstallFilePreview {
    pub path: PathBuf,
    pub original: Option<String>,
    pub candidate: String,
    pub existed: bool,
}

/// What `rimz hooks install` does to the agent's statusline command, surfaced
/// in the consent gate alongside the hook diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusLineChange {
    /// No prior statusline; install adds RimZ's reader.
    Added,
    /// Wraps the user's existing statusline command, restored on uninstall.
    /// `original` is the user's command, shown verbatim in the summary.
    Wrapping { original: String },
    /// Re-install over an identical RimZ wrap — no change.
    Unchanged,
}

/// How an agent invokes a configured statusline command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusLineInvocation {
    /// The provider evaluates the configured command as shell text.
    #[default]
    Shell,
    /// The provider splits the configured command into argv and spawns it directly.
    DirectArgv,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookUninstallReport {
    pub agent: &'static str,
    pub files: Vec<HookInstallFileReport>,
    pub removed_events: Vec<String>,
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

/// Context for [`AgentDefinition::context_refresh_spawn`]: the session and
/// workspace to refresh, plus the model hint its latest observation resolved.
/// On [`RefreshTrigger::Tick`] and [`RefreshTrigger::Watch`], callers pass
/// `server_url: None`.
pub struct LifecycleRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub workspace_id: &'a str,
    pub model_hint: Option<&'a str>,
    pub server_url: Option<&'a str>,
}

/// The leading argv of the detached `rimz agents refresh-context` helper: the
/// one command that runs [`AgentDefinition::refresh_session_context`] for any
/// provider. An adapter appends its own flags.
pub fn refresh_context_argv(kind: &str, ctx: &LifecycleRefreshCtx<'_>) -> Vec<String> {
    vec![
        "agents".to_owned(),
        "refresh-context".to_owned(),
        "--kind".to_owned(),
        kind.to_owned(),
        "--session-id".to_owned(),
        ctx.agent_id.to_owned(),
        "--workspace-id".to_owned(),
        ctx.workspace_id.to_owned(),
    ]
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
    /// A second provider-owned file whose bytes participate in the same local
    /// context reading. Droid pairs its conversation JSONL with the sibling
    /// settings snapshot so either an AskUser record or fresh token telemetry
    /// invalidates one stat gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<TranscriptCompanionStat>,
}

/// Resumable per-request spend and raw token-split fold over a session's local transcript.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalSpendFold {
    pub cursor: spending::SpendCursor,
    pub total_usd: f64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub cache_read: u64,
}

impl LocalSpendFold {
    pub fn session_usage(&self) -> Option<AgentSessionUsage> {
        (self.input > 0 || self.output > 0 || self.cache_write > 0 || self.cache_read > 0)
            .then_some(AgentSessionUsage {
                input_tokens: Some(self.input),
                output_tokens: Some(self.output),
                cache_creation_input_tokens: Some(self.cache_write),
                cache_read_input_tokens: Some(self.cache_read),
                thinking_tokens: None,
            })
    }
}

impl TranscriptStat {
    /// Read the durable identity of a transcript-like file.
    pub fn from_path(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Self {
            mtime_secs: i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX),
            mtime_nanos: since_epoch.subsec_nanos(),
            len: metadata.len(),
            companion: None,
        })
    }

    /// Newest usable whole-second modification time across every file in this
    /// logical source. Spending age checks operate in Unix seconds, so times
    /// before the epoch retain the existing best-effort zero fallback.
    pub fn newest_mtime_secs(&self) -> u64 {
        let newest = self.companion.map_or(self.mtime_secs, |companion| {
            self.mtime_secs.max(companion.mtime_secs)
        });
        u64::try_from(newest).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCompanionStat {
    pub mtime_secs: i64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub mtime_nanos: u32,
    pub len: u64,
}

impl From<TranscriptStat> for TranscriptCompanionStat {
    fn from(stat: TranscriptStat) -> Self {
        Self {
            mtime_secs: stat.mtime_secs,
            mtime_nanos: stat.mtime_nanos,
            len: stat.len,
        }
    }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Context for [`AgentDefinition::local_context_refresh`]: the session to refresh,
/// its current model hint, and the local-source gate state from the latest
/// sidecar.
pub struct LocalContextRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub model_hint: Option<&'a str>,
    /// Transcript path carried by the current hook payload, if any.
    pub current_transcript_path: Option<&'a str>,
    pub prior_transcript_path: Option<&'a str>,
    pub prior_transcript_stat: Option<&'a TranscriptStat>,
    pub prior_spend_fold: Option<&'a LocalSpendFold>,
    /// Persistent shared `pricing-cache.json`, for adapters that price token
    /// counts into card dollars. The spending producer owns writes.
    pub shared_pricing_cache_path: &'a Path,
}

/// Display-only context derived from a local transcript, rollout, or telemetry read. The
/// adapter owns the provider mapping; the CLI owns merging and writing the
/// sidecar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalContextRefresh {
    /// Explicit field and token merge policy owned by the adapter.
    pub context: LocalContextPatch,
    /// Latest local source. `None` means the prior source disappeared.
    pub transcript_path: Option<String>,
    pub transcript_stat: Option<TranscriptStat>,
    /// Resumable pricing state is sparse: an absent new fold preserves the old one.
    pub spend_fold: FieldPatch<LocalSpendFold>,
}

impl LocalContextRefresh {
    /// Sparse enrichment updates only fields explicitly set by its producer.
    pub fn sparse() -> Self {
        Self::default()
    }

    /// A current local snapshot owns the latest-turn attention markers and
    /// reports an absent token reading while preserving an established gauge.
    pub fn authoritative_current() -> Self {
        Self {
            context: LocalContextPatch::authoritative_current(),
            ..Self::default()
        }
    }
}

/// Lifecycle fields projected by a provider-owned local session store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSessionState {
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub latest_prompt: Option<String>,
    pub native_prompt_detail: Option<String>,
    pub waiting_since: Option<Timestamp>,
    pub context_pct: Option<u8>,
}

/// The lifecycle authority carried by a local session observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalSessionProjection {
    /// The provider store proves identity and activity bounds only. Snapshot
    /// merge preserves exact durable lifecycle truth and synthesizes idle only
    /// when no durable session state exists.
    IdentityOnly,
    /// The provider store validates and folds lifecycle truth itself.
    Lifecycle(LocalSessionState),
}

/// Provider-owned local session binding evidence normalized for transient
/// sidebar projection. Adapters validate native paths and payloads before
/// constructing this shape; snapshot code never sees provider wire records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSessionObservation {
    pub kind: crate::ids::AgentKind,
    pub session_id: crate::ids::AgentSessionId,
    pub workspace: PathBuf,
    pub transcript_path: PathBuf,
    pub created_at: Timestamp,
    /// Adapter-authorized evidence for fresh same-cwd binding. This is
    /// independent of transcript activity: a provider may establish a safe
    /// session identity before its first lifecycle record exists.
    pub fresh_binding_at: Option<Timestamp>,
    /// Timestamp of the first real transcript or lifecycle record.
    pub first_event_at: Option<Timestamp>,
    pub last_activity: Timestamp,
    pub projection: LocalSessionProjection,
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
/// lives in [`AgentSpec`].
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
    adapter: &AgentDefinition,
    lifecycle: TurnLifecycleNeed,
) -> std::result::Result<(), HookPreflightErr> {
    let coverage = adapter
        .spec()
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
            fix: hook_trust_fix(adapter.spec().kind),
        });
    }
    Ok(())
}

fn turn_lifecycle_gap(coverage: ConcernCoverage, need: TurnLifecycleNeed) -> Option<&'static str> {
    match need {
        TurnLifecycleNeed::None => None,
        TurnLifecycleNeed::NotUnsupported => match coverage {
            ConcernCoverage::Unsupported { reason } => Some(reason),
            ConcernCoverage::Wired { .. } | ConcernCoverage::Partial { .. } => None,
        },
        TurnLifecycleNeed::Wired => (!coverage.is_wired()).then(|| coverage.detail()),
    }
}

/// One-line fix for an installed-but-untrusted hook set, shared by hook
/// preflights, the `rimz start` notice, and `rimz doctor`.
pub fn hook_trust_fix(kind: &str) -> String {
    format!("run /hooks inside {kind} and trust the RimZ hooks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_stat_from_path_preserves_metadata() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("transcript.jsonl");
        std::fs::write(&path, b"hello").unwrap();

        let stat = TranscriptStat::from_path(&path).unwrap();

        assert_eq!(stat.len, 5);
        assert!(stat.mtime_secs >= 0);
        assert!(stat.mtime_nanos < 1_000_000_000);
        assert_eq!(stat.companion, None);
    }

    #[test]
    fn lifecycle_preflight_preserves_partial_coverage_strictness() {
        let partial = ConcernCoverage::Partial {
            via: "derived",
            gap: "not executable",
        };
        let unsupported = ConcernCoverage::Unsupported {
            reason: "no lifecycle signal",
        };

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
