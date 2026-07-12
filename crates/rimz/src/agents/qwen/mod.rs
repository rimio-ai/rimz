//! Qwen Code hook, context, account, and spend adapter.

pub(crate) mod account;
mod ask;
mod install;
pub(crate) mod payloads;
pub(crate) mod spend;
mod statusline;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use self::install::{
    hooks_installed_at, install_into, managed_artifacts_at, preview_install_at, qwen_settings_path,
    read_existing_json, uninstall_from, wrapped_status_line_command_from,
};
use self::payloads::{
    QwenStopError, parse_compact, parse_session_start, parse_stop, parse_stop_failure,
    parse_subagent, parse_tool_use, parse_user_prompt_submit,
};
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationConcern,
    PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey, ToolClassification,
};
use super::hook_types::{BackgroundTask, SessionSource};
use super::lifecycle::{AskKind, LifecycleSignal, LifecycleSignalKind};
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::transcript::{TranscriptMessage, TranscriptRole};
use super::{
    AgentAdapter, AgentContext, AgentLifecycleObservation, AgentTurnError, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, PresetErr, Result, RootIdentity,
    SessionOrigin, SubagentIdentity, TurnErrorClass, classify_agent_hook, non_empty_trimmed,
    optional_payload_string, read_transcript_tail, resolve_root_identity,
    resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
use crate::harness::run::PermissionMode;
use crate::transcript::AskQuestion;

static QWEN_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "qwen",
    display_name: "Qwen Code",
    bin_names: &["qwen"],
    brand: Brand {
        emblem: None,
        color: 99,
        color_rgb: (0x61, 0x5c, 0xed),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    tools: ToolClassification {
        mutating: &[
            "edit",
            "write_file",
            "notebook_edit",
            "replace",
            "run_shell_command",
            "save_memory",
        ],
        editing: &["edit", "write_file", "notebook_edit", "replace"],
        blocking: &[
            ("exit_plan_mode", AskKind::PlanApproval),
            ("ask_user_question", AskKind::Question),
        ],
    },
    capabilities: Capabilities {
        blocking_asks: true,
        native_ask_ui: true,
        rich_context: true,
        transcript_tail_context: false,
        context_usage: true,
        account_spend: true,
        subagents: true,
        background_tasks: true,
        registers_lazily: false,
        daemon_hooked_sessions: false,
        hook_install: true,
        realtime_usage: RealtimeUsageChannel {
            covers_account_while_live: false,
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: QWEN_COVERAGE,
    lifecycle_hooks: QWEN_LIFECYCLE_HOOKS,
    default_context_window: None,
    // Qwen routes across multiple provider protocols, each with its own model
    // catalog. Preserve the model selected in Qwen settings unless a Rimz
    // profile explicitly supplies `--model`.
    default_model: None,
    process_names: &["qwen", "node"],
    extra_bin_dirs: &[],
    activity_events: &[
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
    ],
    hook_install_unavailable: None,
    thread_key: ThreadKey::PerFile,
};

const QWEN_COVERAGE: &[(IntegrationConcern, ConcernCoverage)] = &[
    (
        IntegrationConcern::TurnLifecycle,
        ConcernCoverage::Wired {
            via: "SessionStart/UserPromptSubmit/Stop/StopFailure",
        },
    ),
    (
        IntegrationConcern::Permission,
        ConcernCoverage::Wired {
            via: "PermissionRequest",
        },
    ),
    (
        IntegrationConcern::PlanApproval,
        ConcernCoverage::Wired {
            via: "PreToolUse:exit_plan_mode",
        },
    ),
    (
        IntegrationConcern::UserQuestion,
        ConcernCoverage::Wired {
            via: "PreToolUse:ask_user_question",
        },
    ),
    (
        IntegrationConcern::Answer,
        ConcernCoverage::Unsupported {
            reason: "native dialog answering not wired; answer in the pane",
        },
    ),
    (
        IntegrationConcern::Compaction,
        ConcernCoverage::Wired {
            via: "PreCompact/PostCompact/SessionStart:compact",
        },
    ),
    (
        IntegrationConcern::Subagents,
        ConcernCoverage::Wired {
            via: "SubagentStart/SubagentStop",
        },
    ),
    (
        IntegrationConcern::BackgroundParking,
        ConcernCoverage::Wired {
            via: "Stop.background_tasks/crons",
        },
    ),
    (
        IntegrationConcern::SessionEnd,
        ConcernCoverage::Wired { via: "SessionEnd" },
    ),
    (
        IntegrationConcern::IdleNotification,
        ConcernCoverage::Wired {
            via: "Notification audit hook",
        },
    ),
    (
        IntegrationConcern::ContextUsage,
        ConcernCoverage::Wired {
            via: "transcript tail/statusline",
        },
    ),
    (
        IntegrationConcern::RealtimeCost,
        ConcernCoverage::Partial {
            via: "priced transcript tokens",
            gap: "multi-provider billing; rewind branch pruning is not reconstructed; off-book models cost $0",
        },
    ),
    (
        IntegrationConcern::RichContext,
        ConcernCoverage::Wired { via: "statusline" },
    ),
    (
        IntegrationConcern::HookInstall,
        ConcernCoverage::Wired {
            via: "~/.qwen/settings.json",
        },
    ),
    (
        IntegrationConcern::AccountSpend,
        ConcernCoverage::Partial {
            via: "credential presence/transcripts",
            gap: "multi-provider billing; rewind branch pruning is not reconstructed; subscription metering unknown",
        },
    ),
    (
        IntegrationConcern::RemoteControl,
        ConcernCoverage::Unsupported {
            reason: "ACP/daemon mode is outside the pane-first adapter",
        },
    ),
];

const QWEN_LIFECYCLE_HOOKS: &[(LifecycleSignalKind, HookCoverage)] = &[
    (
        LifecycleSignalKind::Registered,
        HookCoverage::Native {
            event: "SessionStart",
        },
    ),
    (
        LifecycleSignalKind::TurnStarted,
        HookCoverage::Native {
            event: "UserPromptSubmit",
        },
    ),
    (
        LifecycleSignalKind::TurnEnded,
        HookCoverage::Native { event: "Stop" },
    ),
    (
        LifecycleSignalKind::ToolUsed,
        HookCoverage::Native {
            event: "PostToolUse",
        },
    ),
    (
        LifecycleSignalKind::AwaitingInput,
        HookCoverage::Native {
            event: "PermissionRequest",
        },
    ),
    (
        LifecycleSignalKind::SubagentStarted,
        HookCoverage::Native {
            event: "SubagentStart",
        },
    ),
    (
        LifecycleSignalKind::SubagentStopped,
        HookCoverage::Native {
            event: "SubagentStop",
        },
    ),
    (
        LifecycleSignalKind::Compacting,
        HookCoverage::Native {
            event: "PreCompact",
        },
    ),
    (
        LifecycleSignalKind::CompactionEnded,
        HookCoverage::Native {
            event: "PostCompact",
        },
    ),
    (
        LifecycleSignalKind::Ended,
        HookCoverage::Native {
            event: "SessionEnd",
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

const QWEN_HOOK_TIMEOUT_MS: u64 = 10_000;
const BLOCKING_TOOL_MATCHER: &str = "exit_plan_mode|ask_user_question";
const INSTALLED_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("SessionEnd", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("StopFailure", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreToolUse", Some(BLOCKING_TOOL_MATCHER)),
    ("PostToolUse", None),
    ("PostToolUseFailure", None),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("PreCompact", None),
    ("PostCompact", None),
];
const LIFECYCLE_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "Notification",
    "PermissionRequest",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
];
const BLOCKING_EVENTS: &[(&str, Option<&str>)] = &[
    ("PermissionRequest", None),
    ("PreToolUse", Some(BLOCKING_TOOL_MATCHER)),
];
const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source qwen";
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source qwen";
const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source qwen";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source qwen";

#[derive(Clone, Debug, Default)]
pub struct QwenAdapter;

impl AgentAdapter for QwenAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &QWEN_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let ask_kind = match event_name {
            "PermissionRequest" => self
                .descriptor()
                .blocking_tool_kind(parse_tool_use(payload).tool_name.as_deref())
                .is_none()
                .then_some(AskKind::Permission),
            "PreToolUse" => self
                .descriptor()
                .blocking_tool_kind(parse_tool_use(payload).tool_name.as_deref()),
            _ => None,
        };
        classify_agent_hook(event_name, ask_kind, LIFECYCLE_EVENTS)
    }

    #[cfg(test)]
    fn installed_hook_events(&self) -> Vec<&'static str> {
        INSTALLED_EVENTS.iter().map(|(event, _)| *event).collect()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};
        let mut samples = INSTALLED_EVENTS.iter().map(|(event, _)| {
            let (payload, class, kind) = match *event {
                "PermissionRequest" => (serde_json::json!({"tool_name":"run_shell_command"}), AgentHookClass::AwaitingUser, Some(AskKind::Permission)),
                "PreToolUse" => (serde_json::json!({"tool_name":"ask_user_question"}), AgentHookClass::AwaitingUser, Some(AskKind::Question)),
                "SubagentStart" | "SubagentStop" => (serde_json::json!({"session_id":"parent","agent_id":"child","agent_type":"review"}), AgentHookClass::Lifecycle, None),
                _ => (serde_json::json!({"session_id":"sess-1"}), AgentHookClass::Lifecycle, None),
            };
            ClassificationSample::new(event, payload, class, kind)
        }).collect::<Vec<_>>();
        samples.push(ClassificationSample::new(
            "PreToolUse",
            serde_json::json!({"session_id":"sess-1","tool_name":"exit_plan_mode"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
        ));
        samples.push(ClassificationSample::new(
            "PermissionRequest",
            serde_json::json!({"session_id":"sess-1","tool_name":"ask_user_question"}),
            AgentHookClass::Lifecycle,
            None,
        ));
        samples
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "sess-1",
            file_name: "sess-1.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"uuid":"msg-1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,"candidatesTokenCount":10,"thoughtsTokenCount":5,"totalTokenCount":115}}"#,
            ),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    fn ends_session(&self, event_name: &str) -> bool {
        event_name == "SessionEnd"
    }
    fn moves_on(&self, event_name: &str) -> bool {
        matches!(event_name, "Stop" | "UserPromptSubmit")
    }

    fn ask_question_detail(&self, event_name: &str, payload: &Value) -> Option<Vec<AskQuestion>> {
        (event_name == "PreToolUse")
            .then(|| parse_tool_use(payload))
            .and_then(|tool| {
                ask::question_detail(tool.tool_name.as_deref()?, tool.tool_input.as_ref()?)
            })
    }

    fn ask_detail(&self, event_name: &str, payload: &Value) -> Option<String> {
        if event_name == "PermissionRequest" {
            return ask::permission_detail(payload);
        }
        self.ask_question_detail(event_name, payload)?
            .first()?
            .question
            .lines()
            .next()
            .map(ToOwned::to_owned)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let signal = lifecycle_signal(self.descriptor(), event_name, payload)?;
        let (agent_id, parent_agent_id) = observation_identity(event_name, payload)?;
        let transcript_path = optional_payload_string(payload, &["transcript_path"]);
        let usage = transcript_path
            .as_deref()
            .map(usage_from_transcript)
            .unwrap_or_default();
        let start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let stop = (event_name == "Stop").then(|| parse_stop(payload));
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        observation.transcript_path = transcript_path;
        observation.launch.model = start
            .as_ref()
            .and_then(|value| value.common.model.clone())
            .or_else(|| optional_payload_string(payload, &["model"]))
            .or(usage.model);
        observation.prompt = (event_name == "UserPromptSubmit")
            .then(|| parse_user_prompt_submit(payload))
            .and_then(|value| sanitize_user_prompt(value.prompt.as_deref()));
        observation.task = if matches!(event_name, "SubagentStart" | "SubagentStop") {
            parse_subagent(payload).common.agent_type
        } else {
            sanitize_user_prompt(optional_payload_string(payload, &["task"]).as_deref())
        };
        observation.context_pct = stop
            .as_ref()
            .and_then(|value| value.context_usage)
            .map(|ratio| (ratio * 100.0).round().clamp(0.0, 100.0) as u8);
        observation.context_window = stop
            .as_ref()
            .and_then(|value| value.context_limit)
            .or(usage.context_window);
        observation.total_tokens = payload_total_tokens(
            payload,
            stop.as_ref()
                .and_then(|value| value.input_tokens)
                .or(usage.total_tokens),
        );
        if event_name == "SessionStart"
            && start.as_ref().is_some_and(|value| {
                matches!(value.source, SessionSource::Startup | SessionSource::Clear)
            })
        {
            observation.origin = Some(SessionOrigin::Fresh);
        }
        Some(observation)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<AgentContext> {
        if !payload.is_object() {
            return None;
        }
        serde_json::from_value::<statusline::StatuslinePayload>(payload.clone())
            .ok()
            .map(|value| value.into_context(source, Timestamp::now()))
    }

    fn observe_turn_error_from_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentTurnError> {
        if event_name != "StopFailure" {
            return None;
        }
        let failure = parse_stop_failure(payload);
        let label = failure
            .last_assistant_message
            .as_deref()
            .and_then(non_empty_trimmed)
            .map(|text| text.chars().take(80).collect());
        let class = match failure.error {
            QwenStopError::RateLimit => TurnErrorClass::PausedRateLimit,
            QwenStopError::ServerError => TurnErrorClass::PausedOverloaded,
            _ => TurnErrorClass::classify_label(label.as_deref()),
        };
        Some(AgentTurnError {
            class,
            at: Timestamp::now(),
            label,
        })
    }

    fn last_assistant_message(
        &self,
        _event_name: &str,
        payload: &Value,
        _observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        optional_payload_string(payload, &["last_assistant_message", "assistant_message"])
            .as_deref()
            .and_then(non_empty_trimmed)
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        parse_messages(lines)
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        let root = read_existing_json(&qwen_settings_path().ok()?).ok()?;
        wrapped_status_line_command_from(&root)
    }
    fn install_hooks(&self) -> Result<HookInstallReport> {
        install_into(&qwen_settings_path()?)
    }
    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        preview_install_at(&qwen_settings_path()?)
    }
    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        uninstall_from(&qwen_settings_path()?)
    }
    fn hooks_installed(&self) -> bool {
        qwen_settings_path().is_ok_and(|path| hooks_installed_at(&path))
    }
    fn managed_hook_artifacts_present(&self) -> bool {
        qwen_settings_path().is_ok_and(|path| managed_artifacts_at(&path))
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }
    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::all_jsonl_files()
    }
    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse_qwen_spend(path, resume.map_or(0, |cursor| cursor.offset), prices)
    }

    fn resume_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec!["qwen".into(), "--resume".into(), session_id.into()])
    }
    fn fork_command(&self, session_id: &str, _cwd: &Path) -> Option<Vec<String>> {
        Some(vec![
            "qwen".into(),
            "--resume".into(),
            session_id.into(),
            "--fork-session".into(),
        ])
    }
    fn permission_args(&self, mode: PermissionMode) -> Vec<String> {
        let value = match mode {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => return Vec::new(),
            PermissionMode::Auto => "auto-edit",
            PermissionMode::Yolo => "yolo",
        };
        vec!["--approval-mode".into(), value.into()]
    }
    fn max_turns_args(&self, limit: u32) -> Option<Vec<String>> {
        Some(vec!["--max-session-turns".into(), limit.to_string()])
    }
    fn compact_command(&self) -> Option<&'static str> {
        Some("/compress")
    }
    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["qwen".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
            argv.extend(["-i".to_owned(), prompt.to_owned()]);
        }
        Some(argv)
    }
    fn render_preset(
        &self,
        preset: &super::LaunchPreset,
    ) -> std::result::Result<Vec<String>, PresetErr> {
        let mut argv = Vec::new();
        if let Some(model) = preset.model.as_deref().filter(|value| !value.is_empty()) {
            argv.extend(["--model".into(), model.into()]);
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
                return Err(PresetErr::UnsupportedField {
                    agent: "qwen",
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
}

fn lifecycle_signal(
    descriptor: &AgentDescriptor,
    event_name: &str,
    payload: &Value,
) -> Option<LifecycleSignal> {
    match event_name {
        "SessionStart" => Some(
            if parse_session_start(payload).source == SessionSource::Compact {
                LifecycleSignal::CompactionEnded { auto: None }
            } else {
                LifecycleSignal::Registered
            },
        ),
        "UserPromptSubmit" => Some(LifecycleSignal::TurnStarted),
        "PreToolUse" => Some(
            match descriptor.blocking_tool_kind(parse_tool_use(payload).tool_name.as_deref()) {
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
        ),
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: descriptor.tool_mutates(payload),
            edits: descriptor.tool_edits_files(payload),
        }),
        "PostToolUseFailure" => Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }),
        "PermissionRequest" => descriptor
            .blocking_tool_kind(parse_tool_use(payload).tool_name.as_deref())
            .is_none()
            .then_some(LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
            }),
        "Stop" => {
            let stop = parse_stop(payload);
            Some(LifecycleSignal::TurnEnded {
                errored: stop_payload_errored(payload),
                parked_on_background: has_pending_work(&stop.background_tasks, &stop.crons),
            })
        }
        "StopFailure" => Some(LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }),
        "SubagentStart" => Some(LifecycleSignal::SubagentStarted),
        "SubagentStop" => Some(LifecycleSignal::SubagentStopped { errored: false }),
        "PreCompact" => Some(LifecycleSignal::Compacting),
        "PostCompact" => Some(LifecycleSignal::CompactionEnded {
            auto: parse_compact(payload).trigger.auto_flag(),
        }),
        "SessionEnd" => Some(LifecycleSignal::Ended),
        _ => None,
    }
}

fn has_pending_work(tasks: &[BackgroundTask], crons: &[payloads::QwenCron]) -> bool {
    tasks.iter().any(|task| {
        task.status
            .as_deref()
            .is_none_or(|status| !matches!(status, "completed" | "failed"))
    }) || crons.iter().any(|cron| {
        cron.status
            .as_deref()
            .is_none_or(|status| !matches!(status, "completed" | "failed"))
    })
}

fn observation_identity(
    event_name: &str,
    payload: &Value,
) -> Option<(
    Option<crate::ids::AgentSessionId>,
    Option<crate::ids::AgentSessionId>,
)> {
    if matches!(event_name, "SubagentStart" | "SubagentStop") {
        let child = parse_subagent(payload);
        return match resolve_subagent_identity(
            "qwen",
            event_name,
            child.common.agent_id.as_deref(),
            child.common.common.session_id.as_deref(),
            payload,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => Some((Some(agent_id), Some(parent_agent_id))),
            SubagentIdentity::Quarantined => None,
        };
    }
    match resolve_root_identity(
        "qwen",
        event_name,
        optional_payload_string(payload, &["agent_id"]).as_deref(),
        optional_payload_string(payload, &["session_id"]).as_deref(),
    ) {
        RootIdentity::Root { agent_id } => Some((agent_id, None)),
        RootIdentity::ForeignChild => None,
    }
}

/// Normalize Qwen's session JSONL into main-thread conversation messages,
/// newest last. Qwen persists the Google `Content` shape, so a `user`/`assistant`
/// record's visible text comes from its non-thought `text` parts; tool
/// call/result records, system records, and sidechain/subagent records
/// (`isSidechain` or an `agentId`) stay out of the root stream. This drives
/// `rimz agents history`, `rimz message --wait`, and `-p --stream` reply
/// extraction.
fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let record = serde_json::from_str::<payloads::TranscriptRecord>(line).ok()?;
            if record.is_sidechain == Some(true) || record.agent_id.is_some() {
                return None;
            }
            let role = match record.r#type.as_deref() {
                Some("user") => TranscriptRole::User,
                Some("assistant") => TranscriptRole::Assistant,
                _ => return None,
            };
            let text = non_empty_trimmed(&record.message.visible_text())?;
            Some(TranscriptMessage {
                role,
                at: record.timestamp.as_deref().and_then(|raw| raw.parse().ok()),
                text,
            })
        })
        .collect()
}

#[derive(Default)]
struct TranscriptUsage {
    total_tokens: Option<u64>,
    model: Option<String>,
    context_window: Option<u64>,
}

fn usage_from_transcript(path: &str) -> TranscriptUsage {
    let Some(text) = read_transcript_tail(Path::new(path)) else {
        return TranscriptUsage::default();
    };
    for line in text.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant")
            || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || value.get("agentId").is_some_and(|value| !value.is_null())
        {
            continue;
        }
        let Some(usage) = value.get("usageMetadata") else {
            continue;
        };
        return TranscriptUsage {
            total_tokens: usage.get("totalTokenCount").and_then(Value::as_u64),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            context_window: value.get("contextWindowSize").and_then(Value::as_u64),
        };
    }
    TranscriptUsage {
        total_tokens: Some(0),
        ..TranscriptUsage::default()
    }
}

#[cfg(test)]
mod tests;
