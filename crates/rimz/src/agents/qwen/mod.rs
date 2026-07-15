//! Qwen Code hook, context, account, and spend adapter.

pub(crate) mod account;
mod alibaba_usage;
mod ask;
mod install;
pub(crate) mod payloads;
mod selection;
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
#[cfg(test)]
use super::AgentHookClass;
use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{
    BackgroundTask, HookRecord, SessionSource, classify_catalog_hook, hook_record,
};
use super::lifecycle::{AskKind, LifecycleSignal};
use super::observation::payload_total_tokens;
use super::pricing::PriceBook;
use super::transcript::{TranscriptMessage, TranscriptRole};
use super::{
    AgentAdapter, AgentContext, AgentLifecycleObservation, AgentTurnError, ClassifiedHook,
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, RootIdentity,
    SessionOrigin, SubagentIdentity, TurnErrorClass, non_empty_trimmed, optional_payload_string,
    resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
#[cfg(test)]
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
        native_ask_ui: true,
        transcript_tail_context: false,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
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
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("qwen"),
        fixed_args: &[],
        prompt: super::PromptStyle::Flag("-i"),
        resume: Some(super::SessionCommand {
            before_id: &["qwen", "--resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["qwen", "--resume"],
            after_id: &["--fork-session"],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--approval-mode", "auto-edit"],
            yolo: &["--approval-mode", "yolo"],
            plan: &["--approval-mode", "plan"],
        },
        ping_args: None,
        max_turn_flag: Some("--max-session-turns"),
        compact_command: Some("/compress"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const QWEN_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop/StopFailure",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "PreToolUse:exit_plan_mode",
    },
    user_question: ConcernCoverage::Wired {
        via: "PreToolUse:ask_user_question",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native dialog answering not wired; answer in the pane",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact/SessionStart:compact",
    },
    subagents: ConcernCoverage::Wired {
        via: "SubagentStart/SubagentStop",
    },
    background_parking: ConcernCoverage::Wired {
        via: "Stop.background_tasks/crons",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Wired {
        via: "Notification audit hook",
    },
    context_usage: ConcernCoverage::Wired {
        via: "transcript tail/statusline",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "statusline model metrics and priced transcript tokens",
        gap: "multi-provider billing; sidechain branch pruning is not reconstructed; off-book models cost $0",
    },
    rich_context: ConcernCoverage::Wired { via: "statusline" },
    hook_install: ConcernCoverage::Wired {
        via: "~/.qwen/settings.json",
    },
    account_spend: ConcernCoverage::Partial {
        via: "effective provider credentials/Alibaba quota/transcripts",
        gap: "multi-provider billing; sidechain branch pruning is not reconstructed; Alibaba quota is experimental and display-only; other subscription metering is unknown",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "ACP/daemon mode is outside the pane-first adapter",
    },
};

const QWEN_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "SessionStart",
    },
    turn_started: HookCoverage::Native {
        event: "UserPromptSubmit",
    },
    turn_ended: HookCoverage::Native { event: "Stop" },
    tool_used: HookCoverage::Native {
        event: "PostToolUse",
    },
    awaiting_input: HookCoverage::Native {
        event: "PermissionRequest",
    },
    subagent_started: HookCoverage::Native {
        event: "SubagentStart",
    },
    subagent_stopped: HookCoverage::Native {
        event: "SubagentStop",
    },
    compacting: HookCoverage::Native {
        event: "PreCompact",
    },
    compaction_ended: HookCoverage::Native {
        event: "PostCompact",
    },
    ended: HookCoverage::Native {
        event: "SessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

const QWEN_HOOK_TIMEOUT_MS: u64 = 10_000;
const BLOCKING_TOOL_MATCHER: &str = "exit_plan_mode|ask_user_question";
const QWEN_HOOKS: &[HookRecord] = &[
    hook_record!(
        "SessionStart",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "SessionEnd",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "UserPromptSubmit",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "Stop",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "StopFailure",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "Notification",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "PermissionRequest",
        None,
        true,
        true,
        r#"{"tool_name":"run_shell_command"}"#,
        AgentHookClass::AwaitingUser,
        Some(AskKind::Permission)
    ),
    hook_record!(
        "PreToolUse",
        Some(BLOCKING_TOOL_MATCHER),
        true,
        true,
        r#"{"tool_name":"ask_user_question"}"#,
        AgentHookClass::AwaitingUser,
        Some(AskKind::Question)
    ),
    hook_record!(
        "PostToolUse",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "PostToolUseFailure",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "SubagentStart",
        None,
        true,
        false,
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "SubagentStop",
        None,
        true,
        false,
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "PreCompact",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
    hook_record!(
        "PostCompact",
        None,
        true,
        false,
        r#"{"session_id":"sess-1"}"#,
        AgentHookClass::Lifecycle,
        None
    ),
];
const RIMZ_HOOK_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source qwen";
const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source qwen";
const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source qwen";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source qwen";
const STATUS_LINE: super::managed_statusline::ManagedStatusLineSpec =
    super::managed_statusline::ManagedStatusLineSpec {
        key_path: &["ui", "statusLine"],
        command: STATUS_LINE_COMMAND,
        command_marker: RIMZ_STATUS_LINE_MARKER,
        rendering_options: super::managed_statusline::RenderingOptions::All,
        wrap_policy: super::managed_statusline::WrapPolicy::CommandMode,
        required_for_install: true,
    };

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
        classify_catalog_hook(QWEN_HOOKS, event_name, ask_kind)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        QWEN_HOOKS.iter().map(|hook| hook.event).collect()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};
        let mut samples = QWEN_HOOKS
            .iter()
            .map(|hook| {
                ClassificationSample::new(
                    hook.event,
                    serde_json::from_str(hook.test_payload).expect("valid catalog payload"),
                    hook.test_class,
                    hook.test_ask,
                )
            })
            .collect::<Vec<_>>();
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

    #[cfg(test)]
    fn context_cost_fixture(&self) -> Option<super::ContextCostFixture> {
        Some(super::ContextCostFixture {
            payload: serde_json::json!({
                "metrics": {
                    "models": {
                        "qwen3-coder-plus": {
                            "tokens": {
                                "prompt": 100,
                                "completion": 20,
                                "total": 120,
                                "cached": 30,
                                "thoughts": 5
                            }
                        }
                    }
                }
            }),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
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
        let usage = matches!(event_name, "SessionStart" | "Stop")
            .then(|| transcript_path.as_deref().map(usage_from_transcript))
            .flatten()
            .unwrap_or_default();
        let start = (event_name == "SessionStart").then(|| parse_session_start(payload));
        let stop = (event_name == "Stop").then(|| parse_stop(payload));
        let transcript_is_current = stop
            .as_ref()
            .and_then(|value| value.input_tokens)
            .is_none_or(|prompt| usage.prompt_tokens == Some(prompt));
        let accepted_usage = transcript_is_current.then_some(&usage);
        let mut observation =
            AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
        observation.parent_agent_id = parent_agent_id;
        observation.transcript_path = transcript_path;
        observation.launch.model = start
            .as_ref()
            .and_then(|value| value.common.model.clone())
            .or_else(|| optional_payload_string(payload, &["model"]))
            .or_else(|| accepted_usage.and_then(|usage| usage.model.clone()));
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
            .or_else(|| accepted_usage.and_then(|usage| usage.context_window));
        let transcript_total = accepted_usage.and_then(|usage| usage.total_tokens);
        let fallback_total = transcript_total
            .filter(|total| *total > 0)
            .or_else(|| stop.as_ref().and_then(|value| value.input_tokens))
            .or(transcript_total);
        observation.total_tokens = payload_total_tokens(payload, fallback_total);
        observation.cache_read_input_tokens =
            accepted_usage.and_then(|usage| usage.cache_read_input_tokens);
        observation.fresh_input_tokens = accepted_usage.and_then(|usage| usage.fresh_input_tokens);
        observation.output_tokens = accepted_usage.and_then(|usage| usage.output_tokens);
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

    fn context_cost(&self, payload: &Value, prices: &super::PriceBook) -> Option<super::AgentCost> {
        if !payload.is_object() {
            return None;
        }
        serde_json::from_value::<statusline::StatuslinePayload>(payload.clone())
            .ok()?
            .cost(prices)
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

    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        parse_physical_assistant_messages(new_lines)
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
    fn probe_account_usage(&self) -> super::AccountUsageProbe {
        match selection::resolve() {
            selection::SelectionState::Found(selection) => alibaba_usage::probe(selection),
            selection::SelectionState::LoggedOut => {
                super::AccountUsageProbe::NoCredentials(Default::default())
            }
            selection::SelectionState::Unavailable => {
                super::AccountUsageProbe::Failed(Default::default())
            }
        }
    }
    fn account_usage_identity(&self) -> Option<super::AccountUsageIdentity> {
        Some(match selection::resolve() {
            selection::SelectionState::Found(selection) => selection.account_usage_identity(),
            selection::SelectionState::LoggedOut | selection::SelectionState::Unavailable => {
                super::AccountUsageIdentity::default()
            }
        })
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
        spend::parse_qwen_spend(path, resume, prices)
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
/// complete `rimz agents history` replay. Incremental reply extraction uses
/// the physical parser below because an appended page may omit its ancestors.
fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    payloads::fold_transcript(lines)
        .active_root()
        .filter_map(|record| {
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

fn parse_physical_assistant_messages(lines: &str) -> Vec<String> {
    lines
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<payloads::TranscriptRecord>(line).ok())
        .filter(|record| {
            record.r#type.as_deref() == Some("assistant")
                && record.is_sidechain != Some(true)
                && record.agent_id.is_none()
        })
        .filter_map(|record| non_empty_trimmed(&record.message.visible_text()))
        .collect()
}

#[derive(Default)]
struct TranscriptUsage {
    total_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    model: Option<String>,
    context_window: Option<u64>,
}

fn usage_from_transcript(path: &str) -> TranscriptUsage {
    let Ok(text) = std::fs::read_to_string(path) else {
        return TranscriptUsage::default();
    };
    let folded = payloads::fold_transcript(&text);
    if let Some(record) = folded.latest_active_assistant_with_usage() {
        let usage = record
            .usage_metadata
            .as_ref()
            .expect("latest active assistant is filtered to records with usage metadata");
        let prompt_tokens = usage.prompt_token_count;
        return TranscriptUsage {
            total_tokens: usage.live_total().or(Some(0)),
            prompt_tokens,
            cache_read_input_tokens: prompt_tokens.and(usage.cached_content_token_count),
            fresh_input_tokens: prompt_tokens
                .map(|prompt| prompt.saturating_sub(usage.cache_read())),
            output_tokens: prompt_tokens.map(|_| usage.output()),
            model: record.model.clone().filter(|value| !value.is_empty()),
            context_window: record.context_window_size,
        };
    }
    TranscriptUsage {
        total_tokens: Some(0),
        ..TranscriptUsage::default()
    }
}

#[cfg(test)]
mod tests;
