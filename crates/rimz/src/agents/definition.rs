//! Composed identity and workflow capabilities for one agent integration.
//!
//! One `const` [`AgentSpec`] per private adapter directory; the registry
//! composes it with selected workflow capability objects in an
//! [`AgentDefinition`]. Immutable identity, presentation, process, and launch
//! facts live here. Native parsing and provider mechanics stay in capability
//! implementations behind the adapters boundary.

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::lifecycle::{AskKind, LifecycleSignalKind};
use super::{LaunchPreset, PresetArgMatcher, PresetErr, PresetField};
use crate::harness::run::PermissionMode;

/// Static launch shapes shared by built-in and process-plugin adapters.
#[derive(Clone, Copy, Debug)]
pub struct LaunchSpec {
    pub program: Option<&'static str>,
    pub fixed_args: &'static [&'static str],
    pub prompt: PromptStyle,
    pub resume: Option<SessionCommand>,
    pub fork: Option<SessionCommand>,
    pub permission: LaunchPermissionArgs,
    pub max_turn_flag: Option<&'static str>,
    pub compact_command: Option<&'static str>,
    pub presets: PresetMatchers,
}

impl LaunchSpec {
    pub const EMPTY: Self = Self {
        program: None,
        fixed_args: &[],
        prompt: PromptStyle::None,
        resume: None,
        fork: None,
        permission: LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        compact_command: None,
        presets: PresetMatchers::EMPTY,
    };

    /// Render argv that forks a prior session under a provider-assigned new id.
    pub fn fork_command(self, session_id: &str) -> Option<Vec<String>> {
        self.fork.map(|command| command.render(session_id))
    }

    /// Render extra launch argv for one supervised permission posture.
    pub fn permission_args(self, mode: PermissionMode) -> Vec<String> {
        let args = match mode {
            PermissionMode::Ask => self.permission.ask,
            PermissionMode::Auto => self.permission.auto,
            PermissionMode::Yolo => self.permission.yolo,
            PermissionMode::Plan => self.permission.plan,
        };
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// Render the native supervised-turn cap, when supported.
    pub fn max_turns_args(self, limit: u32) -> Option<Vec<String>> {
        self.max_turn_flag
            .map(|flag| vec![flag.to_owned(), limit.to_string()])
    }

    /// Return the interactive command for manual context compaction.
    pub const fn compact_command(self) -> Option<&'static str> {
        self.compact_command
    }

    /// Describe the provider-native argv spelling rendered for a preset field.
    pub fn preset_arg_matcher(self, field: PresetField) -> Option<PresetArgMatcher> {
        let matcher = match field {
            PresetField::Model => self.presets.model,
            PresetField::Effort => self.presets.effort,
            PresetField::SystemPromptFile => self.presets.system_prompt_file,
        }?;
        Some(match matcher {
            StaticPresetMatcher::Flag(flags) => {
                PresetArgMatcher::Flag(flags.iter().map(|flag| (*flag).to_owned()).collect())
            }
            StaticPresetMatcher::TextFlag(flags) => {
                PresetArgMatcher::TextFlag(flags.iter().map(|flag| (*flag).to_owned()).collect())
            }
            StaticPresetMatcher::ConfigKey { flags, key } => PresetArgMatcher::ConfigKey {
                flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
                key: key.to_owned(),
            },
        })
    }

    fn render_preset(
        self,
        agent_kind: &'static str,
        preset: &LaunchPreset,
    ) -> Result<Vec<String>, PresetErr> {
        let values: [(PresetField, &'static str, Option<String>); 2] = [
            (
                PresetField::Model,
                "model",
                preset.model.clone().filter(|value| !value.is_empty()),
            ),
            (
                PresetField::Effort,
                "effort",
                preset.effort.clone().filter(|value| !value.is_empty()),
            ),
        ];
        let mut argv = Vec::new();
        for (field, field_name, value) in values {
            let Some(value) = value else { continue };
            let matcher = self
                .preset_arg_matcher(field)
                .ok_or(PresetErr::UnsupportedField {
                    agent: agent_kind,
                    field: field_name,
                })?;
            match matcher {
                PresetArgMatcher::Flag(flags) | PresetArgMatcher::TextFlag(flags) => {
                    let flag = flags.first().ok_or(PresetErr::UnsupportedField {
                        agent: agent_kind,
                        field: field_name,
                    })?;
                    argv.extend([flag.clone(), value]);
                }
                PresetArgMatcher::ConfigKey { flags, key } => {
                    let flag = flags.first().ok_or(PresetErr::UnsupportedField {
                        agent: agent_kind,
                        field: field_name,
                    })?;
                    argv.extend([flag.clone(), format!("{key}={value}")]);
                }
            }
        }
        Ok(argv)
    }

    pub(super) fn resume_command(self, session_id: &str) -> Option<Vec<String>> {
        self.resume.map(|command| command.render(session_id))
    }

    pub(super) fn launch_command(
        self,
        extra_args: &[String],
        prompt: Option<&str>,
    ) -> Option<Vec<String>> {
        let mut argv = vec![self.program?.to_owned()];
        argv.extend(self.fixed_args.iter().map(|arg| (*arg).to_owned()));
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            match self.prompt {
                PromptStyle::None => {}
                PromptStyle::PositionalAfterDoubleDash => {
                    argv.extend(["--".to_owned(), prompt.to_owned()]);
                }
                PromptStyle::Flag(flag) => {
                    argv.extend([flag.to_owned(), prompt.to_owned()]);
                }
                PromptStyle::FlagWithSuffix { flag, suffix } => {
                    argv.extend([flag.to_owned(), prompt.to_owned()]);
                    argv.extend(suffix.iter().map(|arg| (*arg).to_owned()));
                }
            }
        }
        Some(argv)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionCommand {
    pub before_id: &'static [&'static str],
    pub after_id: &'static [&'static str],
}

impl SessionCommand {
    fn render(self, session_id: &str) -> Vec<String> {
        self.before_id
            .iter()
            .copied()
            .chain(std::iter::once(session_id))
            .chain(self.after_id.iter().copied())
            .map(ToOwned::to_owned)
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PromptStyle {
    None,
    PositionalAfterDoubleDash,
    Flag(&'static str),
    FlagWithSuffix {
        flag: &'static str,
        suffix: &'static [&'static str],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct LaunchPermissionArgs {
    pub ask: &'static [&'static str],
    pub auto: &'static [&'static str],
    pub yolo: &'static [&'static str],
    pub plan: &'static [&'static str],
}

impl LaunchPermissionArgs {
    pub const EMPTY: Self = Self {
        ask: &[],
        auto: &[],
        yolo: &[],
        plan: &[],
    };
}

#[derive(Clone, Copy, Debug)]
pub struct PresetMatchers {
    pub model: Option<StaticPresetMatcher>,
    pub effort: Option<StaticPresetMatcher>,
    pub system_prompt_file: Option<StaticPresetMatcher>,
}

impl PresetMatchers {
    pub const EMPTY: Self = Self {
        model: None,
        effort: None,
        system_prompt_file: None,
    };
}

#[derive(Clone, Copy, Debug)]
pub enum StaticPresetMatcher {
    Flag(&'static [&'static str]),
    TextFlag(&'static [&'static str]),
    ConfigKey {
        flags: &'static [&'static str],
        key: &'static str,
    },
}

/// Provider-identity check for a [`bin_names`](AgentSpec::bin_names) entry that
/// another provider's installer also ships under the same filename. Cursor's
/// `agent` collides with the alias Grok's installer symlinks onto `$PATH`, so a
/// bare filename match cannot prove the located binary is Cursor's.
#[derive(Clone, Copy, Debug)]
pub struct BinIdentity {
    /// The subset of [`bin_names`](AgentSpec::bin_names) that collide with
    /// another provider's install alias. A candidate matched by one of these is
    /// accepted only after `verify` confirms it; every other name matches on
    /// filename alone.
    pub ambiguous: &'static [&'static str],
    /// Confirms a candidate is genuinely this provider from its `--version`
    /// stdout and stderr. The adapter's own version parser recognizes only its
    /// own release banner, so a colliding alias fails the check and discovery
    /// skips it.
    pub verify: fn(&str, &str) -> bool,
}

/// Static identity, branding, capabilities, and classification tables for one
/// agent. See the module doc for the definition-vs-trait split.
#[derive(Debug)]
pub struct AgentSpec {
    /// The stable kind string — the `--source` tag, the per-provider bucket
    /// key, the rollup `kind`.
    pub kind: &'static str,
    /// Alternate source spellings accepted by registry lookup.
    pub aliases: &'static [&'static str],
    /// Human display name; the provider dashboard panel title.
    pub display_name: &'static str,
    /// Brand emblem + color for the provider dashboard panel.
    pub brand: Brand,
    /// How a raw plan tier becomes a brand label (`max` → `Claude Max`).
    pub plan_label: PlanLabel,
    /// Subscription provider ids whose account budget this agent meters, as a
    /// multi-provider client's auth file names them (Pi's `auth.json` keys:
    /// `anthropic`, `openai`, …). Used for account labeling and
    /// provider-specific probes.
    pub sub_providers: &'static [&'static str],
    /// Budget-window labels a metered account of this kind reports, ordered
    /// short-to-long and matching the rendered `window_label` form (`5h`,
    /// `7d`). The dashboard paints these as placeholder tracks before the
    /// first reading; empty when the shape is unknown or unstable.
    pub expected_windows: &'static [&'static str],
    /// Tool-name classification tables for lifecycle and native blocking prompts.
    pub tools: ToolClassification,
    /// What this agent can and cannot do — consumed by the sidebar and doctor
    /// so a missing surface renders as a declared absence, never an
    /// accidental gap.
    pub capabilities: Capabilities,
    /// Declared integration checklist. Every [`IntegrationConcern`] appears
    /// exactly once as wired, partial, or unsupported, and conformance tests
    /// cross-check the declaration against the definition and classification
    /// corpus.
    pub coverage: CoverageAnnotations,
    /// Declared user-facing capability checklist — the six marks the
    /// compatibility matrix prints. Every [`UserCapability`] appears exactly
    /// once as full, partial, or unsupported, phrased in what the user sees;
    /// conformance cross-checks each mark against the concerns backing it.
    pub user_coverage: UserCoverage,
    /// Declared lifecycle-hook checklist. Every [`LifecycleSignalKind`] appears
    /// exactly once as native, derived, or absent; conformance checks the
    /// native event names against the installed hook events and classification
    /// corpus.
    pub lifecycle_hooks: LifecycleAnnotations,
    /// Provider-owned fallback for the model context window shown in an agent
    /// card before a richer runtime source reports the exact value.
    pub default_context_window: Option<u64>,
    /// Provider-owned default model slug. Used as the idle-row display
    /// fallback before a wired agent reports a session model and as the launch
    /// `--model` default when `rimz agents` has no configured model.
    pub default_model: Option<&'static str>,
    /// Process names this agent's instance can run under — its own `comm`
    /// plus any launcher (`node` for a JS bundle). Drives the PID-attribution
    /// `/proc` walk.
    pub process_names: &'static [&'static str],
    /// `$PATH` probe names in preference order. Most agents expose the bare
    /// kind; Cursor ships its CLI as `cursor-agent`/`agent` while `cursor` is
    /// the IDE.
    pub bin_names: &'static [&'static str],
    /// Identity check that keeps an ambiguous [`bin_names`](Self::bin_names)
    /// entry from resolving to a different provider's colliding install alias —
    /// Cursor's generic `agent` collides with the alias Grok's installer
    /// symlinks onto `$PATH`. `None` when every `bin_names` entry is
    /// provider-unique and a filename match settles identity.
    pub bin_identity: Option<BinIdentity>,
    /// Well-known install directories, relative to `$HOME`, where this agent's
    /// binary lives when its installer has not put
    /// it on `$PATH` — OpenCode's installer drops `opencode` in `~/.opencode/bin`
    /// and edits a shell rc the daemon never sources. Searched after `$PATH` by
    /// [`locate_binary`](super::locate_binary); empty for an agent that only
    /// ever ships on `$PATH`.
    pub extra_bin_dirs: &'static [&'static str],
    /// How this agent's transcript files map to billing threads, for the
    /// spending session count.
    pub thread_key: ThreadKey,
    /// Static process launch contract and its argv renderers.
    pub launch: LaunchSpec,
}

/// The provider-neutral registry value for one agent integration.
///
/// Callers resolve a definition and use its workflow methods; the provider
/// implementation remains an internal detail of the catalog.
pub struct AgentDefinition {
    core: &'static dyn super::capabilities::AgentIntegration,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid agent definition `{kind}`: {reason}")]
pub struct DefinitionValidationError {
    kind: &'static str,
    reason: String,
}

impl AgentDefinition {
    pub const fn new(core: &'static dyn super::capabilities::AgentIntegration) -> Self {
        Self { core }
    }

    pub fn spec(&self) -> &'static AgentSpec {
        self.core.spec()
    }

    pub fn validate(&self) -> Result<(), DefinitionValidationError> {
        let spec = self.spec();
        let invalid = |reason: String| DefinitionValidationError {
            kind: spec.kind,
            reason,
        };
        if spec.kind.trim().is_empty() {
            return Err(invalid("kind is empty".to_owned()));
        }
        if spec.display_name.trim().is_empty() {
            return Err(invalid("display name is empty".to_owned()));
        }
        let mut aliases = std::collections::BTreeSet::new();
        for alias in spec.aliases {
            if alias.trim().is_empty() || *alias == spec.kind || !aliases.insert(*alias) {
                return Err(invalid(format!("invalid or duplicate alias `{alias}`")));
            }
        }
        if let Some(tool) = spec
            .tools
            .editing
            .iter()
            .find(|tool| !spec.tools.mutating.contains(tool))
        {
            return Err(invalid(format!(
                "editing tool `{tool}` is absent from the mutating set"
            )));
        }
        Ok(())
    }

    pub fn concern_coverage(&self, concern: IntegrationConcern) -> ConcernCoverage {
        self.core.spec().coverage.get(concern)
    }

    pub fn lifecycle_coverage(&self, signal: LifecycleSignalKind) -> HookCoverage {
        self.core.spec().lifecycle_hooks.get(signal)
    }

    pub fn capability_level(&self, capability: UserCapability) -> CapabilityLevel {
        self.core.spec().user_coverage.get(capability)
    }
}

/// Every capability method reaches the adapter directly. The trait default in
/// [`super::capabilities`] is the single home for "this agent does not do that".
impl std::ops::Deref for AgentDefinition {
    type Target = dyn super::capabilities::AgentIntegration;

    fn deref(&self) -> &Self::Target {
        self.core
    }
}

impl std::fmt::Debug for AgentDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentDefinition")
            .field("kind", &self.spec().kind)
            .finish_non_exhaustive()
    }
}

impl AgentSpec {
    /// Render typed launch profile presets into provider-native argv.
    pub fn render_preset(&self, preset: &LaunchPreset) -> Result<Vec<String>, PresetErr> {
        self.launch.render_preset(self.kind, preset)
    }
}

/// Architecture tokens that open a Rust target triple, so a release binary
/// named `<kind>-<triple>` (`codex-aarch64-apple-darwin`) still reads as
/// `<kind>`. Matching the arch, not the full triple, stays correct under the
/// kernel's 15-char `comm` truncation (`codex-aarch64-a`), where the arch fits
/// and the vendor/os tail does not.
const TARGET_ARCHES: &[&str] = &[
    "x86_64",
    "aarch64",
    "arm64",
    "armv7",
    "arm",
    "i686",
    "i386",
    "riscv64",
    "powerpc64",
    "powerpc",
    "s390x",
    "loongarch64",
];

/// Whether a program `comm`/argv0 basename names `kind`: the bare kind, or the
/// kind under a target-triple release-binary suffix (`codex-aarch64-apple-darwin`,
/// or its `comm`-truncated `codex-aarch64-a`).
pub fn program_names_kind(name: &str, kind: &str) -> bool {
    if name == kind {
        return true;
    }
    let Some(rest) = name
        .strip_prefix(kind)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    TARGET_ARCHES.iter().any(|arch| {
        rest == *arch
            || rest
                .strip_prefix(arch)
                .is_some_and(|tail| tail.starts_with('-'))
    })
}

/// How a provider's transcript files map to billing threads (sessions), so the
/// spending pass counts one thread once however many files it spread across.
#[derive(Clone, Copy, Debug)]
pub enum ThreadKey {
    /// One transcript file per session — the file path is the thread.
    PerFile,
    /// One directory per session holding a main JSONL plus `subagents/*.jsonl`
    /// children — the session directory is the thread, so a subagent file
    /// folds under its parent session (Claude).
    SessionDir,
}

/// Brand styling for the provider dashboard panel.
#[derive(Debug)]
pub struct Brand {
    /// Optional plugin-supplied emblem override. Built-in agents resolve their
    /// art from the embedded emblem catalog by kind.
    pub emblem: Option<&'static str>,
    /// 256-color index.
    pub color: u8,
    /// Truecolor brand tone for renderers using RGB depth.
    pub color_rgb: (u8, u8, u8),
}

/// How a raw plan tier string becomes its brand label.
#[derive(Debug)]
pub enum PlanLabel {
    /// `"<prefix> <TitleCase(tier)>"` — Claude → "Claude Max",
    /// Codex → "ChatGPT Pro".
    Prefixed { prefix: &'static str },
    /// Just title-case the tier — for an agent whose sessions span many
    /// provider accounts, where no single brand prefix is honest.
    TitleCaseOnly,
}

impl PlanLabel {
    /// Format a raw provider tier in the provider's declared plan vocabulary.
    pub fn format(&self, raw: &str) -> String {
        let tier = crate::theme::provider_title_case(raw);
        match self {
            Self::Prefixed { prefix } => format!("{prefix} {tier}"),
            Self::TitleCaseOnly => tier,
        }
    }
}

/// The agent's tool vocabulary, classified for lifecycle and native blocking prompts.
#[derive(Debug)]
pub struct ToolClassification {
    /// Raw provider-payload key that holds a tool call's arguments. `None`
    /// when the protocol exposes no arguments RimZ can compare accurately.
    pub input_key: Option<&'static str>,
    /// Tools that mutate the workspace — write files or run commands. A
    /// mutating tool is proof of real work, so its completed tool signal is
    /// durable even when it does not change state.
    pub mutating: &'static [&'static str],
    /// The file-editing subset of `mutating` — the turn's first edit moves it
    /// from reasoning to acting. A shell tool mutates but does not edit, so a
    /// research turn that only runs commands keeps the thinking head.
    pub editing: &'static [&'static str],
    /// Tools whose pre-use hook is a blocking ask, paired with the ask kind
    /// they raise. Empty when the agent's blocking gate is an event, not a tool.
    pub blocking: &'static [(&'static str, AskKind)],
}

macro_rules! integration_concerns {
    ($($variant:ident => $label:literal),+ $(,)?) => {
        /// Product-level integration concerns every adapter declares explicitly.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum IntegrationConcern {
            $($variant),+
        }

        impl IntegrationConcern {
            pub const ALL: [Self; integration_concerns!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            pub const fn short_label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }
        }
    };
    (@count $($variant:ident),+ $(,)?) => {
        <[()]>::len(&[$(integration_concerns!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

integration_concerns! {
    TurnLifecycle => "turn",
    Permission => "perm",
    PlanApproval => "plan",
    UserQuestion => "ask",
    Answer => "answer",
    Compaction => "compact",
    Subagents => "sub",
    BackgroundParking => "bg",
    SessionEnd => "end",
    IdleNotification => "idle",
    ContextUsage => "usage",
    RealtimeCost => "live$",
    RichContext => "rich",
    HookInstall => "install",
    AccountSpend => "spend",
    ToolStats => "tools",
    RemoteControl => "remote",
}

/// How an adapter covers a concern: a native signal carries it directly,
/// derivation reconstructs it where the native signal is absent, or it is
/// unreachable from the current protocol surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConcernCoverage {
    /// The concern reaches a user-complete state; `via` names the path. This is
    /// a native signal that carries it directly, or a value RimZ reconciles to
    /// its authoritative figure at each turn boundary so the surface is visually
    /// full — complete to the user even without a continuous native push (for
    /// example the realtime-cost dollar, settled to the session spend sum every
    /// turn). Reserve `Partial` for a surface the user can still see is missing
    /// something between reconciliations.
    Wired { via: &'static str },
    /// Native coverage is incomplete, but RimZ reconstructs the behaviour from
    /// another signal or state: `via` is the combined path, `gap` what it still
    /// lacks.
    Partial {
        via: &'static str,
        gap: &'static str,
    },
    /// Unreachable from the current protocol surface, by any inference; `reason`
    /// says why.
    Unsupported { reason: &'static str },
}

impl ConcernCoverage {
    pub const fn is_wired(self) -> bool {
        matches!(self, Self::Wired { .. })
    }

    /// The reason-like text: the via for wired, the gap for partial, the
    /// unsupported reason — what the matrix prints after the concern label.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Wired { via } => via,
            Self::Partial { gap, .. } => gap,
            Self::Unsupported { reason } => reason,
        }
    }
}

/// How an adapter covers a lifecycle signal: a native event carries it directly,
/// derivation reconstructs it where the native event is absent, or the agent
/// cannot produce the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookCoverage {
    /// A native event carries the lifecycle signal directly.
    Native { event: &'static str },
    /// No native event, but RimZ reconstructs the behaviour from other state:
    /// `via` is the derivation, `gap` what the reconstruction still lacks.
    Derived {
        via: &'static str,
        gap: &'static str,
    },
    /// Unreachable from the current protocol surface; `reason` says why.
    Absent { reason: &'static str },
}

impl HookCoverage {
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native { .. })
    }

    /// The reason-like text: event for native, gap for derived, reason for
    /// absent — what the matrix prints after the signal label.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Native { event } => event,
            Self::Derived { gap, .. } => gap,
            Self::Absent { reason } => reason,
        }
    }
}

/// Compile-time-complete integration support claims for one adapter.
#[derive(Clone, Copy, Debug)]
pub struct CoverageAnnotations {
    pub turn_lifecycle: ConcernCoverage,
    pub permission: ConcernCoverage,
    pub plan_approval: ConcernCoverage,
    pub user_question: ConcernCoverage,
    pub answer: ConcernCoverage,
    pub compaction: ConcernCoverage,
    pub subagents: ConcernCoverage,
    pub background_parking: ConcernCoverage,
    pub session_end: ConcernCoverage,
    pub idle_notification: ConcernCoverage,
    pub context_usage: ConcernCoverage,
    pub realtime_cost: ConcernCoverage,
    pub rich_context: ConcernCoverage,
    pub hook_install: ConcernCoverage,
    pub account_spend: ConcernCoverage,
    pub tool_stats: ConcernCoverage,
    pub remote_control: ConcernCoverage,
}

impl CoverageAnnotations {
    pub const fn get(self, concern: IntegrationConcern) -> ConcernCoverage {
        match concern {
            IntegrationConcern::TurnLifecycle => self.turn_lifecycle,
            IntegrationConcern::Permission => self.permission,
            IntegrationConcern::PlanApproval => self.plan_approval,
            IntegrationConcern::UserQuestion => self.user_question,
            IntegrationConcern::Answer => self.answer,
            IntegrationConcern::Compaction => self.compaction,
            IntegrationConcern::Subagents => self.subagents,
            IntegrationConcern::BackgroundParking => self.background_parking,
            IntegrationConcern::SessionEnd => self.session_end,
            IntegrationConcern::IdleNotification => self.idle_notification,
            IntegrationConcern::ContextUsage => self.context_usage,
            IntegrationConcern::RealtimeCost => self.realtime_cost,
            IntegrationConcern::RichContext => self.rich_context,
            IntegrationConcern::HookInstall => self.hook_install,
            IntegrationConcern::AccountSpend => self.account_spend,
            IntegrationConcern::ToolStats => self.tool_stats,
            IntegrationConcern::RemoteControl => self.remote_control,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (IntegrationConcern, ConcernCoverage)> {
        IntegrationConcern::ALL
            .into_iter()
            .map(move |concern| (concern, self.get(concern)))
    }
}

/// The six capabilities the user-facing compatibility matrix names. These are
/// the questions a person asks before picking an agent, coarser than
/// [`IntegrationConcern`] and phrased in what they can see rather than what the
/// adapter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCapability {
    /// The card tracks the agent's whole life: start, working, waiting, idle, end.
    State,
    /// What the card shows mid-turn: context fill, token breakdown, live dollars.
    Live,
    /// Past sessions read end to end, with per-turn tokens and dollars.
    History,
    /// Login, plan, and each usage window with its fill and reset.
    Account,
    /// A blocked agent raising Waiting, and its question reaching `rimz asks`.
    Ask,
    /// Child agents nested under the parent card while they work.
    Subagents,
}

impl UserCapability {
    pub const ALL: [Self; 6] = [
        Self::State,
        Self::Live,
        Self::History,
        Self::Account,
        Self::Ask,
        Self::Subagents,
    ];

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Live => "live",
            Self::History => "history",
            Self::Account => "account",
            Self::Ask => "ask",
            Self::Subagents => "subagents",
        }
    }
}

/// What a user gets for one capability. The mark answers what they see and when
/// they see it; how RimZ obtains the figure stays out of it. A value folded from
/// a transcript tail and a value pushed by a native hook both read [`Full`] when
/// the surface is complete and current, and a native signal carrying half the
/// story reads [`Partial`].
///
/// [`Full`]: CapabilityLevel::Full
/// [`Partial`]: CapabilityLevel::Partial
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityLevel {
    /// Complete and live: the capability reads the way it does on Claude Code.
    /// `note` names what the user gets, in their own terms.
    Full { note: &'static str },
    /// A working version with a stated limit — part of the detail, or the whole
    /// of it a beat late. `shows` is what lands, `limit` what the user can still
    /// see is missing.
    Partial {
        shows: &'static str,
        limit: &'static str,
    },
    /// Nothing to render for this capability; `reason` says why.
    Unsupported { reason: &'static str },
}

impl CapabilityLevel {
    pub const fn is_full(self) -> bool {
        matches!(self, Self::Full { .. })
    }

    pub const fn is_unsupported(self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }

    /// The text the matrix prints after the capability label: what a full cell
    /// gives, what a partial cell still lacks, why an unsupported cell is empty.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Full { note } => note,
            Self::Partial { limit, .. } => limit,
            Self::Unsupported { reason } => reason,
        }
    }
}

/// Compile-time-complete user-facing capability claims for one adapter. Every
/// [`UserCapability`] appears exactly once; conformance checks each mark against
/// the [`CoverageAnnotations`] that back it, so a full claim always rests on
/// wired mechanism while a wired mechanism may still roll up to partial when the
/// user-visible result is incomplete or late.
#[derive(Clone, Copy, Debug)]
pub struct UserCoverage {
    pub state: CapabilityLevel,
    pub live: CapabilityLevel,
    pub history: CapabilityLevel,
    pub account: CapabilityLevel,
    pub ask: CapabilityLevel,
    pub subagents: CapabilityLevel,
}

impl UserCoverage {
    pub const fn get(self, capability: UserCapability) -> CapabilityLevel {
        match capability {
            UserCapability::State => self.state,
            UserCapability::Live => self.live,
            UserCapability::History => self.history,
            UserCapability::Account => self.account,
            UserCapability::Ask => self.ask,
            UserCapability::Subagents => self.subagents,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (UserCapability, CapabilityLevel)> {
        UserCapability::ALL
            .into_iter()
            .map(move |capability| (capability, self.get(capability)))
    }
}

/// Compile-time-complete lifecycle-signal support claims for one adapter.
#[derive(Clone, Copy, Debug)]
pub struct LifecycleAnnotations {
    pub registered: HookCoverage,
    pub turn_started: HookCoverage,
    pub turn_ended: HookCoverage,
    pub tool_used: HookCoverage,
    pub awaiting_input: HookCoverage,
    pub subagent_started: HookCoverage,
    pub subagent_stopped: HookCoverage,
    pub compacting: HookCoverage,
    pub compaction_ended: HookCoverage,
    pub ended: HookCoverage,
    pub lost: HookCoverage,
}

impl LifecycleAnnotations {
    pub const fn get(self, signal: LifecycleSignalKind) -> HookCoverage {
        match signal {
            LifecycleSignalKind::Registered => self.registered,
            LifecycleSignalKind::TurnStarted => self.turn_started,
            LifecycleSignalKind::TurnEnded => self.turn_ended,
            LifecycleSignalKind::ToolUsed => self.tool_used,
            LifecycleSignalKind::AwaitingInput => self.awaiting_input,
            LifecycleSignalKind::SubagentStarted => self.subagent_started,
            LifecycleSignalKind::SubagentStopped => self.subagent_stopped,
            LifecycleSignalKind::Compacting => self.compacting,
            LifecycleSignalKind::CompactionEnded => self.compaction_ended,
            LifecycleSignalKind::Ended => self.ended,
            LifecycleSignalKind::Lost => self.lost,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = (LifecycleSignalKind, HookCoverage)> {
        LifecycleSignalKind::ALL
            .into_iter()
            .map(move |signal| (signal, self.get(signal)))
    }
}

/// Operational policy that cannot be derived from integration coverage.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// Renders its own ask UI in the pane — permission prompts, plan
    /// approvals, questions — so RimZ can mark the agent waiting while the
    /// prompt stays in the native UI. An agent without one (pi gates tools
    /// only through the extension) resolves the same ask neutrally with no
    /// waiting state: there is no native prompt to route the human to.
    pub native_ask_ui: bool,
    /// Local transcript/rollout tail is a live context source refreshable
    /// outside hooks. Drives producer ticks and renderer transcript watches.
    pub transcript_tail_context: bool,
    /// Registers its session lazily and/or routes hooks through a daemon, so
    /// an instance can be present without a stamped session. The sidebar binds
    /// such a session to its pane by cwd.
    pub registers_lazily: bool,
    /// Discovers live session identity and lifecycle from a provider-owned
    /// machine-local store, without executable hooks or RimZ store writes.
    pub local_session_discovery: bool,
    /// Sessions route hooks through a per-user daemon that outlives any one
    /// conversation, so a new session may succeed another in the same pane
    /// before the reaper clears the stamp.
    pub daemon_hooked_sessions: bool,
    /// Provides an authoritative, identity-bearing direct account-usage
    /// probe. Scheduling uses this static declaration before provider work.
    pub direct_account_usage: bool,
    /// Which co-resident root session owns a live pane when one agent process
    /// carries more than one session id.
    pub same_pane_session: SamePaneSessionPolicy,
    /// Remote-control surfaces the provider can host.
    pub remote_control: RemoteControlCapability,
}

/// Pane ownership for multiple root sessions hosted by one live agent process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamePaneSessionPolicy {
    /// Keep the earliest registered session as the pane's primary owner.
    KeepPrimary,
    /// Hand the pane to the most recently registered active conversation.
    FollowLatest,
}

/// Static remote-control capability. Dynamic "enabled on this machine" state
/// lives on [`AgentDefinition`](super::AgentDefinition), because it may read provider
/// settings.
#[derive(Clone, Copy, Debug)]
pub struct RemoteControlCapability {
    /// Living pane sessions can be driven remotely.
    pub pane_sessions: bool,
    /// The provider can spawn background remote sessions without a local pane.
    pub background_sessions: bool,
}

impl AgentSpec {
    /// Declared support for one product concern.
    pub const fn concern_coverage(&self, concern: IntegrationConcern) -> ConcernCoverage {
        self.coverage.get(concern)
    }

    /// Whether RimZ can install hooks this adapter executes.
    pub const fn has_wired_hook_install(&self) -> bool {
        self.concern_coverage(IntegrationConcern::HookInstall)
            .is_wired()
    }

    /// User-facing reason shown when hook installation is unavailable.
    pub const fn hook_install_failure_detail(&self) -> Option<&'static str> {
        match self.concern_coverage(IntegrationConcern::HookInstall) {
            ConcernCoverage::Wired { .. } => None,
            coverage => Some(coverage.detail()),
        }
    }

    /// Whether this adapter publishes authoritative account-level dollars that
    /// can safely enforce a provider-account budget.
    pub fn has_authoritative_account_spend(&self) -> bool {
        matches!(
            self.concern_coverage(IntegrationConcern::AccountSpend),
            ConcernCoverage::Wired { .. }
        )
    }

    /// The kind as a typed identity — the one sanctioned mint of an
    /// [`AgentKind`](crate::ids::AgentKind) for a known adapter.
    pub fn kind_id(&self) -> crate::ids::AgentKind {
        crate::ids::AgentKind::new_unchecked(self.kind)
    }

    /// Whether a process `comm`/argv0 basename belongs to this agent: one of
    /// its declared process names (its own binary plus any launcher), including
    /// a declared name under a target-triple release-binary suffix.
    pub fn runs_as(&self, name: &str) -> bool {
        self.process_names
            .iter()
            .any(|program| program_names_kind(name, program))
    }

    /// Whether a command basename launches this agent. This is distinct from
    /// [`runs_as`](Self::runs_as): a resolved executable may expose a provider-
    /// independent name (`agent` for Cursor) while its kernel `comm` names the
    /// runtime instead. Target-triple suffix matching applies to each declared
    /// name rather than implicitly accepting the kind, so `antigravity` does
    /// not shadow the actual `agy` process.
    pub fn launches_as(&self, name: &str) -> bool {
        self.bin_names
            .iter()
            .any(|program| program_names_kind(name, program))
    }

    /// The identity check to run against a candidate located by `name`, when
    /// `name` is one of this agent's ambiguous [`bin_names`](Self::bin_names).
    /// `None` for a provider-unique name, which discovery accepts on filename
    /// alone.
    pub(crate) fn ambiguous_bin_identity(&self, name: &str) -> Option<&BinIdentity> {
        self.bin_identity
            .as_ref()
            .filter(|identity| identity.ambiguous.contains(&name))
    }

    /// Whether a tool-use payload names a workspace-mutating tool. The tool
    /// name rides `tool_name` in every provider's payload.
    pub fn tool_mutates(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.mutating)
    }

    /// A stable digest of a tool call's identity: its name plus its arguments.
    /// Returns `None` when the adapter declares no input key or the payload
    /// carries neither a name nor input there, keeping repeat detection off
    /// instead of falling back to an imprecise name-only key.
    pub fn tool_signature(&self, payload: &Value) -> Option<String> {
        let name = payload.get("tool_name").and_then(Value::as_str)?.trim();
        if name.is_empty() {
            return None;
        }
        let input = payload.get(self.tools.input_key?)?;
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(input).ok()?);
        let digest = hasher.finalize();
        Some(
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Whether a tool-use payload names a *file-editing* tool.
    pub fn tool_edits_files(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.editing)
    }

    /// Whether a pre-tool-use payload names a blocking ask tool.
    pub fn blocking_tool_kind(&self, tool_name: Option<&str>) -> Option<AskKind> {
        let name = tool_name?;
        self.tools
            .blocking
            .iter()
            .find_map(|(tool, kind)| (*tool == name).then_some(*kind))
    }

    fn tool_in(&self, payload: &Value, set: &[&str]) -> bool {
        payload
            .get("tool_name")
            .and_then(Value::as_str)
            .is_some_and(|name| set.contains(&name))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{LaunchSpec, PromptStyle, program_names_kind};
    use crate::agents::AskKind;
    use crate::agents::registry::BUILTINS;

    #[test]
    fn every_descriptor_keeps_editing_a_subset_of_mutating() {
        for adapter in BUILTINS {
            let definition = adapter.spec();
            for tool in definition.tools.editing {
                assert!(
                    definition.tools.mutating.contains(tool),
                    "{}: editing tool {tool} missing from the mutating set",
                    definition.kind,
                );
            }
        }
    }

    #[test]
    fn prompt_flag_suffix_is_conditional_on_nonempty_prompt() {
        let launch = LaunchSpec {
            program: Some("agent"),
            fixed_args: &["--fixed"],
            prompt: PromptStyle::FlagWithSuffix {
                flag: "--prompt",
                suffix: &["--format", "jsonl"],
            },
            ..LaunchSpec::EMPTY
        };
        let extra = ["--verbose".to_owned()];
        assert_eq!(
            launch.launch_command(&extra, Some("review")),
            Some(
                [
                    "agent",
                    "--fixed",
                    "--verbose",
                    "--prompt",
                    "review",
                    "--format",
                    "jsonl",
                ]
                .map(ToOwned::to_owned)
                .to_vec()
            )
        );
        for prompt in [None, Some("")] {
            assert_eq!(
                launch.launch_command(&extra, prompt),
                Some(
                    ["agent", "--fixed", "--verbose"]
                        .map(ToOwned::to_owned)
                        .to_vec()
                )
            );
        }
    }

    #[test]
    fn descriptor_classifies_mutating_editing_and_blocking_tools() {
        let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
        assert!(claude.tool_mutates(&json!({ "tool_name": "Edit" })));
        assert!(claude.tool_mutates(&json!({ "tool_name": "Bash" })));
        assert!(!claude.tool_mutates(&json!({ "tool_name": "Read" })));
        assert!(!claude.tool_mutates(&json!({})));
        // Command runners mutate but do not edit — the reasoning phase survives.
        assert!(!claude.tool_edits_files(&json!({ "tool_name": "Bash" })));
        assert!(claude.tool_edits_files(&json!({ "tool_name": "Write" })));
        assert_eq!(
            claude.blocking_tool_kind(Some("ExitPlanMode")),
            Some(AskKind::PlanApproval)
        );
        assert_eq!(
            claude.blocking_tool_kind(Some("AskUserQuestion")),
            Some(AskKind::Question)
        );
        assert_eq!(claude.blocking_tool_kind(Some("request_user_input")), None);

        let codex = crate::agents::registry::spec_by_kind("codex").unwrap();
        assert!(codex.tool_mutates(&json!({ "tool_name": "apply_patch" })));
        assert!(codex.tool_edits_files(&json!({ "tool_name": "apply_patch" })));
        assert!(!codex.tool_edits_files(&json!({ "tool_name": "shell" })));
        assert_eq!(
            codex.blocking_tool_kind(Some("request_user_input")),
            Some(AskKind::Question)
        );
        assert_eq!(codex.blocking_tool_kind(Some("ExitPlanMode")), None);
        assert_eq!(codex.blocking_tool_kind(Some("update_plan")), None);
        assert_eq!(codex.blocking_tool_kind(None), None);
    }

    #[test]
    fn tool_signature_is_canonical_and_argument_sensitive() {
        let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
        let ordered = claude
            .tool_signature(&json!({
                "tool_name": "Bash",
                "tool_input": { "command": "cargo check", "timeout": 30 }
            }))
            .expect("Claude signature");
        let reordered = claude
            .tool_signature(&json!({
                "tool_input": { "timeout": 30, "command": "cargo check" },
                "tool_name": "Bash"
            }))
            .expect("reordered Claude signature");
        let changed = claude
            .tool_signature(&json!({
                "tool_name": "Bash",
                "tool_input": { "command": "cargo test", "timeout": 30 }
            }))
            .expect("changed Claude signature");

        assert_eq!(ordered, reordered);
        assert_ne!(ordered, changed);
    }

    #[test]
    fn tool_signature_covers_each_reachable_adapter() {
        for (kind, payload, expected) in [
            (
                "claude",
                json!({
                    "tool_name": "Bash",
                    "tool_input": {"command": "cargo check", "timeout": 30}
                }),
                "b5b6f4d3b2a47915",
            ),
            (
                "codex",
                json!({"tool_name": "exec_command", "tool_input": {"cmd": "cargo check"}}),
                "934fb6c158c4e4bb",
            ),
        ] {
            let spec = crate::agents::registry::spec_by_kind(kind).unwrap();
            assert_eq!(
                spec.tool_signature(&payload).as_deref(),
                Some(expected),
                "{kind} hook payload"
            );
        }

        let opencode = crate::agents::registry::spec_by_kind("opencode").unwrap();
        assert_eq!(
            opencode.tool_signature(&json!({
                "tool_name": "bash",
                "input": {"command": "cargo check"}
            })),
            None
        );
    }

    #[test]
    fn tool_signature_rejects_empty_names() {
        let claude = crate::agents::registry::spec_by_kind("claude").unwrap();
        for name in ["", "   "] {
            assert_eq!(
                claude.tool_signature(&json!({
                    "tool_name": name,
                    "tool_input": {"command": "cargo check"}
                })),
                None
            );
        }
    }

    #[test]
    fn target_triple_binary_names_still_name_the_agent_kind() {
        for name in [
            "codex",
            "codex-aarch64-apple-darwin",
            "codex-x86_64-apple-darwin",
            "codex-x86_64-unknown-linux-musl",
            "codex-aarch64-unknown-linux-gnu",
            "codex-aarch64-a",
        ] {
            assert!(program_names_kind(name, "codex"), "{name}");
        }

        for name in ["codexfoo", "codex-plan", "codex-appserver-stub", "node"] {
            assert!(!program_names_kind(name, "codex"), "{name}");
        }
    }

    #[test]
    fn descriptor_run_names_include_launchers_and_target_triples() {
        let codex = crate::agents::registry::spec_by_kind("codex").unwrap();

        assert!(codex.runs_as("codex"));
        assert!(codex.runs_as("node"));
        assert!(codex.runs_as("codex-aarch64-a"));
        assert!(!codex.runs_as("zsh"));
    }
}
