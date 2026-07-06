//! Static, allocation-free identity and capability data for one agent.
//!
//! One `const` [`AgentDescriptor`] per adapter directory; the registry holds
//! `&'static` references. Everything here is data a `match kind { … }` used to
//! encode somewhere in core — folded into the one place an agent is declared,
//! so adding an agent never edits a shared dispatch site. Anything that parses
//! a payload, touches the filesystem or network, or spawns a helper is an
//! [`AgentAdapter`](super::AgentAdapter) trait method, never descriptor data.

use serde_json::Value;

use crate::feed::FeedKind;

use super::lifecycle::LifecycleSignalKind;

/// Static identity, branding, capabilities, and classification tables for one
/// agent. See the module doc for the descriptor-vs-trait split.
#[derive(Debug)]
pub struct AgentDescriptor {
    /// The stable kind string — the `--source` tag, the per-provider bucket
    /// key, the rollup `kind`.
    pub kind: &'static str,
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
    /// Tool-name classification tables for lifecycle and blocking feed use.
    pub tools: ToolClassification,
    /// What this agent can and cannot do — consumed by the sidebar and doctor
    /// so a missing surface renders as a declared absence, never an
    /// accidental gap.
    pub capabilities: Capabilities,
    /// Declared integration checklist. Every [`IntegrationConcern`] appears
    /// exactly once as wired, partial, or unsupported, and conformance tests
    /// cross-check the declaration against the descriptor and classification
    /// corpus.
    pub coverage: &'static [(IntegrationConcern, ConcernCoverage)],
    /// Declared lifecycle-hook checklist. Every [`LifecycleSignalKind`] appears
    /// exactly once as native, derived, or absent; conformance checks the
    /// native event names against the installed hook events and classification
    /// corpus.
    pub lifecycle_hooks: &'static [(LifecycleSignalKind, HookCoverage)],
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
    /// Well-known install directories, relative to `$HOME`, where this agent's
    /// binary (named [`kind`](Self::kind)) lives when its installer has not put
    /// it on `$PATH` — OpenCode's installer drops `opencode` in `~/.opencode/bin`
    /// and edits a shell rc the daemon never sources. Searched after `$PATH` by
    /// [`locate_binary`](super::locate_binary); empty for an agent that only
    /// ever ships on `$PATH`.
    pub extra_bin_dirs: &'static [&'static str],
    /// Lifecycle events, in this agent's own wire vocabulary, that prove the
    /// agent is actively making progress — a tool completed, a turn started
    /// or ended, a subagent spawned or finished. These refresh the per-agent
    /// activity heartbeat. A blocking pre-tool gate (Claude's `PreToolUse`,
    /// pi's `tool_call`) is deliberately excluded: it can fire in the same
    /// tool call as a blocking ask, so touching on it would race the ask
    /// creation and instantly un-block the row. An idle notification is
    /// excluded too — waiting for input is not progress.
    pub activity_events: &'static [&'static str],
    /// User-facing reason shown by doctor/start when
    /// [`Capabilities::hook_install`] is false.
    pub hook_install_unavailable: Option<&'static str>,
    /// How this agent's transcript files map to billing threads, for the
    /// spending session count.
    pub thread_key: ThreadKey,
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
    /// Multi-line ASCII emblem, written as drawn (one row per line). The
    /// literal opens with a bare newline so the art starts at column 0 with
    /// its leading spaces intact — a `\` continuation would eat them — and
    /// the read path trims the surrounding newlines.
    pub emblem: &'static str,
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

/// The agent's tool vocabulary, classified for lifecycle and blocking feed use.
#[derive(Debug)]
pub struct ToolClassification {
    /// Tools that mutate the workspace — write files or run commands. A
    /// mutating tool is proof of real work, so its `PostToolUse` is the only
    /// tool event recorded on the lifecycle channel.
    pub mutating: &'static [&'static str],
    /// The file-editing subset of `mutating` — the turn's first edit moves it
    /// from reasoning to acting. A shell tool mutates but does not edit, so a
    /// research turn that only runs commands keeps the thinking head.
    pub editing: &'static [&'static str],
    /// Tools whose pre-use hook is a blocking ask, paired with the feed kind
    /// they raise. Empty when the agent's blocking gate is an event, not a tool.
    pub blocking: &'static [(&'static str, FeedKind)],
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
    RemoteControl => "remote",
}

/// How an adapter covers a concern: a native signal carries it directly,
/// derivation reconstructs it where the native signal is absent, or it is
/// unreachable from the current protocol surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConcernCoverage {
    /// A native signal carries the concern directly; `via` names it.
    Wired { via: &'static str },
    /// No native signal, but Rimz reconstructs the behaviour from other state:
    /// `via` is the derivation, `gap` what the reconstruction still lacks.
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
    /// No native event, but Rimz reconstructs the behaviour from other state:
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

/// Explicit capability declaration. A provider that *cannot* do something
/// declares it here instead of leaving an inferable absence. Several flags
/// gate behavior today: `rich_context` (the provider-owned live context
/// transport), `context_usage` and `account_spend` (the token/cost read
/// paths), `registers_lazily` (cwd session binding),
/// `hook_install` (the install and doctor surfaces), and `native_ask_ui`
/// (whether an unresolved blocking ask becomes a `native_ui` feed item). The
/// rest state the adapter contract up front — pinned by each adapter's tests,
/// consumed as shared sites grow capability-aware.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// Can natively hold a turn open for a permission/plan/question decision
    /// (the blocking-feed channel).
    pub blocking_feed: bool,
    /// Renders its own ask UI in the pane — permission prompts, plan
    /// approvals, questions — so a blocking ask can hand off to the agent's
    /// surface as a `native_ui` feed item. An agent
    /// without one (pi gates tools only through the extension) resolves the
    /// same ask neutrally with no feed item: there is no surface the item
    /// could route the human to, so pushing one would strand it waiting.
    pub native_ask_ui: bool,
    /// Surfaces provider-owned rich context beyond the local lifecycle and
    /// transcript tail, such as account windows, official model labels, PR
    /// metadata, or agent version.
    pub rich_context: bool,
    /// Local transcript/rollout tail is a live context source refreshable
    /// outside hooks. Drives producer ticks and renderer transcript watches.
    pub transcript_tail_context: bool,
    /// Surfaces per-session token/context usage into the agent row.
    pub context_usage: bool,
    /// Surfaces provider spend from transcripts, account usage, or session
    /// events.
    pub account_spend: bool,
    /// Routes child tasks through `Subagent{Start,Stop}` lifecycle signals.
    pub subagents: bool,
    /// Has a notion of parking a turn on still-in-flight background work.
    pub background_tasks: bool,
    /// Registers its session lazily and/or routes hooks through a daemon, so
    /// an instance can be present without a stamped session. The sidebar binds
    /// such a session to its pane by cwd.
    pub registers_lazily: bool,
    /// Sessions route hooks through a per-user daemon that outlives any one
    /// conversation, so a new session may succeed another in the same pane
    /// before the reaper clears the stamp.
    pub daemon_hooked_sessions: bool,
    /// Rimz can install a hook configuration the agent actually executes.
    pub hook_install: bool,
    /// How this provider's realtime usage channel interacts with the uniform
    /// OAuth account-usage driver.
    pub realtime_usage: RealtimeUsageChannel,
    /// Remote-control surfaces the provider can host.
    pub remote_control: RemoteControlCapability,
}

/// How a provider's realtime usage channel interacts with the uniform OAuth
/// account-usage driver.
#[derive(Clone, Copy, Debug)]
pub struct RealtimeUsageChannel {
    /// A live root session's realtime channel already covers the
    /// account-scoped fetch, so the driver skips this kind while one is live.
    pub covers_account_while_live: bool,
    /// A content-fresh realtime windows reading owns the included-budget
    /// windows, so the OAuth merge defers to it.
    pub windows_defer_to_fresh_realtime: bool,
}

/// Static remote-control capability. Dynamic "enabled on this machine" state
/// lives on [`AgentAdapter`](super::AgentAdapter), because it may read provider
/// settings.
#[derive(Clone, Copy, Debug)]
pub struct RemoteControlCapability {
    /// Living pane sessions can be driven remotely.
    pub pane_sessions: bool,
    /// The provider can spawn background remote sessions without a local pane.
    pub background_sessions: bool,
}

impl AgentDescriptor {
    /// The kind as a typed identity — the one sanctioned mint of an
    /// [`AgentKind`](crate::ids::AgentKind) for a known adapter.
    pub fn kind_id(&self) -> crate::ids::AgentKind {
        crate::ids::AgentKind::new_unchecked(self.kind)
    }

    /// Whether an event refreshes the per-agent activity heartbeat. See
    /// [`activity_events`](Self::activity_events).
    pub fn records_activity(&self, event_name: &str) -> bool {
        self.activity_events.contains(&event_name)
    }

    /// Whether a process `comm`/argv0 basename belongs to this agent: one of
    /// its declared process names (its own binary plus any launcher), or the
    /// kind under a target-triple release-binary suffix.
    pub fn runs_as(&self, name: &str) -> bool {
        self.process_names.contains(&name) || program_names_kind(name, self.kind)
    }

    /// Whether a tool-use payload names a workspace-mutating tool. The tool
    /// name rides `tool_name` in every provider's payload.
    pub fn tool_mutates(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.mutating)
    }

    /// Whether a tool-use payload names a *file-editing* tool.
    pub fn tool_edits_files(&self, payload: &Value) -> bool {
        self.tool_in(payload, self.tools.editing)
    }

    /// Whether a pre-tool-use payload names a blocking ask tool.
    pub fn blocking_tool_kind(&self, tool_name: Option<&str>) -> Option<FeedKind> {
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

    use super::program_names_kind;
    use crate::agents::registry::ADAPTERS;
    use crate::feed::FeedKind;

    #[test]
    fn every_descriptor_keeps_editing_a_subset_of_mutating() {
        for adapter in ADAPTERS {
            let descriptor = adapter.descriptor();
            for tool in descriptor.tools.editing {
                assert!(
                    descriptor.tools.mutating.contains(tool),
                    "{}: editing tool {tool} missing from the mutating set",
                    descriptor.kind,
                );
            }
        }
    }

    #[test]
    fn descriptor_classifies_mutating_editing_and_blocking_tools() {
        let claude = crate::agents::registry::descriptor_by_kind("claude").unwrap();
        assert!(claude.tool_mutates(&json!({ "tool_name": "Edit" })));
        assert!(claude.tool_mutates(&json!({ "tool_name": "Bash" })));
        assert!(!claude.tool_mutates(&json!({ "tool_name": "Read" })));
        assert!(!claude.tool_mutates(&json!({})));
        // Command runners mutate but do not edit — the reasoning phase survives.
        assert!(!claude.tool_edits_files(&json!({ "tool_name": "Bash" })));
        assert!(claude.tool_edits_files(&json!({ "tool_name": "Write" })));
        assert_eq!(
            claude.blocking_tool_kind(Some("ExitPlanMode")),
            Some(FeedKind::PlanApproval)
        );
        assert_eq!(
            claude.blocking_tool_kind(Some("AskUserQuestion")),
            Some(FeedKind::Question)
        );
        assert_eq!(claude.blocking_tool_kind(Some("request_user_input")), None);

        let codex = crate::agents::registry::descriptor_by_kind("codex").unwrap();
        assert!(codex.tool_mutates(&json!({ "tool_name": "apply_patch" })));
        assert!(codex.tool_edits_files(&json!({ "tool_name": "apply_patch" })));
        assert!(!codex.tool_edits_files(&json!({ "tool_name": "shell" })));
        assert_eq!(
            codex.blocking_tool_kind(Some("request_user_input")),
            Some(FeedKind::Question)
        );
        assert_eq!(codex.blocking_tool_kind(Some("ExitPlanMode")), None);
        assert_eq!(codex.blocking_tool_kind(Some("update_plan")), None);
        assert_eq!(codex.blocking_tool_kind(None), None);
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
        let codex = crate::agents::registry::descriptor_by_kind("codex").unwrap();

        assert!(codex.runs_as("codex"));
        assert!(codex.runs_as("node"));
        assert!(codex.runs_as("codex-aarch64-a"));
        assert!(!codex.runs_as("zsh"));
    }

    #[test]
    fn capabilities_are_pinned_per_adapter() {
        let claude = crate::agents::registry::descriptor_by_kind("claude").unwrap();
        assert!(claude.capabilities.remote_control.pane_sessions);
        assert!(claude.capabilities.remote_control.background_sessions);
        assert!(claude.capabilities.rich_context);
        assert!(!claude.capabilities.transcript_tail_context);
        assert!(!claude.capabilities.daemon_hooked_sessions);
        assert!(!claude.capabilities.realtime_usage.covers_account_while_live);
        assert!(
            claude
                .capabilities
                .realtime_usage
                .windows_defer_to_fresh_realtime
        );

        let codex = crate::agents::registry::descriptor_by_kind("codex").unwrap();
        assert!(codex.capabilities.remote_control.pane_sessions);
        assert!(codex.capabilities.remote_control.background_sessions);
        assert!(codex.capabilities.rich_context);
        assert!(codex.capabilities.transcript_tail_context);
        assert!(codex.capabilities.daemon_hooked_sessions);
        assert!(codex.capabilities.realtime_usage.covers_account_while_live);
        assert!(
            !codex
                .capabilities
                .realtime_usage
                .windows_defer_to_fresh_realtime
        );

        let pi = crate::agents::registry::descriptor_by_kind("pi").unwrap();
        assert!(!pi.capabilities.remote_control.pane_sessions);
        assert!(!pi.capabilities.remote_control.background_sessions);
        assert!(!pi.capabilities.rich_context);
        assert!(!pi.capabilities.transcript_tail_context);
        assert!(!pi.capabilities.daemon_hooked_sessions);
        assert!(!pi.capabilities.realtime_usage.covers_account_while_live);
        assert!(
            !pi.capabilities
                .realtime_usage
                .windows_defer_to_fresh_realtime
        );

        let opencode = crate::agents::registry::descriptor_by_kind("opencode").unwrap();
        assert!(!opencode.capabilities.remote_control.pane_sessions);
        assert!(!opencode.capabilities.remote_control.background_sessions);
        assert!(opencode.capabilities.rich_context);
        assert!(!opencode.capabilities.transcript_tail_context);
        assert!(!opencode.capabilities.daemon_hooked_sessions);
        assert!(
            !opencode
                .capabilities
                .realtime_usage
                .covers_account_while_live
        );
        assert!(
            !opencode
                .capabilities
                .realtime_usage
                .windows_defer_to_fresh_realtime
        );
    }
}
