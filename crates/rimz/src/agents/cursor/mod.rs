//! Cursor CLI hook adapter.
//!
//! Cursor's native hooks expose session, turn, tool, exit, and compaction-open
//! signals. They expose no local permission/question gate, machine-readable
//! spend, or post-compaction event, so those gaps remain explicit in the
//! descriptor rather than inferred from pane text.

mod install;
mod payloads;
mod transcript;

use std::path::{Path, PathBuf};

#[cfg(test)]
use serde_json::json as test_json;
use serde_json::{Value, json};

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::{
    AgentAdapter, AgentLifecycleObservation, ClassifiedHook, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, classify_agent_hook, locate_binary, sanitize_user_prompt,
};
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;

static CURSOR_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "cursor",
    display_name: "Cursor",
    brand: Brand {
        emblem: None,
        color: 255,
        color_rgb: (0xe8, 0xe8, 0xe8),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Cursor" },
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["Shell", "Write", "Delete"],
        editing: &["Write", "Delete"],
        blocking: &[],
    },
    capabilities: Capabilities {
        blocking_asks: false,
        native_ask_ui: false,
        rich_context: false,
        transcript_tail_context: true,
        context_usage: true,
        account_spend: false,
        subagents: false,
        background_tasks: false,
        registers_lazily: false,
        daemon_hooked_sessions: false,
        hook_install: true,
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
    coverage: CURSOR_COVERAGE,
    lifecycle_hooks: CURSOR_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["cursor-agent", "agent"],
    bin_names: &["cursor-agent", "agent"],
    extra_bin_dirs: &[],
    activity_events: &[
        "sessionStart",
        "beforeSubmitPrompt",
        "postToolUse",
        "postToolUseFailure",
        "afterAgentResponse",
        "stop",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const CURSOR_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "sessionStart/beforeSubmitPrompt/stop including native interruption",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Unsupported {
            reason: "no local permission hook; ACP-only",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "no local plan-approval hook; ACP-only",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Unsupported {
            reason: "no local question hook; ACP-only",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "no observable local ask surface",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Partial {
            via: "preCompact opens; the next lifecycle signal closes the bracket",
            gap: "no post-compaction event; landing status and phase held",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "subagentStop omits the child id supplied by subagentStart",
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
        ConcernCoverage::Wired { via: "sessionEnd" },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "turn boundaries + stall window",
            gap: "no idle Notification hook",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "preCompact occupancy plus stop token composition",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "stop reports per-turn tokens but no model-priced dollar feed",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Unsupported {
            reason: "no out-of-band transport with a published schema",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.cursor/hooks.json merge",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "status/about JSON schemas and usage store are unpublished",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "no remote-control surface",
        },
    ),
];

const CURSOR_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "sessionStart",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "beforeSubmitPrompt",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "stop" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "postToolUse",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Absent {
            reason: "no local permission/question/plan hook; ACP-only",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "subagentStop has no child id",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "subagentStop has no child id",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "preCompact",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Derived {
            via: "next lifecycle signal closes the bracket in step + display-window expiry",
            gap: "no post-compaction event; landing status and phase held",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Native {
            event: "sessionEnd",
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

const LIFECYCLE_EVENTS: &[&str] = &[
    "sessionStart",
    "beforeSubmitPrompt",
    "postToolUse",
    "postToolUseFailure",
    "afterAgentResponse",
    "stop",
    "sessionEnd",
    "preCompact",
];
const WIRED_EVENTS: &[&str] = LIFECYCLE_EVENTS;
pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source cursor";
pub(super) const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source cursor";

#[derive(Clone, Debug, Default)]
pub struct CursorAdapter;

impl AgentAdapter for CursorAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &CURSOR_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, _payload: &Value) -> ClassifiedHook {
        classify_agent_hook(event_name, None, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        WIRED_EVENTS.to_vec()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};
        vec![
            ClassificationSample::new(
                "sessionStart",
                test_json!({ "conversation_id": "c1", "session_id": "c1", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "beforeSubmitPrompt",
                test_json!({ "conversation_id": "c1", "prompt": "fix it", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUse",
                test_json!({ "conversation_id": "c1", "tool_name": "Write", "cwd": "/tmp", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUseFailure",
                test_json!({ "conversation_id": "c1", "tool_name": "Shell", "failure_type": "error", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "afterAgentResponse",
                test_json!({ "conversation_id": "c1", "text": "done", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "stop",
                test_json!({ "conversation_id": "c1", "status": "completed", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "sessionEnd",
                test_json!({ "conversation_id": "c1", "reason": "quit", "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "preCompact",
                test_json!({ "conversation_id": "c1", "trigger": "manual", "context_usage_percent": 83.2, "context_window_size": 200000, "cursor_version": "1.7" }),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    fn render_neutral(&self, event_name: &str) -> Result<Option<Value>> {
        Ok(LIFECYCLE_EVENTS.contains(&event_name).then(|| json!({})))
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        let signal = match event_name {
            "sessionStart" => LifecycleSignal::Registered,
            "beforeSubmitPrompt" => LifecycleSignal::TurnStarted,
            "postToolUse" if self.descriptor().tool_mutates(payload) => LifecycleSignal::ToolUsed {
                mutates: true,
                edits: self.descriptor().tool_edits_files(payload),
            },
            "stop" if parsed.stop_outcome() == payloads::StopOutcome::Aborted => {
                LifecycleSignal::TurnInterrupted
            }
            "stop" => LifecycleSignal::TurnEnded {
                errored: parsed.stop_outcome() == payloads::StopOutcome::Error,
                parked_on_background: false,
            },
            "sessionEnd" => LifecycleSignal::Ended,
            "preCompact" => LifecycleSignal::Compacting,
            _ => return None,
        };
        let agent_id = parsed
            .conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        let prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        observation.task = prompt.clone();
        observation.prompt = prompt;
        let effort = parsed.model_param("effort").map(ToOwned::to_owned);
        observation.transcript_path = parsed.transcript_path;
        observation.launch.model = parsed.model_id.or(parsed.model);
        observation.launch.effort = effort;
        observation.context_pct = parsed
            .context_usage_percent
            .filter(|value| value.is_finite())
            .map(|value| value.round().clamp(0.0, 100.0) as u8);
        observation.context_window = parsed.context_window_size;
        if event_name == "stop" {
            observation.fresh_input_tokens = parsed.input_tokens;
            observation.output_tokens = parsed.output_tokens;
            observation.cache_read_input_tokens = parsed.cache_read_tokens;
            observation.cache_write_input_tokens = parsed.cache_write_tokens;
        }
        Some(observation)
    }

    fn observe_assistant_message(&self, event_name: &str, payload: &Value) -> Option<String> {
        (event_name == "afterAgentResponse")
            .then(|| payloads::parse_payload(payload).text)
            .flatten()
    }

    fn local_context_refresh(
        &self,
        trigger: super::RefreshTrigger<'_>,
        ctx: &super::LocalContextRefreshCtx<'_>,
    ) -> Option<super::LocalContextRefresh> {
        if let super::RefreshTrigger::Hook(event) = trigger
            && !matches!(
                event,
                "sessionStart"
                    | "beforeSubmitPrompt"
                    | "postToolUse"
                    | "postToolUseFailure"
                    | "afterAgentResponse"
                    | "stop"
                    | "preCompact"
            )
        {
            return None;
        }
        transcript::refresh(ctx)
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        transcript::resolve_transcript(session_id, None, prior_path)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "sessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "beforeSubmitPrompt" | "stop")
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        Some(vec![bin, "--resume".to_owned(), session_id.to_owned()])
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Plan => vec!["--mode=plan".to_owned()],
            PermissionMode::Auto => vec!["--auto-review".to_owned()],
            PermissionMode::Yolo => vec![
                "--force".to_owned(),
                "--sandbox".to_owned(),
                "disabled".to_owned(),
            ],
        }
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/summarize")
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|model| !model.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        for (present, field) in [
            (
                preset
                    .effort
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "effort",
            ),
            (preset.system_prompt_file.is_some(), "system-prompt-file"),
            (
                preset.append_system_prompt_file.is_some(),
                "append-system-prompt-file",
            ),
        ] {
            if present {
                return Err(super::PresetErr::UnsupportedField {
                    agent: "cursor",
                    field,
                });
            }
        }
        Ok(argv)
    }

    fn preset_arg_matcher(&self, field: super::PresetField) -> Option<super::PresetArgMatcher> {
        (field == super::PresetField::Model)
            .then(|| super::PresetArgMatcher::Flag(vec!["--model".to_owned()]))
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let bin = locate_binary(self.descriptor())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent".to_owned());
        Some(super::positional_prompt_argv(&bin, extra_args, prompt))
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install_into(&install::cursor_hooks_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview_at(&install::cursor_hooks_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall_from(&install::cursor_hooks_path()?)
    }

    fn hooks_installed(&self) -> bool {
        install::cursor_hooks_path().is_ok_and(|path| install::hooks_installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        install::cursor_hooks_path().is_ok_and(|path| install::managed_artifacts_at(&path))
    }
}

#[cfg(test)]
mod tests;
