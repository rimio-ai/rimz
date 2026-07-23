//! Kiro CLI v3 launch, local-session, transcript, and live-state adapter.
//!
//! Kiro CLI 2.12.1 does not execute the documented standalone hook configs in
//! a stock v3 session. Keep launch, resume, and process identity available;
//! executable hook installation stays unsupported. The stock structured
//! session store supplies validated pulled truth for live display and history.

mod install;
mod session;
// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::AccountCapability for KiroAdapter {}
impl crate::agents::capabilities::HookCapability for KiroAdapter {}
impl crate::agents::capabilities::RuntimeControlCapability for KiroAdapter {}

#[cfg(test)]
mod tests;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::{
    LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx, LocalSessionObservation,
    RefreshTrigger, TranscriptMessage, TranscriptStat,
};

const HOOK_INSTALL_UNAVAILABLE: &str = "the v3 engine does not execute standalone hook configs (verified against Kiro CLI 2.12.1); re-enable after a pinned v3 release provides a reproducible native hook contract";

static KIRO_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "kiro",
    aliases: &[],
    bin_names: &["kiro-cli"],
    bin_identity: None,
    display_name: "Kiro",
    brand: Brand {
        emblem: None,
        color: 92,
        color_rgb: (0x79, 0x0e, 0xcb),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Kiro" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &["fs_write"],
        editing: &["fs_write"],
        blocking: &[],
    },
    capabilities: Capabilities {
        // Kiro can draw native prompts, but v3 exposes no hook that records
        // them for RimZ routing.
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: KIRO_COVERAGE,
    user_coverage: KIRO_USER_COVERAGE,
    lifecycle_hooks: KIRO_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    // `kiro-cli` launches the session; the v3 chat engine lives in the separate
    // `kiro-cli-chat` binary the launcher execs into, so a live pane can read as
    // either. `kiro-cli-term` is the figterm shell-integration daemon (it runs
    // for every integrated shell, not just agent panes), so it is deliberately
    // excluded to avoid false-positive presence.
    process_names: &["kiro-cli", "kiro-cli-chat"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("kiro-cli"),
        fixed_args: &["chat", "--v3"],
        prompt: super::PromptStyle::PositionalAfterDoubleDash,
        resume: Some(super::SessionCommand {
            before_id: &["kiro-cli", "chat", "--v3", "--resume-id"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs::EMPTY,
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--effort"])),
            system_prompt_file: None,
            append_system_prompt_file: None,
        },
    },
};

const KIRO_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Partial {
        via: "ordered stock-v3 session store records",
        gap: "pulled display truth; no executable hook or uncaptured failure/cancel shapes",
    },
    permission: ConcernCoverage::Partial {
        via: "unresolved pending_interaction tool_approval records",
        gap: "waiting is visible but not routable through rimz asks/answer",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "no v3 plan-approval hook",
    },
    user_question: ConcernCoverage::Unsupported {
        reason: "no v3 native-question hook",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native prompt choreography is not mapped",
    },
    compaction: ConcernCoverage::Unsupported {
        reason: "no stock-TUI compaction hook; compaction rotates the session id",
    },
    subagents: ConcernCoverage::Unsupported {
        reason: "v3 hooks do not fire in subagents and publish no child lifecycle",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no background-task parking signal",
    },
    session_end: ConcernCoverage::Partial {
        via: "pane liveness + rollup reaper",
        gap: "no SessionEnd hook; v3 Stop is turn end, not session end",
    },
    idle_notification: ConcernCoverage::Partial {
        via: "successful session_pause and turn_end records",
        gap: "pulled state has no native notification wakeup",
    },
    context_usage: ConcernCoverage::Partial {
        via: "latest contextUsage session_metadata percentage",
        gap: "percentage only; no token counts or context-window size",
    },
    realtime_cost: ConcernCoverage::Unsupported {
        reason: "credit-metered; no machine-readable usage surface",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "hooks publish no model, effort, context, or transcript contract",
    },
    hook_install: ConcernCoverage::Unsupported {
        reason: HOOK_INSTALL_UNAVAILABLE,
    },
    account_spend: ConcernCoverage::Unsupported {
        reason: "whoami schema and credit ledger are unpublished",
    },
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no stock-TUI remote-control surface",
    },
};

const KIRO_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Partial {
        shows: "the card follows turns from Kiro's own session store",
        limit: "state is read rather than reported, so cancels can read as ordinary stops",
    },
    live: CapabilityLevel::Partial {
        shows: "a context-fill percentage",
        limit: "no token counts, no context-window size, and no dollar figure",
    },
    history: CapabilityLevel::Partial {
        shows: "past sessions read end to end from Kiro's own store",
        limit: "no tokens or dollars, so Kiro stays out of rimz stats",
    },
    account: CapabilityLevel::Unsupported {
        reason: "Kiro publishes no readable login, plan, or credit ledger",
    },
    ask: CapabilityLevel::Partial {
        shows: "a pending tool approval raises Waiting and routes you to the pane",
        limit: "the prompt stays in Kiro's own UI, so rimz asks stays empty",
    },
    subagents: CapabilityLevel::Unsupported {
        reason: "Kiro publishes no child lifecycle",
    },
};

const KIRO_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
    registered: HookCoverage::Derived {
        via: "validated local session metadata",
        gap: "provider store discovery replaces an executable registration hook",
    },
    turn_started: HookCoverage::Derived {
        via: "ordered turn_start records",
        gap: "pulled provider state, not an installed hook",
    },
    turn_ended: HookCoverage::Derived {
        via: "verified successful turn_end/session_pause records",
        gap: "failure and cancellation records remain uncaptured",
    },
    tool_used: HookCoverage::Derived {
        via: "verified tool_call/tool_result records",
        gap: "only observed stock-v3 tool vocabulary is classified",
    },
    awaiting_input: HookCoverage::Derived {
        via: "unresolved pending_interaction tool approval",
        gap: "native prompt is visible but has no structured RimZ answer route",
    },
    subagent_started: HookCoverage::Absent {
        reason: "v3 hooks do not fire in subagents",
    },
    subagent_stopped: HookCoverage::Absent {
        reason: "v3 hooks do not fire in subagents",
    },
    compacting: HookCoverage::Absent {
        reason: "no stock-TUI compaction hook",
    },
    compaction_ended: HookCoverage::Absent {
        reason: "no stock-TUI compaction hook",
    },
    ended: HookCoverage::Derived {
        via: "pane liveness + rollup reaper",
        gap: "Stop ends a turn, not the session",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

#[derive(Clone, Debug, Default)]
pub struct KiroAdapter;

impl crate::agents::capabilities::CoreCapability for KiroAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &KIRO_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            local_session: Some(session::fixture_observation()),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::LaunchCapability for KiroAdapter {}

impl crate::agents::capabilities::InstallationCapability for KiroAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

impl crate::agents::capabilities::SessionCapability for KiroAdapter {
    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
    }
}

impl crate::agents::capabilities::TranscriptCapability for KiroAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for KiroAdapter {
    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let path =
            self.session_transcript(ctx.agent_id, ctx.prior_transcript_path.map(Path::new))?;
        let stat = TranscriptStat::from_path(&path)?;
        if ctx.prior_transcript_stat == Some(&stat) {
            return None;
        }
        Some(LocalContextRefresh {
            context: LocalContextPatch::authoritative_current(),
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
            ..LocalContextRefresh::authoritative_current()
        })
    }
}

impl crate::agents::capabilities::SpendingCapability for KiroAdapter {
    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| session::valid_transcript(path, session_id)) {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session(session_id)
    }
}
