//! Antigravity CLI 1.1.1 launch, local-session, transcript, and live-state adapter.
//!
//! Antigravity's command-hook decision channel has no verified observer-neutral
//! pre-tool result yet. Keep hook installation unsupported; the provider-owned
//! conversation cache and JSONL transcript supply validated pulled truth for
//! process identity, basic turn state, and visible main-thread history.

mod session;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::LifecycleSignalKind;
use super::{
    AgentAdapter, AgentErr, AgentHookClass, ClassifiedHook, HookInstallPreview, HookInstallReport,
    LocalSessionObservation, PresetArgMatcher, PresetErr, PresetField, Result, TranscriptMessage,
};
use crate::harness::run::PermissionMode;

pub const SUPPORTED_VERSION: &str = "1.1.1";
const HOOK_INSTALL_UNAVAILABLE: &str = "Antigravity CLI 1.1.1 observer hooks are deferred until a behavior-preserving PreToolUse/Stop neutral result and statusline callback lifecycle are verified";

static ANTIGRAVITY_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "antigravity",
    display_name: "Antigravity",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x42, 0x85, 0xf4),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
            "run_command",
        ],
        editing: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: false,
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: false,
        context_usage: false,
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
    coverage: ANTIGRAVITY_COVERAGE,
    lifecycle_hooks: ANTIGRAVITY_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["agy"],
    bin_names: &["agy"],
    extra_bin_dirs: &[".local/bin"],
    activity_events: &[],
    hook_install_unavailable: Some(HOOK_INSTALL_UNAVAILABLE),
    thread_key: ThreadKey::PerFile,
};

const ANTIGRAVITY_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Partial {
            via: "provider-owned USER_INPUT and completed PLANNER_RESPONSE transcript records",
            gap: "pulled text-turn boundaries only; failures, waits, and tool activity remain uncaptured",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Unsupported {
            reason: "observer-neutral PreToolUse result is unverified and the transcript omits native prompts",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "artifact status enums and review transitions remain uncaptured",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "ask_question dialog transitions are absent from the verified transcript records",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "no out-of-band answer API or verified native-key planner",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Unsupported {
            reason: "no documented compaction command, event, or transcript marker",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "verified local records expose no stable child identity and parent relation",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "fullyIdle and background-task status require the deferred live channels",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Partial {
            via: "pane liveness + rollup reaper",
            gap: "Antigravity publishes no session-end event",
        },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "completed PLANNER_RESPONSE records + pane liveness",
            gap: "pulled state has no native notification wakeup or failure verdict",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Unsupported {
            reason: "context usage exists only on the deferred custom-statusline channel",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "no machine-readable session-dollar surface",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "custom-statusline install and callback lifecycle remain unverified",
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
            reason: "quota is work-metered and no cumulative billing ledger is published",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no CLI remote-control host is documented",
        },
    ),
];

const ANTIGRAVITY_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Derived {
            via: "workspace conversation cache + validated brain transcript",
            gap: "fresh registration waits for the workspace cache; exact resume binds immediately",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Derived {
            via: "USER_EXPLICIT/USER_INPUT transcript record",
            gap: "pulled provider state, not a realtime callback",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Derived {
            via: "completed MODEL/PLANNER_RESPONSE transcript record",
            gap: "failure, cancellation, and background-work endings remain uncaptured",
        },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Absent {
            reason: "verified visible transcript records expose no completed tool envelope",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Absent {
            reason: "native prompts require deferred hook/statusline verification",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "no verified child identity relation",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "no verified child identity relation",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Absent {
            reason: "no compaction signal",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Absent {
            reason: "no compaction signal",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Derived {
            via: "pane liveness + rollup reaper",
            gap: "no session-end event",
        },
    ),
    (
        LifecycleSignalKind::Lost,
        HookCoverage::Derived {
            via: "rimz exec wrapper",
            gap: "provider callbacks stop with the mux session",
        },
    ),
];

#[derive(Clone, Debug, Default)]
pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &ANTIGRAVITY_DESCRIPTOR
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
        // Hook stdout is Antigravity's decision channel. Stay silent until the
        // exact observer-neutral bytes are proven for the pinned release.
        Ok(None)
    }

    fn discover_local_sessions(&self, workspace: &Path) -> Vec<LocalSessionObservation> {
        session::discover(workspace)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        session::messages(lines)
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| session::valid_transcript(path, session_id)) {
            return Some(path.to_path_buf());
        }
        session::transcript_for_session(session_id)
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<crate::ids::AgentSessionId> {
        session::resumed_session_id(cmdline)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        session::valid_conversation_id(session_id).then(|| {
            vec![
                "agy".to_owned(),
                "--conversation".to_owned(),
                session_id.to_owned(),
            ]
        })
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Auto => vec!["--mode".to_owned(), "accept-edits".to_owned()],
            PermissionMode::Plan => vec!["--mode".to_owned(), "plan".to_owned()],
            PermissionMode::Yolo => vec!["--dangerously-skip-permissions".to_owned()],
        }
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, PresetErr> {
        if preset
            .effort
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return Err(PresetErr::UnsupportedField {
                agent: "antigravity",
                field: "effort",
            });
        }
        if preset.system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: "antigravity",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(PresetErr::UnsupportedField {
                agent: "antigravity",
                field: "append-system-prompt-file",
            });
        }
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: PresetField) -> Option<PresetArgMatcher> {
        (field == PresetField::Model).then(|| PresetArgMatcher::Flag(vec!["--model".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["agy".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.extend(["--prompt-interactive".to_owned(), prompt.to_owned()]);
        }
        Some(argv)
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

    fn hooks_installed(&self) -> bool {
        false
    }
}
