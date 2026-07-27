//! Qwen Code hook, context, account, and spend adapter.

pub(crate) mod account;
mod alibaba_usage;
mod ask;
mod install;
pub(crate) mod payloads;
mod selection;
pub(crate) mod spend;
mod statusline;

pub(crate) use crate::agents::capabilities::*;

use std::path::Path;

use jiff::Timestamp;
use serde_json::Value;

use self::install::MANAGED_SOURCE;
use self::payloads::{
    QwenStopError, parse_compact, parse_session_start, parse_stop, parse_stop_failure,
    parse_subagent, parse_tool_use, parse_user_prompt_submit,
};
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RemoteControlCapability, ThreadKey,
    ToolClassification, UserCoverage,
};
use super::hook_types::{BackgroundTask, HookEventSpec, SessionSource, decode_catalog_hook};
use super::lifecycle::{AskKind, LifecycleSignal};
use super::observation::{payload_has_context_observation, payload_total_tokens};
use super::pricing::PriceBook;
use super::transcript::{TranscriptMessage, TranscriptRole};
use super::{
    AgentLifecycleObservation, AgentTurnError, HookOutput, HookRouting, Result, RootIdentity,
    SessionOrigin, SubagentIdentity, TurnErrorClass, non_empty_trimmed, optional_payload_string,
    resolve_root_identity, resolve_subagent_identity, sanitize_user_prompt, stop_payload_errored,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;

static QWEN_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "qwen",
    aliases: &[],
    display_name: "Qwen Code",
    bin_names: &["qwen"],
    bin_identity: None,
    brand: Brand {
        emblem: None,
        color: 99,
        color_rgb: (0x61, 0x5c, 0xed),
    },
    plan_label: PlanLabel::TitleCaseOnly,
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        input_key: None,
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
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: QWEN_COVERAGE,
    user_coverage: QWEN_USER_COVERAGE,
    lifecycle_hooks: QWEN_LIFECYCLE_HOOKS,
    default_context_window: None,
    // Qwen routes across multiple provider protocols, each with its own model
    // catalog. Preserve the model selected in Qwen settings unless a RimZ
    // profile explicitly supplies `--model`.
    default_model: None,
    process_names: &["qwen", "node"],
    extra_bin_dirs: &[],
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
        max_turn_flag: Some("--max-session-turns"),
        compact_command: Some("/compress"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            system_prompt_file: Some(super::StaticPresetMatcher::TextFlag(&["--system-prompt"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const QWEN_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop/StopFailure",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "PermissionRequest + PreToolUse:exit_plan_mode",
    },
    user_question: ConcernCoverage::Wired {
        via: "PermissionRequest + PreToolUse:ask_user_question",
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
    tool_stats: ConcernCoverage::Unsupported {
        reason: "tool statistics are not integrated for this adapter",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "ACP/daemon mode is outside the pane-first adapter",
    },
};

const QWEN_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card tracks the session from start through every turn to close",
    },
    live: CapabilityLevel::Partial {
        shows: "context fill, token counts, and a priced dollar figure mid-turn",
        limit: "off-book models price at zero and multi-provider billing stays unmodeled",
    },
    history: CapabilityLevel::Partial {
        shows: "past sessions read end to end, with per-turn tokens and dollars",
        limit: "multi-provider billing is unmodeled and pruned sidechain branches stay uncounted",
    },
    account: CapabilityLevel::Partial {
        shows: "provider identity and Alibaba Coding Plan usage windows",
        limit: "the quota is experimental and display-only, and other providers report nothing",
    },
    ask: CapabilityLevel::Full {
        note: "permission prompts, plan approvals, and questions all reach rimz asks",
    },
    subagents: CapabilityLevel::Full {
        note: "children nest live under the parent card with name, task, and tokens",
    },
};

const QWEN_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
const QWEN_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle("SessionStart", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("SessionEnd", r#"{"session_id":"sess-1"}"#).session_ended(),
    HookEventSpec::lifecycle("UserPromptSubmit", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("Stop", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("StopFailure", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle("Notification", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::blocking(
        "PreToolUse",
        r#"{"tool_name":"ask_user_question","tool_use_id":"ask-1"}"#,
        AskKind::Question,
    )
    .with_matcher("ask_user_question|exit_plan_mode")
    .synchronous()
    .with_lifecycle_fallback(),
    HookEventSpec::blocking(
        "PermissionRequest",
        r#"{"tool_name":"run_shell_command"}"#,
        AskKind::Permission,
    )
    .synchronous()
    .with_lifecycle_fallback(),
    HookEventSpec::lifecycle("PostToolUse", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle("PostToolUseFailure", r#"{"session_id":"sess-1"}"#).progress(),
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"session_id":"parent","agent_id":"child","agent_type":"review"}"#,
    )
    .progress(),
    HookEventSpec::lifecycle("PreCompact", r#"{"session_id":"sess-1"}"#),
    HookEventSpec::lifecycle("PostCompact", r#"{"session_id":"sess-1"}"#),
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

fn qwen_lifecycle(
    adapter: &QwenAdapter,
    event_name: &str,
    payload: &Value,
) -> Option<AgentLifecycleObservation> {
    let signal = lifecycle_signal(adapter.spec(), event_name, payload)?;
    let (agent_id, parent_agent_id) = observation_identity(event_name, payload)?;
    let transcript_path = optional_payload_string(payload, &["transcript_path"]);
    let subagent =
        matches!(event_name, "SubagentStart" | "SubagentStop").then(|| parse_subagent(payload));
    let subagent_meta = subagent.as_ref().and_then(|child| {
        payloads::read_subagent_meta(
            transcript_path.as_deref()?,
            child.common.common.session_id.as_deref()?,
            child.common.agent_id.as_deref()?,
        )
    });
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
        .or_else(|| accepted_usage.and_then(|usage| usage.model.clone()))
        .or_else(|| {
            subagent_meta
                .as_ref()
                .and_then(|meta| meta.persisted_cli_flags.model.as_deref())
                .and_then(non_empty_trimmed)
        });
    observation.prompt = (event_name == "UserPromptSubmit")
        .then(|| parse_user_prompt_submit(payload))
        .and_then(|value| sanitize_user_prompt(value.prompt.as_deref()));
    observation.task = if let Some(subagent) = &subagent {
        subagent.common.agent_type.clone().or_else(|| {
            subagent_meta.as_ref().and_then(|meta| {
                meta.agent_type
                    .as_deref()
                    .and_then(non_empty_trimmed)
                    .or_else(|| meta.subagent_name.as_deref().and_then(non_empty_trimmed))
            })
        })
    } else {
        sanitize_user_prompt(optional_payload_string(payload, &["task"]).as_deref())
    };
    observation.description = usage.title.clone().or_else(|| {
        subagent_meta
            .as_ref()
            .and_then(|meta| meta.description.as_deref())
            .and_then(non_empty_trimmed)
    });
    observation.usage.context_pct = stop
        .as_ref()
        .and_then(|value| value.context_usage)
        .map(|ratio| (ratio * 100.0).round().clamp(0.0, 100.0) as u8);
    observation.usage.context_window = stop
        .as_ref()
        .and_then(|value| value.context_limit)
        .or_else(|| accepted_usage.and_then(|usage| usage.context_window));
    let transcript_total = accepted_usage.and_then(|usage| usage.total_tokens);
    let fallback_total = transcript_total
        .filter(|total| *total > 0)
        .or_else(|| stop.as_ref().and_then(|value| value.input_tokens))
        .or(transcript_total);
    observation.usage.total_tokens = payload_total_tokens(payload, fallback_total);
    observation.usage.cache_read_input_tokens =
        accepted_usage.and_then(|usage| usage.cache_read_input_tokens);
    observation.usage.fresh_input_tokens =
        accepted_usage.and_then(|usage| usage.fresh_input_tokens);
    observation.usage.output_tokens = accepted_usage.and_then(|usage| usage.output_tokens);
    if event_name == "SessionStart"
        && start.as_ref().is_some_and(|value| {
            matches!(value.source, SessionSource::Startup | SessionSource::Clear)
        })
    {
        observation.origin = Some(SessionOrigin::Fresh);
    }
    Some(observation)
}

impl crate::agents::capabilities::CoreCapability for QwenAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &QWEN_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        use super::{AgentHookClass, ClassificationSample};
        let mut samples = super::hook_types::catalog_classification_corpus(QWEN_HOOKS);
        samples.push(ClassificationSample::new(
            "PermissionRequest",
            serde_json::json!({"session_id":"sess-1","tool_name":"ask_user_question"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::Question),
        ));
        samples.push(ClassificationSample::new(
            "PermissionRequest",
            serde_json::json!({"session_id":"sess-1","tool_name":"exit_plan_mode"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
        ));
        samples.push(ClassificationSample::new(
            "PreToolUse",
            serde_json::json!({"session_id":"sess-1","tool_name":"exit_plan_mode","tool_use_id":"plan-1"}),
            AgentHookClass::AwaitingUser,
            Some(AskKind::PlanApproval),
        ));
        samples.push(ClassificationSample::new(
            "PreToolUse",
            serde_json::json!({"session_id":"sess-1","tool_name":"run_shell_command","tool_use_id":"shell-1"}),
            AgentHookClass::Lifecycle,
            None,
        ));
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "sess-1",
                file_name: "sess-1.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"uuid":"msg-1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,"candidatesTokenCount":10,"thoughtsTokenCount":5,"totalTokenCount":115}}"#,
                ),
            }),
            context_cost: Some(super::ContextCostFixture {
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
            }),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::LaunchCapability for QwenAdapter {}

impl crate::agents::capabilities::HookCapability for QwenAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let tool = matches!(event_name, "PermissionRequest" | "PreToolUse")
            .then(|| parse_tool_use(payload));
        let ask_kind = match event_name {
            "PermissionRequest" => Some(
                self.spec()
                    .blocking_tool_kind(tool.as_ref().and_then(|tool| tool.tool_name.as_deref()))
                    .unwrap_or(AskKind::Permission),
            ),
            "PreToolUse" => self
                .spec()
                .blocking_tool_kind(tool.as_ref().and_then(|tool| tool.tool_name.as_deref())),
            _ => None,
        };
        let mut decoded = decode_catalog_hook(QWEN_HOOKS, event_name, ask_kind);
        decoded.set_routing(
            HookRouting::split(
                optional_payload_string(payload, &["session_id", "agent_id"]).map(Into::into),
                optional_payload_string(payload, &["session_id"]).map(Into::into),
            )
            .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        let questions = tool
            .as_ref()
            .and_then(|tool| {
                ask::question_detail(tool.tool_name.as_deref()?, tool.tool_input.as_ref()?)
            })
            .unwrap_or_default();
        let ask_detail = questions
            .first()
            .and_then(|question| question.question.lines().next().map(ToOwned::to_owned))
            .or_else(|| {
                matches!(event_name, "PermissionRequest" | "PreToolUse")
                    .then(|| super::question::permission_detail(payload))
                    .flatten()
            });
        decoded.set_ask(questions, ask_detail);
        if event_name == "StopFailure" {
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
            decoded.set_turn_error(Some(AgentTurnError {
                class,
                at: Timestamp::now(),
                label,
            }));
        }
        if payload_has_context_observation(payload) {
            decoded.set_observed_context(self.observe_context(self.spec().kind, payload));
        }
        decoded.set_final_message(
            optional_payload_string(payload, &["last_assistant_message", "assistant_message"])
                .as_deref()
                .and_then(non_empty_trimmed),
        );
        if let Some(observation) = qwen_lifecycle(self, event_name, payload) {
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for QwenAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::TranscriptCapability for QwenAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        parse_messages(lines)
    }

    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        parse_physical_assistant_messages(new_lines)
    }
}

impl crate::agents::capabilities::ContextCapability for QwenAdapter {
    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        if !payload.is_object() {
            return None;
        }
        let parsed =
            serde_json::from_value::<statusline::StatuslinePayload>(payload.clone()).ok()?;
        let agent_id = parsed.session_id.clone()?;
        super::ContextObservation::new(agent_id, parsed.into_context(source, Timestamp::now()))
    }

    fn context_cost(&self, payload: &Value, prices: &super::PriceBook) -> Option<super::AgentCost> {
        if !payload.is_object() {
            return None;
        }
        serde_json::from_value::<statusline::StatuslinePayload>(payload.clone())
            .ok()?
            .cost(prices)
    }
}

impl crate::agents::capabilities::AccountCapability for QwenAdapter {
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
    fn resolve_managed_launch(
        &self,
        cwd: &Path,
        env: &std::collections::BTreeMap<String, String>,
        model: Option<&str>,
        argv: &[String],
    ) -> super::ManagedLaunchState {
        selection::resolve_managed_launch(cwd, env, model, argv)
    }
}

impl crate::agents::capabilities::SpendingCapability for QwenAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        crate::agents::spending::SpendingSource::tree(
            spend::runtime_base().join("projects"),
            "*/chats/*.jsonl",
        )
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
    spec: &AgentSpec,
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
        "UserPromptSubmit" => {
            // Qwen fires this hook on internal continuations with an empty prompt;
            // a real user prompt always carries text.
            let prompt = parse_user_prompt_submit(payload).prompt;
            prompt
                .as_deref()
                .is_none_or(|prompt| !prompt.trim().is_empty())
                .then_some(LifecycleSignal::TurnStarted)
        }
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: spec.tool_mutates(payload),
            edits: spec.tool_edits_files(payload),
            name: None,
            native_key: parse_tool_use(payload).tool_use_id,
        }),
        "PostToolUseFailure" => Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: parse_tool_use(payload).tool_use_id,
        }),
        "PreToolUse" => {
            let tool = parse_tool_use(payload);
            spec.blocking_tool_kind(tool.tool_name.as_deref())
                .map(|kind| LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                    native_key: tool.tool_use_id,
                })
        }
        "PermissionRequest" => {
            let tool = parse_tool_use(payload);
            Some(LifecycleSignal::AwaitingInput {
                kind: spec
                    .blocking_tool_kind(tool.tool_name.as_deref())
                    .unwrap_or(AskKind::Permission),
                ask_id: None,
                detail: None,
                native_key: tool.tool_use_id,
            })
        }
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
    title: Option<String>,
}

fn usage_from_transcript(path: &str) -> TranscriptUsage {
    let Ok(text) = std::fs::read_to_string(path) else {
        return TranscriptUsage::default();
    };
    let folded = payloads::fold_transcript(&text);
    let title = folded.latest_active_custom_title().map(ToOwned::to_owned);
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
            title,
        };
    }
    TranscriptUsage {
        total_tokens: Some(0),
        title,
        ..TranscriptUsage::default()
    }
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for QwenAdapter {}
impl crate::agents::capabilities::SessionCapability for QwenAdapter {}

#[cfg(test)]
mod tests;
