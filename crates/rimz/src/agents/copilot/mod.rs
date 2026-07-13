//! GitHub Copilot CLI command-hook adapter.
//!
//! Copilot's native camelCase command hooks provide lifecycle truth and its
//! synchronous permission/question gates. RimZ owns one whole hook file at
//! `$COPILOT_HOME/hooks/rimz.json`; empty hook stdout preserves Copilot's native
//! decision UI. Per-session events provide conversation history and optional
//! metadata-only OTel chat spans provide the live model/token composition.

mod otel;
mod paths;
pub(crate) mod payloads;
mod transcript;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::lifecycle::{LifecycleSignal, LifecycleSignalKind};
use super::managed_source::ManagedSource;
use super::{
    AgentAdapter, AgentLifecycleObservation, AgentTurnError, AskKind, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, LocalContextRefresh,
    LocalContextRefreshCtx, RefreshTrigger, Result, SessionOrigin, TranscriptMessage,
    TurnErrorClass, classify_agent_hook, sanitize_user_prompt,
};
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;
use crate::transcript::AskQuestion;

static COPILOT_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "copilot",
    display_name: "Copilot",
    brand: Brand {
        emblem: None,
        color: 140,
        color_rgb: (0x89, 0x57, 0xe5),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Copilot" },
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &["bash", "powershell", "create", "edit"],
        editing: &["create", "edit"],
        blocking: &[("ask_user", AskKind::Question)],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: false,
        transcript_tail_context: true,
        context_usage: false,
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
    coverage: COPILOT_COVERAGE,
    lifecycle_hooks: COPILOT_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["copilot", "node"],
    bin_names: &["copilot"],
    extra_bin_dirs: &[],
    activity_events: &[
        "sessionStart",
        "userPromptSubmitted",
        "postToolUse",
        "postToolUseFailure",
        "agentStop",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const COPILOT_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "sessionStart/userPromptSubmitted/agentStop",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "permissionRequest",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Unsupported {
            reason: "plan mode has no approval hook",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "preToolUse(ask_user)",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "no native answer protocol; answer in the pane",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Partial {
            via: "preCompact + next lifecycle signal",
            gap: "no native post-compact hook",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Unsupported {
            reason: "hooks publish no unique child instance id",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Unsupported {
            reason: "no parked-on-background signal",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Wired { via: "sessionEnd" },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Partial {
            via: "agentStop + stall window",
            gap: "notification(agent_idle) is not wired",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Partial {
            via: "optional metadata-only OTel chat spans",
            gap: "latest-call token composition has no context-window denominator",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Unsupported {
            reason: "OTel chat spans expose token counts but no authoritative session cost",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Partial {
            via: "optional metadata-only OTel chat spans",
            gap: "model only; no quota or account metadata",
        },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "$COPILOT_HOME/hooks/rimz.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Unsupported {
            reason: "no machine-readable auth or usage surface",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "remote-control preflight is not wired",
        },
    ),
];

const COPILOT_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "sessionStart",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "userPromptSubmitted",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "agentStop" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "postToolUse",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            event: "permissionRequest",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Absent {
            reason: "hooks publish no unique child instance id",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Absent {
            reason: "hooks publish no unique child instance id",
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
            via: "next lifecycle signal + display-window expiry",
            gap: "no native post-compact hook",
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
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "agentStop",
    "preCompact",
    "errorOccurred",
    "sessionEnd",
];

const WIRED_EVENTS: &[&str] = &[
    "sessionStart",
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "permissionRequest",
    "agentStop",
    "preCompact",
    "errorOccurred",
    "sessionEnd",
];

// If Copilot starts rejecting unknown top-level keys, move the ownership
// marker into the first hook entry's `env` overlay after live verification.
const HOOK_SOURCE: &str = include_str!("hooks.json");

const COPILOT_MANAGED_SOURCE: ManagedSource = ManagedSource {
    agent: "copilot",
    source: HOOK_SOURCE,
    wired_events: WIRED_EVENTS,
    artifact_noun: "hook file",
};

#[derive(Clone, Debug, Default)]
pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &COPILOT_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let parsed = payloads::parse_payload(payload);
        let ask_kind = if event_name == "permissionRequest" {
            Some(AskKind::Permission)
        } else if event_name == "preToolUse" {
            self.descriptor()
                .blocking_tool_kind(parsed.tool_name.as_deref())
        } else {
            None
        };
        classify_agent_hook(event_name, ask_kind, LIFECYCLE_EVENTS)
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
                json!({"sessionId":"sess-1","source":"startup"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "userPromptSubmitted",
                json!({"sessionId":"sess-1","prompt":"fix auth"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "preToolUse",
                json!({"sessionId":"sess-1","toolName":"ask_user","toolArgs":{"question":"Proceed?"}}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Question),
            ),
            ClassificationSample::new(
                "preToolUse",
                json!({"sessionId":"sess-1","toolName":"bash"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUse",
                json!({"sessionId":"sess-1","toolName":"edit"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "postToolUseFailure",
                json!({"sessionId":"sess-1","toolName":"bash","error":"failed"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "permissionRequest",
                json!({"sessionId":"sess-1","toolName":"bash"}),
                AgentHookClass::AwaitingUser,
                Some(AskKind::Permission),
            ),
            ClassificationSample::new(
                "agentStop",
                json!({"sessionId":"sess-1","stopReason":"end_turn"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "preCompact",
                json!({"sessionId":"sess-1","trigger":"auto"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "errorOccurred",
                json!({"sessionId":"sess-1","recoverable":true,"error":{"message":"retry"}}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "sessionEnd",
                json!({"sessionId":"sess-1","reason":"user_exit"}),
                AgentHookClass::Lifecycle,
                None,
            ),
        ]
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse_payload(payload);
        let signal = match event_name {
            "sessionStart"
                if parsed
                    .initial_prompt
                    .as_deref()
                    .is_some_and(|prompt| !prompt.trim().is_empty()) =>
            {
                LifecycleSignal::TurnStarted
            }
            "sessionStart" => LifecycleSignal::Registered,
            "userPromptSubmitted" => LifecycleSignal::TurnStarted,
            "permissionRequest" => LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
            },
            "preToolUse" => match self
                .descriptor()
                .blocking_tool_kind(parsed.tool_name.as_deref())
            {
                Some(kind) => LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                },
                None => LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                },
            },
            "postToolUse" | "postToolUseFailure"
                if parsed
                    .tool_name
                    .as_deref()
                    .is_some_and(|name| self.descriptor().tools.mutating.contains(&name)) =>
            {
                LifecycleSignal::ToolUsed {
                    mutates: true,
                    edits: parsed
                        .tool_name
                        .as_deref()
                        .is_some_and(|name| self.descriptor().tools.editing.contains(&name)),
                }
            }
            "agentStop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "preCompact" => LifecycleSignal::Compacting,
            "sessionEnd" => LifecycleSignal::Ended,
            _ => return None,
        };
        let agent_id = parsed.session_id.clone().map(AgentSessionId::from);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        if let Some(session_id) = parsed.session_id.as_deref() {
            observation.transcript_path = parsed
                .transcript_path
                .as_deref()
                .and_then(|path| paths::validated_transcript_path(Path::new(path), session_id))
                .or_else(|| paths::session_transcript_path(session_id))
                .map(|path| path.to_string_lossy().into_owned());
        }
        if event_name == "sessionStart"
            && matches!(parsed.source.as_deref(), Some("startup" | "new"))
        {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        if event_name == "userPromptSubmitted" {
            observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
            observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
        }
        Some(observation)
    }

    fn observe_turn_error_from_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentTurnError> {
        if event_name != "errorOccurred" {
            return None;
        }
        let parsed = payloads::parse_payload(payload);
        if parsed.recoverable != Some(false) {
            return None;
        }
        let label = parsed
            .error
            .and_then(payloads::CopilotHookError::into_message)
            .map(|message| message.trim().chars().take(500).collect::<String>())
            .filter(|message| !message.is_empty())?;
        let at = parsed
            .timestamp
            .as_ref()
            .and_then(Value::as_i64)
            .and_then(|millis| Timestamp::from_millisecond(millis).ok())
            .unwrap_or_else(Timestamp::now);
        Some(AgentTurnError {
            class: TurnErrorClass::classify_label(Some(&label)),
            at,
            label: Some(label),
        })
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        if event_name != "preToolUse" {
            return None;
        }
        let parsed = payloads::parse_payload(payload);
        if parsed.tool_name.as_deref() != Some("ask_user") {
            return None;
        }
        let tool_args = parsed.tool_args?;
        let args = tool_args.as_object()?;
        let question = ["question", "prompt", "message"]
            .into_iter()
            .find_map(|key| args.get(key).and_then(Value::as_str))
            .or_else(|| args.values().find_map(Value::as_str))?
            .trim();
        if question.is_empty() {
            return None;
        }
        Some(vec![AskQuestion {
            question: question.to_owned(),
            options: Vec::new(),
            multi_select: false,
            has_option_previews: false,
        }])
    }

    fn ask_detail(&self, event_name: &str, payload: &Value) -> Option<String> {
        if event_name == "permissionRequest" {
            return payloads::parse_payload(payload)
                .tool_name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty());
        }
        self.ask_question_detail(event_name, payload)
            .and_then(|questions| questions.into_iter().next())
            .map(|question| question.question)
    }

    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "sessionEnd"
    }

    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "userPromptSubmitted" | "agentStop")
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        _payload: &Value,
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        if event_name != "agentStop" {
            return None;
        }
        transcript::last_assistant_message(Path::new(observation.transcript_path.as_deref()?))
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if let RefreshTrigger::Hook(event_name) = trigger
            && !matches!(
                event_name,
                "sessionStart"
                    | "userPromptSubmitted"
                    | "postToolUse"
                    | "postToolUseFailure"
                    | "agentStop"
            )
        {
            return None;
        }
        otel::refresh(ctx)
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path
            .and_then(|path| paths::validated_transcript_path(path, session_id))
            .filter(|path| path.is_file())
        {
            return Some(path);
        }
        paths::session_transcript_path(session_id)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "copilot".to_owned(),
            "--resume".to_owned(),
            session_id.to_owned(),
        ])
    }

    fn compact_command(&self) -> Option<&'static str> {
        Some("/compact")
    }

    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => Vec::new(),
            PermissionMode::Plan => vec!["--plan".to_owned()],
            PermissionMode::Auto => vec!["--autopilot".to_owned()],
            PermissionMode::Yolo => vec!["--allow-all".to_owned()],
        }
    }

    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, super::PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = preset.effort.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        if preset.system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "copilot",
                field: "system-prompt-file",
            });
        }
        if preset.append_system_prompt_file.is_some() {
            return Err(super::PresetErr::UnsupportedField {
                agent: "copilot",
                field: "append-system-prompt-file",
            });
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
        let mut argv = std::iter::once("copilot".to_owned())
            .chain(extra_args.iter().cloned())
            .collect::<Vec<_>>();
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
            argv.extend(["--interactive".to_owned(), prompt.to_owned()]);
        }
        Some(argv)
    }

    fn ping_args(&self) -> Option<Vec<String>> {
        None
    }

    fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        vec![(
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            "false",
        )]
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        COPILOT_MANAGED_SOURCE.install_into(&paths::hooks_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        COPILOT_MANAGED_SOURCE.preview_at(&paths::hooks_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        COPILOT_MANAGED_SOURCE.uninstall_from(&paths::hooks_path()?)
    }

    fn hooks_installed(&self) -> bool {
        paths::hooks_path().is_ok_and(|path| COPILOT_MANAGED_SOURCE.installed_at(&path))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        self.hooks_installed()
    }
}

#[cfg(test)]
mod tests;
