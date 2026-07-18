//! Kiro CLI v3 launch, local-session, transcript, and live-state adapter.
//!
//! Kiro CLI 2.12.1 does not execute the documented standalone hook configs in
//! a stock v3 session. Keep launch, resume, and process identity available;
//! executable hook installation stays unsupported. The stock structured
//! session store supplies validated pulled truth for live display and history.

mod install;
mod session;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::{
    AgentAdapter, AgentErr, AgentHookClass, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, LocalContextRefresh, LocalContextRefreshCtx, LocalSessionObservation,
    RefreshTrigger, Result, TranscriptMessage, TranscriptStat,
};

const HOOK_INSTALL_UNAVAILABLE: &str = "the v3 engine does not execute standalone hook configs (verified against Kiro CLI 2.12.1); re-enable after a pinned v3 release provides a reproducible native hook contract";

static KIRO_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "kiro",
    bin_names: &["kiro-cli"],
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
    activity_events: &[],
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
        ping_args: None,
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

const KIRO_COVERAGE: IntegrationCoverage = IntegrationCoverage {
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
    remote_control: ConcernCoverage::Unsupported {
        reason: "no stock-TUI remote-control surface",
    },
};

const KIRO_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
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

impl AgentAdapter for KiroAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &KIRO_DESCRIPTOR
    }

    #[cfg(test)]
    fn local_session_fixture(&self) -> Option<LocalSessionObservation> {
        Some(session::fixture_observation())
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        ClassifiedHook {
            class: AgentHookClass::Unknown,
            ask_kind: None,
            event_name: event_name.to_owned(),
        }
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        // Keep defensive manual feeds silent; if a future pinned release makes
        // documented hooks executable, stdout may become an agent input.
        Ok(None)
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }

    fn discover_local_sessions(&self, workspaces: &[&Path]) -> Vec<LocalSessionObservation> {
        session::discover(workspaces)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        session::transcript_files()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| session::valid_transcript(path, session_id)) {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session(session_id)
    }

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
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
            ..LocalContextRefresh::default()
        })
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: HOOK_INSTALL_UNAVAILABLE.to_owned(),
        })
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        Err(AgentErr::Install {
            agent: self.descriptor().kind,
            reason: HOOK_INSTALL_UNAVAILABLE.to_owned(),
        })
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall_from(&install::hooks_path()?)
    }

    fn hooks_installed(&self) -> bool {
        false
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::hooks_path().is_ok_and(|path| install::managed_at(&path))
    }
}
