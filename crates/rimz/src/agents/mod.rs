//! Provider-neutral agent catalog, domain types, and workflow services.
//!
//! [`AgentDefinition`] composes immutable [`AgentSpec`] facts with optional
//! caller-aligned capabilities. Private provider implementations parse native
//! inputs and return neutral results; CLI, sidebar, harness, and store callers
//! resolve definitions without importing provider modules. The registry joins
//! built-ins with validated process plugins.

pub mod account;
pub mod account_links;
mod adapters;
pub mod attribution;
mod background_shell;
pub mod capabilities;
#[cfg(test)]
pub(crate) mod conformance;
pub mod context;
pub mod credits;
pub mod definition;
pub(crate) mod delegated_account;
mod emblems;
pub mod empty_dir;
mod folder_trust;
mod headless;
pub(crate) mod hook_types;
pub(crate) mod identity;
pub(crate) mod jsonc;
pub mod lifecycle;
mod local_session_cache;
pub(crate) mod locate;
pub mod login;
pub(crate) mod managed_json_hooks;
mod managed_source;
pub(crate) mod managed_statusline;
pub mod model_display;
mod observation;
mod open_ask;
mod park;
pub(crate) mod payload;
pub mod petname;
pub mod pricing;
mod provider_file;
pub(crate) mod question;
pub mod registry;
pub mod runtime_control;
pub mod session;
pub(crate) mod settings_json;
pub mod skill_links;
pub mod spending;
pub mod state;
#[cfg(test)]
pub(crate) mod testkit;
mod tools;
pub mod transcript;
pub(crate) mod transcript_fs;
pub mod turns;
pub mod version;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pane::RuntimeOwnerKind;
use crate::pane::keys::NamedKey;
use crate::transcript::{AskOption, AskQuestion};

pub(crate) use account::WindowSurplus;
#[doc(hidden)]
pub use account::provider_budget_gate;
pub use account::{
    AccountUsageIdentity, ManagedLaunchState, ProviderAccountBinding, ProviderCapacity,
    RateLimitCacheEntry, RateLimitsCache, WindowSpan,
};
pub use background_shell::{BackgroundShell, BackgroundShellReport};
pub use capabilities::ManualSkill;
pub use context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentSessionUsage,
    AgentTokenUsage, AgentTurnError, CacheHealth, CostCoverage, FieldPatch, LocalContextPatch,
    LocalTokenPatch, ProviderAccountScope, RateLimitWindow, RateLimitWindowScope,
    SessionContextInput, SubagentContext, SubagentUsageCursor, TurnErrorClass, TurnSettle,
    TurnSettleOutcome,
};
use context::{ContextObservation, SessionContextRefresh, SubagentObservation};
use credits::HttpErrKind;
pub use credits::{
    AccountUsageProbe, AccountUsageSnapshot, Entitlement, ExtraCredits, RedeemEffect, ResetCredits,
};
pub use definition::{
    AgentDefinition, AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage,
    HookCoverage, IntegrationConcern, LocalSessionDiscovery, PermissionMode, PlanLabel,
    SamePaneSessionPolicy, UserCapability,
};
use definition::{
    CompactCommand, CompactInstruction, FastSwitch, LaunchPermissionArgs, LaunchSpec,
    PresetMatchers, PromptStyle, SessionCommand, StaticPresetMatcher, program_names_kind,
};
pub use delegated_account::expects_usage_reading;
pub(crate) use emblems::emblem_for;
pub use emblems::{Emblem, EmblemTint};
pub use folder_trust::{
    FolderTrust, FolderTrustErr, FolderTrustGap, FolderTrustPreview, FolderTrustRow,
    folder_trust_rows, grant_folder_trust,
};
pub use headless::{
    CHECK_VERDICT_SCHEMA, HeadlessError, HeadlessErrorKind, HeadlessForm, HeadlessRequest,
    HeadlessResult, HeadlessVerdict,
};
pub use hook_types::{HookOutput, HookReply, HookRouting};
use identity::{RootIdentity, SubagentIdentity, resolve_root_identity, resolve_subagent_identity};
use lifecycle::LifecycleState;
pub(crate) use lifecycle::step;
pub use lifecycle::{
    AskKind, CONDITION_CHECKPOINT, DELIVERY_CHECKPOINT, LIFECYCLE_EVENT_VERSION, LifecycleEvent,
    LifecycleSignal, LifecycleSignalKind, LifecycleTransition, SignalSet, TurnPhase,
};
pub use locate::locate_binary;
use locate::{agent_config_path, probe_descriptor_version, read_optional_file};
pub use login::{
    AccountStatus, BirthLoginErr, LoginCatalog, LoginConfigErr, LoginSource, ProviderHomeLogin,
    ProviderLogin, RoomAccount, RoomAccounts, RoomLoginErr, RoomLoginSet, ambient_env,
    machine_login_catalog, provider_home_logins, resolve_room_accounts, room_account,
    room_accounts, session_login, session_login_env,
};
pub use managed_source::ManagedIntegration;
pub use observation::{
    AgentLifecycleObservation, AgentUsageSummary, AnsweredQuestion, AskQueueEdit, LaunchParams,
    LaunchRecord, LaunchedBy, QueuedQuestion, SessionOrigin, SpawnedSubagent, SubagentCorrelation,
    SubagentCorrelationInput, SubagentSpawnInput, TierPass, TierSkip, TierSkipReason, TierStamp,
    TierUnbuildable, WorkflowAttemptRef, WorkflowAttemptRole,
};
pub use open_ask::{AskRoute, OpenAskDetail, OpenAskReadErr, read_open_ask};
pub use park::ParkDemotion;
use payload::{CONTROL_TAG_PREFIXES, optional_payload_string, stop_payload_errored};
pub use payload::{RimzBlock, SanitizedPrompt, peel_rimz_blocks};
pub(crate) use payload::{non_empty_trimmed, sanitize_user_prompt};
pub use pricing::PriceBook;
use pricing::TokenSplit;
pub(crate) use registry::{
    ResumeProbe, resumed_session_id_for_root, resumed_session_id_from_cmdline,
};
pub use registry::{
    SoftCompactSkip, all_definitions, compact_command, definition_by_kind, find_definition,
    known_kinds, soft_compact_prompt, soft_compact_skip, spec_by_kind,
};
pub use spending::{HeadlineSpec, SpendTally, SpendWindow, SpendWindowMode};
pub use state::{
    ATTENTION_AGE_CEILING_SECS, AgentCardRef, AgentState, AgentStatus, AskDelivery, BudgetPark,
    BudgetScope, BudgetWindow, COMPACTING_WINDOW_SECS, ContextSeverity, IdleStop, OpenAsk,
    PendingIdleStop, PendingWait, PendingWaitClass, PendingWaitTrigger, QueuedAsk, TurnCompletion,
    is_stalled, is_tool_looping, is_turn_dead, settled_outcome, single_line_description,
};
pub(crate) use state::{display_turn_error, effective_turn_error_class, usable_description};
use tools::ToolErr;
pub(crate) use tools::{
    ToolNotice, canonical_tool_name, definition_defaults, render_launch_tool_args,
    render_tool_args, runs_other_providers_models, tools_required,
};
pub use tools::{ToolSet, definition_model_kind, skills_lack_skill_tool};
use transcript::{TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole};
use transcript_fs::read_transcript_lines;
use transcript_fs::{read_transcript_tail, read_transcript_tail_with_status};

pub mod plugins;
pub mod skills;

const ROUTINE_RIMZ_PREFIXES: &[&str] = &[
    "rimz message",
    "rimz agents",
    "rimz subagents",
    "rimz teams",
    "rimz asks",
    "rimz answer",
    "rimz wait --in",
    "rimz pane list",
    "rimz pane capture",
    "rimz loop show",
    "rimz loop logs",
    "rimz packs list",
    "rimz packs show",
    "rimz packs diff",
    "rimz lsp def",
    "rimz lsp refs",
    "rimz lsp hover",
    "rimz lsp impl",
    "rimz lsp callers",
    "rimz lsp callees",
    "rimz lsp symbols",
    "rimz lsp find",
    "rimz lsp check",
    "rimz lsp list",
    "rimz lsp status",
];

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
    WriteHookConfig(#[from] crate::disk::atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, AgentErr>;

/// Simulate Codex's Trust all for fixtures, excluding any named catalog events.
#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub fn trust_hook_config_for_test(path: &Path, excluded_events: &[&str]) -> Result<()> {
    adapters::codex::trust_hook_config_for_test(path, excluded_events)
}

/// Machine policy for provider-managed hook installation.
#[derive(Clone, Copy, Debug)]
pub struct HookInstallOptions {
    pub allow_routine_rimz: bool,
}

impl Default for HookInstallOptions {
    fn default() -> Self {
        Self {
            allow_routine_rimz: true,
        }
    }
}

/// A provider setting that prevents an explicitly requested capability.
#[derive(Clone, Debug)]
pub struct SettingsConflict {
    pub path: PathBuf,
    pub key: &'static str,
    pub value: Value,
    pub fix: String,
}

impl std::fmt::Display for SettingsConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}: {}; {}",
            self.path.display(),
            self.key,
            self.value,
            self.fix
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPreset {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Native auto-compaction window as a canonical decimal token count.
    pub auto_compact: Option<String>,
    /// Absolute path to a file whose contents replace the agent's base system
    /// prompt. Resolved and existence-checked by the launcher before render.
    pub system_prompt_file: Option<PathBuf>,
    /// Absolute paths composed in order into the replacement system prompt.
    /// A non-empty launch override replaces the profile's complete list.
    pub append_system_prompt_files: Vec<PathBuf>,
    /// Provider fast mode: `Some(true)` renders the adapter's fast switch,
    /// `Some(false)` removes a rendered one, `None` leaves argv as it is.
    pub fast: Option<bool>,
}

impl LaunchPreset {
    pub fn is_empty(&self) -> bool {
        self.model.as_deref().is_none_or(str::is_empty)
            && self.effort.as_deref().is_none_or(str::is_empty)
            && self.auto_compact.as_deref().is_none_or(str::is_empty)
            && self.system_prompt_file.is_none()
            && self.append_system_prompt_files.is_empty()
            && self.fast.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetField {
    Model,
    Effort,
    AutoCompact,
    SystemPromptFile,
}

impl PresetField {
    pub(crate) const fn flag_name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
            Self::AutoCompact => "auto-compact",
            Self::SystemPromptFile => "system-prompt-file",
        }
    }

    pub(crate) fn launch_preset(self, value: String) -> LaunchPreset {
        let mut preset = LaunchPreset::default();
        match self {
            Self::Model => preset.model = Some(value),
            Self::Effort => preset.effort = Some(value),
            Self::AutoCompact => preset.auto_compact = Some(value),
            Self::SystemPromptFile => preset.system_prompt_file = Some(value.into()),
        }
        preset
    }
}

/// How a launch-preset field reaches the agent's arguments or environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetArgMatcher {
    /// A single-use named flag: `--model VALUE` or `--model=VALUE`.
    Flag(Vec<String>),
    /// A single-use named flag whose value is prompt text rather than a path.
    TextFlag(Vec<String>),
    /// An environment variable whose value is a replacement file path.
    EnvPathVar(String),
    /// A config member carried in an environment variable, edited by the launch-env hook.
    EnvConfigMember { var: String, member: String },
    /// A repeatable config override carrying the field as `<flag> <key>=VALUE`.
    ConfigKey { flags: Vec<String>, key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresetArgOccurrence {
    pub(crate) argv_range: std::ops::Range<usize>,
    pub(crate) value: String,
}

impl PresetArgMatcher {
    /// Remove every occurrence of this typed setting from provider argv.
    ///
    /// Environment-only settings leave argv unchanged.
    pub fn remove_occurrences(&self, argv: &mut Vec<String>) {
        for occurrence in self.occurrences(argv).iter().rev() {
            argv.drain(occurrence.argv_range.clone());
        }
    }

    pub(crate) fn occurrences(&self, argv: &[String]) -> Vec<PresetArgOccurrence> {
        let mut occurrences = Vec::new();
        let mut index = 0;
        while index < argv.len() {
            let occurrence = match self {
                Self::Flag(flags) | Self::TextFlag(flags) => flags.iter().find_map(|flag| {
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
                Self::EnvPathVar(_) | Self::EnvConfigMember { .. } => None,
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
            Self::Flag(flags) | Self::TextFlag(flags) => flags
                .first()
                .map_or_else(|| value.to_owned(), |flag| format!("{flag} {value}")),
            Self::ConfigKey { key, .. } => format!("{key} {value}"),
            Self::EnvPathVar(key) => format!("{key}={value}"),
            Self::EnvConfigMember { var, member } => format!("{var} {member}={value}"),
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PresetErr {
    #[error(
        "agent `{agent}` does not support profile {field} value `{value}`; use a native value or remove it"
    )]
    UnsupportedValue {
        agent: &'static str,
        field: &'static str,
        value: String,
    },
    #[error(
        "agent `{agent}` does not support profile field `{field}`; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedField {
        agent: &'static str,
        field: &'static str,
    },
    #[error("agent `{agent}` has no fast-mode launch switch; {fix}")]
    UnsupportedFast {
        agent: &'static str,
        fix: &'static str,
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
    MuseChildThread,
}

impl HookIngressIgnoreReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeRemoteControl => "claude_remote_control",
            Self::CodexInternalAppServer => "codex_internal_app_server",
            Self::DroidStockTui => "droid_stock_tui",
            Self::MuseChildThread => "muse_child_thread",
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

/// One provider turn priced for a live-session accumulator.
#[derive(Clone, Debug, PartialEq)]
pub struct LocallyPricedTurnCost {
    pub turn_id: String,
    pub cost_usd: f64,
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
    /// Local transcript refresh on a producer pass, or an explicit card refresh.
    Tick,
    /// Filesystem watcher response to transcript growth.
    Watch,
}

/// Context for `ContextCapability::context_refresh_spawn`: the session and
/// workspace to refresh, plus the model hint its latest observation resolved.
/// On [`RefreshTrigger::Tick`] and [`RefreshTrigger::Watch`], callers pass
/// `server_url: None`.
pub struct LifecycleRefreshCtx<'a> {
    pub agent_id: &'a str,
    pub workspace_id: &'a crate::ids::WorkspaceId,
    pub model_hint: Option<&'a str>,
    pub server_url: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleRefreshRequest {
    pub kind: crate::ids::AgentKind,
    pub session_id: String,
    pub workspace_id: crate::ids::WorkspaceId,
    pub model: Option<String>,
    pub server_url: Option<String>,
}

/// The detached `rimz agents refresh-context` helper request: the one command
/// that runs `ContextCapability::refresh_session_context` for any provider.
fn refresh_context_argv(kind: &str, ctx: &LifecycleRefreshCtx<'_>) -> Vec<String> {
    crate::child_process::agent_helper_argv(
        "refresh-context",
        &LifecycleRefreshRequest {
            kind: crate::ids::AgentKind::new_unchecked(kind),
            session_id: ctx.agent_id.to_owned(),
            workspace_id: ctx.workspace_id.clone(),
            model: ctx.model_hint.map(str::to_owned),
            server_url: ctx.server_url.map(str::to_owned),
        },
    )
}

/// Stat gate for a bounded transcript, rollout, or telemetry tail read. Producers persist it
/// beside the sidecar so a high-frequency hook can stat-gate local enrichment
/// before reading the tail again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptStat {
    pub mtime_secs: i64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub mtime_nanos: u32,
    pub len: u64,
    /// Provider-owned companion files whose bytes participate in the same local
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
    /// Last absorbed response group, retained across suffix parses so
    /// contiguous duplicate rows are skipped or replace their earlier forms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    last_requests: Vec<FoldedRequest>,
    #[serde(default, skip_serializing_if = "is_false")]
    dedup_window_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FoldedRequest {
    key: Option<(String, Option<String>)>,
    group: Option<FoldedRequestGroup>,
    cost_usd: f64,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    has_speed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum FoldedRequestGroup {
    Request(String),
    Message(String),
}

impl LocalSpendFold {
    /// Resume a persisted fold when its cursor and duplicate window are valid.
    /// Folds written before the window existed replay once from byte zero.
    fn resume(prior: Option<&Self>, file_len: u64) -> Self {
        let fold = prior.cloned().unwrap_or_default();
        if fold.cursor.offset > file_len || fold.needs_dedup_replay() {
            return Self::default();
        }
        fold
    }

    fn absorb(&mut self, entries: &[spending::CachedEntry]) {
        self.dedup_window_ready = true;
        for entry in entries {
            let request = FoldedRequest::from(entry);
            let Some(group) = request.group.as_ref() else {
                self.last_requests.clear();
                self.add(&request);
                continue;
            };
            if self
                .last_requests
                .first()
                .and_then(|previous| previous.group.as_ref())
                != Some(group)
            {
                self.last_requests.clear();
            }
            if let Some(index) = self
                .last_requests
                .iter()
                .position(|previous| previous.key == request.key)
            {
                let previous = &self.last_requests[index];
                if spending::should_replace_usage_duplicate(
                    request.token_total(),
                    request.has_speed,
                    previous.token_total(),
                    previous.has_speed,
                ) {
                    let previous = previous.clone();
                    self.subtract(&previous);
                    self.add(&request);
                    self.last_requests[index] = request;
                }
                continue;
            }
            self.add(&request);
            self.last_requests.push(request);
        }
    }

    fn needs_dedup_replay(&self) -> bool {
        self.cursor.offset > 0 && !self.dedup_window_ready
    }

    fn add(&mut self, request: &FoldedRequest) {
        self.total_usd += request.cost_usd;
        self.input = self.input.saturating_add(request.input);
        self.output = self.output.saturating_add(request.output);
        self.cache_write = self.cache_write.saturating_add(request.cache_write);
        self.cache_read = self.cache_read.saturating_add(request.cache_read);
    }

    fn subtract(&mut self, request: &FoldedRequest) {
        self.total_usd -= request.cost_usd;
        self.input = self.input.saturating_sub(request.input);
        self.output = self.output.saturating_sub(request.output);
        self.cache_write = self.cache_write.saturating_sub(request.cache_write);
        self.cache_read = self.cache_read.saturating_sub(request.cache_read);
    }

    fn session_usage(&self) -> Option<AgentSessionUsage> {
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

impl From<&spending::CachedEntry> for FoldedRequest {
    fn from(entry: &spending::CachedEntry) -> Self {
        Self {
            key: entry.message_id.clone().map(|message_id| {
                (
                    message_id.into_string(),
                    entry.request_id.clone().map(String::from),
                )
            }),
            group: entry.message_id.as_deref().map(|message_id| {
                entry.request_id.clone().map(String::from).map_or_else(
                    || {
                        FoldedRequestGroup::Message(
                            advisor_parent_message_id(message_id).to_owned(),
                        )
                    },
                    FoldedRequestGroup::Request,
                )
            }),
            cost_usd: entry.cost_usd,
            input: entry.input,
            output: entry.output,
            cache_write: entry.cache_write,
            cache_read: entry.cache_read,
            has_speed: entry.has_speed,
        }
    }
}

fn advisor_parent_message_id(message_id: &str) -> &str {
    message_id
        .rsplit_once(":advisor:")
        .filter(|(_, index)| index.parse::<usize>().is_ok())
        .map_or(message_id, |(parent, _)| parent)
}

impl FoldedRequest {
    fn token_total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }
}

impl TranscriptStat {
    /// Read the stat gate of a transcript-like file.
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
    fn newest_mtime_secs(&self) -> u64 {
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

fn is_false(value: &bool) -> bool {
    !*value
}

/// Context for `ContextCapability::local_context_refresh`: the session to refresh,
/// its current model hint, and the local-source gate state from the latest
/// sidecar.
pub struct LocalContextRefreshCtx<'a> {
    /// The env the provider home resolves from; the ambient env for the default account.
    pub login_env: &'a BTreeMap<String, String>,
    pub agent_id: &'a str,
    pub model_hint: Option<&'a str>,
    /// Stored name used to suppress unchanged local title writes.
    pub prior_session_name: Option<&'a str>,
    /// Transcript path carried by the current hook payload, if any.
    pub current_transcript_path: Option<&'a str>,
    pub prior_transcript_path: Option<&'a str>,
    pub prior_transcript_stat: Option<&'a TranscriptStat>,
    pub prior_spend_fold: Option<&'a LocalSpendFold>,
    /// Persistent shared `pricing-cache.json`, for adapters that price token
    /// counts into card dollars. The spending producer owns writes.
    pub shared_pricing_cache_path: &'a Path,
}

impl LocalContextRefreshCtx<'_> {
    fn changed_transcript(&self, stat: TranscriptStat) -> Option<TranscriptStat> {
        (self.prior_transcript_stat != Some(&stat)).then_some(stat)
    }
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
    /// A current local snapshot owns the latest-turn attention markers and
    /// reports an absent token reading while preserving an established gauge.
    fn authoritative_current() -> Self {
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
    /// The account whose home the session was discovered in; `None` is the
    /// provider's own `default` home, as on [`AgentState::login`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<crate::ids::LoginName>,
}

/// A detached `rimz` helper an adapter requests after a lifecycle event lands
/// — just the argv. [`crate::harness::hook_drain::spawn_refresh_detached`]
/// owns the spawn discipline (fresh, fully-nulled stdio; fire-and-forget), so
/// adapters stay pure mappers.
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
    #[error("hooks disabled: {reason}")]
    HooksDisabled { reason: String, fix: String },
}

/// Check the hook installation, trust, and requested turn-lifecycle coverage
/// needed before a hook-driven operation starts.
pub fn preflight_hooks(
    adapter: &AgentDefinition,
    login_env: &BTreeMap<String, String>,
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
    let integration = adapter.managed_integration();
    if let Some(disabled) =
        integration.and_then(|integration| integration.hooks_disabled(login_env))
    {
        return Err(disabled);
    }
    if !adapter.hooks_installed(login_env) {
        return Err(HookPreflightErr::HooksMissing);
    }
    let untrusted = integration.map_or_else(Vec::new, |integration| {
        integration.untrusted_preflight_hooks(login_env)
    });
    if !untrusted.is_empty() {
        return Err(HookPreflightErr::HooksUntrusted {
            hooks: untrusted.join(", "),
            fix: hook_trust_fix(adapter.spec().kind),
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("{kind} {reason} for `{}`: {fix}", dir.display())]
pub struct LaunchDirUntrusted {
    pub kind: &'static str,
    pub dir: PathBuf,
    pub fix: String,
    reason: &'static str,
}

/// Why an unattended run may not start in its working directory. The home
/// case carries no fix: only the entry knows how its user moves the run.
#[derive(Debug, thiserror::Error)]
pub enum LaunchDirRefusal {
    #[error("`{}` is the home directory, where an unattended agent cannot be started", dir.display())]
    Home { dir: PathBuf },
    #[error(transparent)]
    Untrusted(#[from] LaunchDirUntrusted),
}

/// Check the working directory for supervised panes or headless execution.
/// The login's `$HOME` is refused for every kind, ahead of folder trust: no
/// one answers an unattended run's trust prompt, and Claude never saves trust
/// there.
pub fn preflight_launch_dir(
    adapter: &AgentDefinition,
    cwd: &Path,
    repo_root: Option<&Path>,
    login_env: &BTreeMap<String, String>,
    mode: LaunchDirMode,
) -> std::result::Result<(), LaunchDirRefusal> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.into());
    let home = login_env.get("HOME").filter(|home| !home.is_empty());
    if home.is_some_and(|home| canonical(Path::new(home)) == canonical(cwd)) {
        return Err(LaunchDirRefusal::Home {
            dir: cwd.to_path_buf(),
        });
    }
    if mode == LaunchDirMode::Headless && !adapter.headless_requires_folder_trust() {
        return Ok(());
    }
    match adapter.folder_trust(cwd, repo_root, login_env) {
        Some(FolderTrust::Undecided(gap)) => Err(LaunchDirUntrusted {
            kind: adapter.spec().kind,
            dir: cwd.to_path_buf(),
            fix: if crate::disk::paths::is_under_root(cwd, &crate::disk::paths::empty_dir_root()) {
                gap.empty_dir_fix(adapter.spec().kind)
            } else {
                gap.fix(adapter.spec().kind, "rimz trust grant")
            },
            reason: "has no trust decision and would stop at its trust prompt",
        }.into()),
        Some(FolderTrust::Restricted { key }) if mode == LaunchDirMode::Supervised => {
            Err(LaunchDirUntrusted {
                kind: adapter.spec().kind,
                dir: key,
                fix: format!(
                    "{}; RimZ leaves the saved decision alone",
                    FolderTrust::RESTRICTED_FIX
                ),
                reason: "is saved untrusted and will stop at its Folder access prompt on every launch",
            }.into())
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchDirMode {
    Supervised,
    Headless,
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
    registry::find_definition(kind).map_or_else(
        || format!("run /hooks inside {kind} and trust the RimZ hooks"),
        |adapter| adapter.hook_trust_fix(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unattended_home_refuses_every_builtin_in_both_modes() {
        let home = tempfile::tempdir().unwrap();
        let login_env = BTreeMap::from([("HOME".into(), home.path().display().to_string())]);
        for adapter in registry::BUILTINS {
            for mode in [LaunchDirMode::Supervised, LaunchDirMode::Headless] {
                assert!(
                    matches!(
                        preflight_launch_dir(adapter, home.path(), None, &login_env, mode),
                        Err(LaunchDirRefusal::Home { dir }) if dir == home.path()
                    ),
                    "{} {mode:?} must refuse home",
                    adapter.spec().kind
                );
            }
        }
    }

    #[test]
    fn unattended_home_refuses_canonical_aliases() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let alias = root.path().join("alias");
        std::fs::create_dir(&home).unwrap();
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let adapter = definition_by_kind("claude").unwrap();
        for (cwd, home) in [(&alias, &home), (&home, &alias), (&home.join("."), &home)] {
            let login_env = BTreeMap::from([("HOME".into(), home.display().to_string())]);
            assert!(matches!(
                preflight_launch_dir(adapter, cwd, None, &login_env, LaunchDirMode::Supervised),
                Err(LaunchDirRefusal::Home { .. })
            ));
        }
    }

    #[test]
    fn unattended_home_allows_subdirectories_and_missing_home() {
        let home = tempfile::tempdir().unwrap();
        let sub = home.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        for adapter in registry::BUILTINS {
            for mode in [LaunchDirMode::Supervised, LaunchDirMode::Headless] {
                let mut login_env =
                    BTreeMap::from([("HOME".into(), home.path().display().to_string())]);
                assert!(!matches!(
                    preflight_launch_dir(adapter, &sub, Some(home.path()), &login_env, mode),
                    Err(LaunchDirRefusal::Home { .. })
                ));
                login_env.remove("HOME");
                assert!(!matches!(
                    preflight_launch_dir(adapter, home.path(), None, &login_env, mode),
                    Err(LaunchDirRefusal::Home { .. })
                ));
                login_env.insert("HOME".into(), String::new());
                assert!(!matches!(
                    preflight_launch_dir(adapter, home.path(), None, &login_env, mode),
                    Err(LaunchDirRefusal::Home { .. })
                ));
            }
        }
    }

    #[test]
    fn unattended_home_refuses_even_when_codex_trusts_it() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        let mut table = toml::Table::new();
        table.insert(
            "projects".into(),
            toml::Value::Table(toml::Table::from_iter([(
                home.path().display().to_string(),
                toml::Value::Table(toml::Table::from_iter([(
                    "trust_level".into(),
                    toml::Value::String("trusted".into()),
                )])),
            )])),
        );
        std::fs::write(&config, toml::to_string(&table).unwrap()).unwrap();
        let login_env = BTreeMap::from([
            ("HOME".into(), home.path().display().to_string()),
            ("RIMZ_CODEX_CONFIG".into(), config.display().to_string()),
        ]);
        let adapter = definition_by_kind("codex").unwrap();
        assert!(matches!(
            adapter.folder_trust(home.path(), None, &login_env),
            Some(FolderTrust::Decided)
        ));
        for mode in [LaunchDirMode::Supervised, LaunchDirMode::Headless] {
            assert!(matches!(
                preflight_launch_dir(adapter, home.path(), None, &login_env, mode),
                Err(LaunchDirRefusal::Home { .. })
            ));
        }
    }

    #[test]
    fn adapter_hook_trust_fix_preserves_codex_instructions() {
        let codex = registry::definition_by_kind("codex").unwrap();
        assert_eq!(
            codex.hook_trust_fix(),
            "run /hooks inside codex and trust the RimZ hooks"
        );
        assert_eq!(hook_trust_fix("codex"), codex.hook_trust_fix());
    }

    #[test]
    fn hook_trust_choice_requires_provider_opt_in() {
        struct Unregistered;
        impl capabilities::CoreCapability for Unregistered {
            fn spec(&self) -> &'static AgentSpec {
                unreachable!("the default choice does not need a provider spec")
            }
        }
        impl capabilities::InstallationCapability for Unregistered {}
        assert_eq!(
            capabilities::InstallationCapability::hook_trust_choice(&Unregistered),
            None
        );
        assert_eq!(
            registry::definition_by_kind("hermes")
                .unwrap()
                .hook_trust_choice(),
            Some("answer y to each RimZ hook prompt")
        );
        assert_eq!(
            registry::definition_by_kind("codex")
                .unwrap()
                .hook_trust_choice(),
            Some("choose \"Trust all and continue\"")
        );
    }

    fn local_context_refresh_ctx(
        prior_transcript_stat: Option<&TranscriptStat>,
    ) -> LocalContextRefreshCtx<'_> {
        static LOGIN_ENV: BTreeMap<String, String> = BTreeMap::new();
        LocalContextRefreshCtx {
            login_env: &LOGIN_ENV,
            agent_id: "session",
            model_hint: None,
            prior_session_name: None,
            current_transcript_path: None,
            prior_transcript_path: None,
            prior_transcript_stat,
            prior_spend_fold: None,
            shared_pricing_cache_path: Path::new("pricing.json"),
        }
    }

    fn spend_entry(key: Option<(&str, &str)>, input: u64, cost_usd: f64) -> spending::CachedEntry {
        spending::CachedEntry {
            input,
            cost_usd,
            message_id: key.map(|(message_id, _)| message_id.into()),
            request_id: key.map(|(_, request_id)| request_id.into()),
            ..spending::CachedEntry::default()
        }
    }

    #[test]
    fn local_spend_fold_replaces_richer_duplicates_and_keeps_keyless_rows() {
        let entries = [
            spend_entry(Some(("message-1", "request-1")), 10, 1.0),
            spend_entry(Some(("message-1", "request-1")), 12, 2.0),
            spend_entry(Some(("message-1", "request-1")), 12, 3.0),
            spend_entry(None, 4, 4.0),
            spend_entry(None, 5, 5.0),
            spend_entry(Some(("message-2", "request-2")), 6, 6.0),
        ];
        let mut fold = LocalSpendFold::default();

        fold.absorb(&entries);

        assert_eq!(fold.input, 27);
        assert_eq!(fold.total_usd, 17.0);
        assert_eq!(
            fold.last_requests
                .first()
                .and_then(|request| request.key.as_ref()),
            Some(&("message-2".to_owned(), Some("request-2".to_owned())))
        );
    }

    #[test]
    fn local_spend_fold_resume_replays_legacy_and_truncated_folds() {
        let legacy = LocalSpendFold {
            cursor: spending::SpendCursor::new(10),
            input: 99,
            ..LocalSpendFold::default()
        };
        assert_eq!(
            LocalSpendFold::resume(Some(&legacy), 10),
            LocalSpendFold::default()
        );

        let mut current = LocalSpendFold::default();
        current.absorb(&[spend_entry(Some(("message-1", "request-1")), 10, 1.0)]);
        current.cursor.offset = 10;
        assert_eq!(LocalSpendFold::resume(Some(&current), 10), current);
        assert_eq!(
            LocalSpendFold::resume(Some(&current), 9),
            LocalSpendFold::default()
        );

        let mut empty = LocalSpendFold::default();
        empty.absorb(&[]);
        empty.cursor.offset = 10;
        assert_eq!(LocalSpendFold::resume(Some(&empty), 10), empty);
    }

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
    fn changed_transcript_accepts_stat_without_prior() {
        let stat = TranscriptStat::default();

        assert_eq!(
            local_context_refresh_ctx(None).changed_transcript(stat),
            Some(stat)
        );
    }

    #[test]
    fn changed_transcript_accepts_changed_stat() {
        let prior = TranscriptStat::default();
        let changed = TranscriptStat {
            len: 1,
            ..TranscriptStat::default()
        };

        assert_eq!(
            local_context_refresh_ctx(Some(&prior)).changed_transcript(changed),
            Some(changed)
        );
    }

    #[test]
    fn changed_transcript_rejects_equal_stat() {
        let stat = TranscriptStat::default();

        assert_eq!(
            local_context_refresh_ctx(Some(&stat)).changed_transcript(stat),
            None
        );
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
