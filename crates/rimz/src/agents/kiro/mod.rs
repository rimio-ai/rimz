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
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::LifecycleSignalKind;
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
    tools: ToolClassification {
        mutating: &["fs_write"],
        editing: &["fs_write"],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: false,
        // Kiro can draw native prompts, but v3 exposes no hook that records
        // them for Rimz routing.
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: true,
        context_usage: true,
        account_spend: false,
        subagents: false,
        background_tasks: false,
        registers_lazily: true,
        local_session_discovery: true,
        daemon_hooked_sessions: false,
        hook_install: false,
        implicit_unlimited_window_mins: &[],
        realtime_usage: RealtimeUsageChannel {
            covers_account_while_live: false,
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
    hook_install_unavailable: Some(HOOK_INSTALL_UNAVAILABLE),
    thread_key: ThreadKey::PerFile,
};

const KIRO_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Partial {
            via: "ordered stock-v3 session store records",
            gap: "pulled display truth; no executable hook or uncaptured failure/cancel shapes",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Partial {
            via: "unresolved pending_interaction tool_approval records",
            gap: "waiting is visible but not routable through rimz asks/answer",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no v3 plan-approval hook",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no v3 native-question hook",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native prompt choreography is not mapped",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Unsupported {
            reason: "no stock-TUI compaction hook; compaction rotates the session id",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "v3 hooks do not fire in subagents and publish no child lifecycle",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "no background-task parking signal",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Partial {
            via: "pane liveness + rollup reaper",
            gap: "no SessionEnd hook; v3 Stop is turn end, not session end",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "successful session_pause and turn_end records",
            gap: "pulled state has no native notification wakeup",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Partial {
            via: "latest contextUsage session_metadata percentage",
            gap: "percentage only; no token counts or context-window size",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "credit-metered; no machine-readable usage surface",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "hooks publish no model, effort, context, or transcript contract",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Unsupported {
            reason: HOOK_INSTALL_UNAVAILABLE,
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "whoami schema and credit ledger are unpublished",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no stock-TUI remote-control surface",
        },
    ),
];

const KIRO_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Derived {
            via: "validated local session metadata",
            gap: "provider store discovery replaces an executable registration hook",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Derived {
            via: "ordered turn_start records",
            gap: "pulled provider state, not an installed hook",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Derived {
            via: "verified successful turn_end/session_pause records",
            gap: "failure and cancellation records remain uncaptured",
        },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Derived {
            via: "verified tool_call/tool_result records",
            gap: "only observed stock-v3 tool vocabulary is classified",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Derived {
            via: "unresolved pending_interaction tool approval",
            gap: "native prompt is visible but has no structured Rimz answer route",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "v3 hooks do not fire in subagents",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "v3 hooks do not fire in subagents",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Absent {
            reason: "no stock-TUI compaction hook",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Absent {
            reason: "no stock-TUI compaction hook",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "Stop ends a turn, not the session",
        },
    ),
    (
        LifecycleSignalKind::Lost,
        HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "native hooks do not report mux-session death",
        },
    ),
];

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

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--resume-id".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }

    fn discover_local_sessions(&self, workspace: &Path) -> Vec<LocalSessionObservation> {
        session::discover(workspace)
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
        let stat = transcript_stat(&path)?;
        if ctx.prior_transcript_stat == Some(&stat) {
            return None;
        }
        Some(LocalContextRefresh {
            model_id: None,
            model_display_name: None,
            effort: None,
            tokens: None,
            cost: None,
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            turn_interrupted: None,
            transcript_path: Some(path.to_string_lossy().into_owned()),
            transcript_stat: Some(stat),
        })
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/compact")
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "kiro",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "kiro",
                field: "append-system-prompt-file",
            });
        }
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        let flag = match field {
            super::PresetField::Model => "--model",
            super::PresetField::Effort => "--effort",
            super::PresetField::SystemPromptFile | super::PresetField::AppendSystemPromptFile => {
                return None;
            }
        };
        Some(super::PresetArgMatcher::Flag(vec![flag.to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        // Profile flags belong to `chat`; putting them after the root-level
        // `--v3` shortcut makes clap reject them before chat starts.
        let mut args = vec!["chat".to_owned(), "--v3".to_owned()];
        args.extend(extra_args.iter().cloned());
        Some(super::positional_prompt_argv("kiro-cli", &args, prompt))
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

fn transcript_stat(path: &Path) -> Option<TranscriptStat> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(TranscriptStat {
        mtime_secs: modified.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: modified.subsec_nanos(),
        len: metadata.len(),
    })
}
