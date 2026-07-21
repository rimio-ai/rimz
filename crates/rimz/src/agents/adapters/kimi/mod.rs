//! Kimi Code command-hook and durable agent-record adapter.

pub(crate) mod account;
mod install;
pub(crate) mod oauth_usage;
pub(crate) mod payloads;
pub(crate) mod spend;
mod subagents;
mod transcript;
pub mod wire;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
use tracing::debug;

use super::context::{AgentCurrentUsage, AgentTokenUsage};
use super::definition::{
    AgentSpec, Brand, Capabilities, CapabilityLevel, ConcernCoverage, CoverageAnnotations,
    HookCoverage, LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    ThreadKey, ToolClassification, UserCoverage,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::LifecycleSignal;
use super::{
    AgentLifecycleObservation, AgentTurnError, FieldPatch, HookOutput, HookRouting,
    LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx, LocalTokenPatch,
    RefreshTrigger, Result, TranscriptStat, TurnErrorClass, non_empty_trimmed,
    sanitize_user_prompt,
};
#[cfg(test)]
use crate::harness::run::PermissionMode;
use crate::ids::AgentSessionId;
use crate::transcript::AskQuestion;

pub(super) const KIMI_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle( "SessionStart", r#"{"session_id":"s"}"#).progress(),
    HookEventSpec::lifecycle( "UserPromptSubmit", r#"{"session_id":"s","prompt":[{"type":"text","text":"fix"}]}"#).progress(),
    HookEventSpec::blocking( "PreToolUse", r#"{"session_id":"s","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Continue?"}]}}"#, super::AskKind::Question)
        .with_matcher(".*")
        .synchronous()
        .with_lifecycle_fallback(),
    HookEventSpec::lifecycle( "PostToolUse", r#"{"session_id":"s","tool_name":"Write"}"#)
        .with_matcher(".*")
        .progress(),
    HookEventSpec::lifecycle( "PostToolUseFailure", r#"{"session_id":"s","tool_name":"Bash"}"#)
        .with_matcher(".*")
        .progress(),
    HookEventSpec::blocking( "PermissionRequest", r#"{"session_id":"s","tool_call_id":"r","tool_name":"Bash","action":"Run shell"}"#, super::AskKind::Permission)
        .with_matcher(".*")
        .synchronous()
        .with_lifecycle_fallback(),
    HookEventSpec::lifecycle( "PermissionResult", r#"{"session_id":"s","tool_call_id":"r","tool_name":"Bash","decision":"approved"}"#)
        .with_matcher(".*")
        .progress(),
    HookEventSpec::lifecycle( "Stop", r#"{"session_id":"s"}"#).progress(),
    HookEventSpec::lifecycle( "StopFailure", r#"{"session_id":"s","error_type":"RuntimeError"}"#),
    HookEventSpec::lifecycle( "Interrupt", r#"{"session_id":"s","turn_id":"t","reason":"cancelled"}"#).progress(),
    HookEventSpec::lifecycle( "SessionEnd", r#"{"session_id":"s"}"#).session_ended(),
    HookEventSpec::lifecycle( "SubagentStart", r#"{"session_id":"s","agent_name":"coder","prompt":"inspect the parser"}"#).progress(),
    HookEventSpec::lifecycle( "SubagentStop", r#"{"session_id":"s","agent_name":"coder","response":"done"}"#).progress(),
    HookEventSpec::lifecycle( "PreCompact", r#"{"session_id":"s","trigger":"manual"}"#),
    HookEventSpec::lifecycle( "PostCompact", r#"{"session_id":"s","trigger":"auto"}"#),
    HookEventSpec::lifecycle( "Notification", r#"{"session_id":"s","notification_type":"task.completed"}"#).progress(),
];

static KIMI_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "kimi",
    aliases: &[],
    display_name: "Kimi",
    brand: Brand {
        emblem: None,
        color: 33,
        color_rgb: (0x17, 0x83, 0xff),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Kimi" },
    sub_providers: &[],
    expected_windows: &["5h", "7d"],
    tools: ToolClassification {
        mutating: &["Bash", "Write", "Edit"],
        editing: &["Write", "Edit"],
        blocking: &[
            ("AskUserQuestion", super::AskKind::Question),
            ("ExitPlanMode", super::AskKind::PlanApproval),
        ],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: true,
        same_pane_session: super::SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: KIMI_COVERAGE,
    user_coverage: KIMI_USER_COVERAGE,
    lifecycle_hooks: KIMI_LIFECYCLE_HOOKS,
    default_context_window: Some(262_144),
    default_model: None,
    process_names: &["kimi", "kimi-code"],
    bin_names: &["kimi"],
    extra_bin_dirs: &[".kimi-code/bin"],
    thread_key: ThreadKey::SessionDir,
    launch: super::LaunchSpec {
        program: Some("kimi"),
        fixed_args: &[],
        prompt: super::PromptStyle::None,
        resume: Some(super::SessionCommand {
            before_id: &["kimi", "--session"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--auto"],
            yolo: &["--yolo"],
            plan: &["--plan"],
        },
        ping_args: Some(&[]),
        max_turn_flag: None,
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model", "-m"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const KIMI_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Wired {
        via: "PermissionRequest hook",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "PermissionRequest ExitPlanMode",
    },
    user_question: ConcernCoverage::Wired {
        via: "PreToolUse AskUserQuestion",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "native Kimi UI owns answers",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact",
    },
    subagents: ConcernCoverage::Partial {
        via: "SubagentStart/Stop + state.json/wire join",
        gap: "resumed subagents and ambiguous starts surface only at stop",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "background parking is not mapped",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Partial {
        via: "turn boundaries + ask path + stall window",
        gap: "no idle notification hook",
    },
    context_usage: ConcernCoverage::Partial {
        via: "derived from Wire usage.record token totals",
        gap: "kimi-code omits the exact context ratio from the durable log",
    },
    realtime_cost: ConcernCoverage::Wired {
        via: "priced Wire usage.record (model present)",
    },
    rich_context: ConcernCoverage::Unsupported {
        reason: "no push transport; bounded Wire tail only",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.kimi-code/config.toml [[hooks]]",
    },
    account_spend: ConcernCoverage::Partial {
        via: "managed OAuth quota plus priced agent-record tokens",
        gap: "effective provider attribution and non-USD balance currency need shared model support",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "no remote-control surface",
    },
};

const KIMI_USER_COVERAGE: UserCoverage = UserCoverage {
    state: CapabilityLevel::Full {
        note: "the card tracks the session from start through every turn to close",
    },
    live: CapabilityLevel::Partial {
        shows: "token totals and a priced dollar figure throughout the turn",
        limit: "kimi-code omits the exact context ratio, so the fill gauge is derived",
    },
    history: CapabilityLevel::Partial {
        shows: "past sessions read end to end, with per-turn tokens and dollars",
        limit: "provider attribution and non-USD balances stay unresolved",
    },
    account: CapabilityLevel::Full {
        note: "managed OAuth identity, plan, and every quota window with fill and reset",
    },
    ask: CapabilityLevel::Full {
        note: "permission prompts, plan approvals, and questions all reach rimz asks",
    },
    subagents: CapabilityLevel::Partial {
        shows: "children nest under the parent card with their own lifecycle",
        limit: "resumed and concurrently started children surface only when they stop",
    },
};

const KIMI_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
    subagent_started: HookCoverage::Derived {
        via: "native hook + durable state.json/wire join",
        gap: "resumed subagents and ambiguous starts have no start-time identity",
    },
    subagent_stopped: HookCoverage::Derived {
        via: "native hook + durable state.json/wire join",
        gap: "ambiguous response matches are quarantined",
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

#[derive(Clone, Debug, Default)]
pub struct KimiAdapter;

fn kimi_ask_kind(
    adapter: &KimiAdapter,
    event_name: &str,
    parsed: &payloads::KimiHookPayload,
) -> Option<super::AskKind> {
    match event_name {
        "PermissionRequest" => Some(
            adapter
                .spec()
                .blocking_tool_kind(parsed.tool_name.as_deref())
                .unwrap_or(super::AskKind::Permission),
        ),
        "PreToolUse"
            if parsed.tool_name.as_deref() == Some("AskUserQuestion")
                && !parsed.question_background() =>
        {
            Some(super::AskKind::Question)
        }
        _ => None,
    }
}

fn kimi_questions(event_name: &str, parsed: &payloads::KimiHookPayload) -> Vec<AskQuestion> {
    (event_name == "PreToolUse" && parsed.tool_name.as_deref() == Some("AskUserQuestion"))
        .then(|| parsed.tool_input.as_ref().and_then(parse_questions))
        .flatten()
        .unwrap_or_default()
}

fn kimi_turn_error(event_name: &str, parsed: &payloads::KimiHookPayload) -> Option<AgentTurnError> {
    (event_name == "StopFailure").then(|| {
        let label = parsed
            .error_message
            .clone()
            .or_else(|| parsed.error_type.clone())
            .and_then(|value| non_empty_trimmed(&value))
            .map(|value| value.chars().take(80).collect::<String>());
        AgentTurnError {
            class: TurnErrorClass::classify_label(label.as_deref()),
            at: Timestamp::now(),
            label,
        }
    })
}

fn kimi_observation(
    adapter: &KimiAdapter,
    event_name: &str,
    payload: &Value,
    parsed: &payloads::KimiHookPayload,
    session_dir: Option<&Path>,
    ask_detail: Option<String>,
) -> Option<AgentLifecycleObservation> {
    if matches!(event_name, "SubagentStart" | "SubagentStop") {
        return session_dir.and_then(|session_dir| {
            adapter.observe_subagent_lifecycle(event_name, payload, parsed, session_dir)
        });
    }
    if event_name == "Stop"
        && session_dir.is_some_and(|session_dir| {
            subagents::has_subagents(session_dir) && subagents::main_turn_mid_step(session_dir)
        })
    {
        debug!(
            target: "rimz::agent::lifecycle",
            kind = adapter.spec().kind,
            session_id = parsed.session_id.as_deref().unwrap_or(""),
            "suppressed child-fired Kimi Stop while the main wire was mid-step",
        );
        return None;
    }
    let signal = kimi_root_signal(adapter, event_name, payload, parsed, ask_detail)?;
    let agent_id = parsed.session_id.clone().map(AgentSessionId::from);
    let mut observation =
        AgentLifecycleObservation::new(agent_id, signal).with_worktree_from_payload(payload);
    observation.task = sanitize_user_prompt(parsed.prompt.as_deref());
    observation.prompt = sanitize_user_prompt(parsed.prompt.as_deref());
    if event_name == "SessionStart"
        && let Some(session_id) = parsed.session_id.as_deref()
    {
        observation.transcript_path =
            wire::wire_path(session_id, parsed.cwd.as_deref().map(Path::new))
                .map(|path| path.to_string_lossy().into_owned());
    }
    Some(observation)
}

fn kimi_root_signal(
    adapter: &KimiAdapter,
    event_name: &str,
    payload: &Value,
    parsed: &payloads::KimiHookPayload,
    ask_detail: Option<String>,
) -> Option<LifecycleSignal> {
    let neutral_tool = || LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        native_key: None,
    };
    match event_name {
        "SessionStart" => Some(LifecycleSignal::Registered),
        "UserPromptSubmit" => Some(LifecycleSignal::TurnStarted),
        "PreToolUse"
            if parsed.tool_name.as_deref() == Some("AskUserQuestion")
                && !parsed.question_background() =>
        {
            Some(LifecycleSignal::AwaitingInput {
                kind: super::AskKind::Question,
                ask_id: None,
                detail: ask_detail,
                native_key: None,
            })
        }
        "PermissionRequest" => Some(LifecycleSignal::AwaitingInput {
            kind: adapter
                .spec()
                .blocking_tool_kind(parsed.tool_name.as_deref())
                .unwrap_or(super::AskKind::Permission),
            ask_id: None,
            detail: ask_detail,
            native_key: None,
        }),
        "PermissionResult" | "PostToolUseFailure" => Some(neutral_tool()),
        "PostToolUse" => Some(LifecycleSignal::ToolUsed {
            mutates: adapter.spec().tool_mutates(payload),
            edits: adapter.spec().tool_edits_files(payload),
            native_key: None,
        }),
        "Stop" | "Interrupt" => Some(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }),
        "StopFailure" => Some(LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }),
        "SessionEnd" => Some(LifecycleSignal::Ended),
        "PreCompact" => Some(LifecycleSignal::Compacting),
        "PostCompact" => Some(LifecycleSignal::CompactionEnded {
            auto: parsed.trigger.as_deref().map(|trigger| trigger == "auto"),
        }),
        _ => None,
    }
}

fn kimi_final_message(event_name: &str, parsed: &payloads::KimiHookPayload) -> Option<String> {
    matches!(event_name, "Stop" | "StopFailure")
        .then(|| {
            let path = wire::wire_path(
                parsed.session_id.as_deref()?,
                parsed.cwd.as_deref().map(Path::new),
            )?;
            let lines = std::fs::read_to_string(path).ok()?;
            transcript::latest_assistant(&lines)
        })
        .flatten()
}

impl crate::agents::capabilities::CoreCapability for KimiAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &KIMI_DESCRIPTOR
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        use super::{AgentHookClass, ClassificationSample};
        let mut samples = super::hook_types::catalog_classification_corpus(KIMI_HOOKS);
        samples.extend([
            ClassificationSample::new(
                "PreToolUse",
                serde_json::json!({"session_id":"s","tool_name":"ExitPlanMode"}),
                AgentHookClass::Lifecycle,
                None,
            ),
            ClassificationSample::new(
                "PermissionRequest",
                serde_json::json!({"session_id":"s","tool_call_id":"r","tool_name":"ExitPlanMode","action":"Exit plan mode"}),
                AgentHookClass::AwaitingUser,
                Some(super::AskKind::PlanApproval),
            ),
        ]);
        super::AdapterConformance {
            classification: samples,
            spend: Some(super::SpendFixture {
                session_id: "s",
                file_name: "sessions/wd/s/agents/main/wire.jsonl",
                body: super::SpendFixtureBody::Jsonl(concat!(
                    "{\"type\":\"metadata\",\"protocol_version\":\"1.4\"}\n",
                    "{\"type\":\"usage.record\",\"time\":1770000000000,\"model\":\"moonshot/kimi-k2.5\",\"usage\":{\"inputOther\":100,\"output\":50,\"inputCacheRead\":10,\"inputCacheCreation\":5},\"usageScope\":\"turn\"}"
                )),
            }),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::HookCapability for KimiAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let parsed = payloads::parse(payload);
        let ask = kimi_ask_kind(self, event_name, &parsed);
        let questions = kimi_questions(event_name, &parsed);
        let mut decoded = decode_catalog_hook(KIMI_HOOKS, event_name, ask);
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(parsed.cwd.clone()),
        );
        let ask_detail = if event_name == "PermissionRequest" {
            parsed.action.as_deref().and_then(non_empty_trimmed)
        } else {
            questions.first().map(|question| question.question.clone())
        };
        decoded.set_ask(questions, ask_detail.clone());
        decoded.set_turn_error(kimi_turn_error(event_name, &parsed));
        let session_dir = matches!(event_name, "SubagentStart" | "SubagentStop" | "Stop")
            .then(|| {
                wire::session_dir(
                    parsed.session_id.as_deref()?,
                    parsed.cwd.as_deref().map(Path::new),
                )
            })
            .flatten();
        let observation = kimi_observation(
            self,
            event_name,
            payload,
            &parsed,
            session_dir.as_deref(),
            ask_detail,
        );
        if let Some(observation) = observation {
            decoded.set_final_message(kimi_final_message(event_name, &parsed));
            decoded.attach_lifecycle(observation);
        }
        Ok(decoded)
    }
}

impl crate::agents::capabilities::InstallationCapability for KimiAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_INTEGRATION)
    }
}

impl crate::agents::capabilities::LaunchCapability for KimiAdapter {
    fn configured_identity(&self) -> (Option<String>, Option<String>) {
        (spend::configured_model(), None)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["kimi".to_owned()];
        argv.extend(
            extra_args
                .iter()
                .filter(|arg| {
                    prompt.is_none() || !matches!(arg.as_str(), "--auto" | "--yolo" | "--plan")
                })
                .cloned(),
        );
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
            argv.extend([
                "--prompt".to_owned(),
                prompt.to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
            ]);
        }
        Some(argv)
    }
}

impl crate::agents::capabilities::TranscriptCapability for KimiAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<super::TranscriptMessage> {
        transcript::parse_messages(lines)
    }
}

impl crate::agents::capabilities::ContextCapability for KimiAdapter {
    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        if matches!(trigger, RefreshTrigger::Hook(event) if !matches!(event, "SessionStart" | "UserPromptSubmit" | "Stop" | "StopFailure" | "Interrupt" | "PostToolUse" | "PostToolUseFailure" | "PostCompact" | "Notification"))
        {
            return None;
        }
        refresh_wire_context(ctx)
    }
}

impl crate::agents::capabilities::AccountCapability for KimiAdapter {
    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> super::AccountUsageProbe {
        oauth_usage::probe()
    }
}

impl crate::agents::capabilities::SpendingCapability for KimiAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        crate::agents::spending::SpendingSource::tree(
            wire::kimi_home().join("sessions"),
            "*/*/agents/main/wire.jsonl",
        )
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = prior_path.filter(|path| valid_main_wire(path, session_id)) {
            return Some(path.to_path_buf());
        }
        wire::wire_path(session_id, None)
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, resume, prices)
    }
}

impl KimiAdapter {
    fn observe_subagent_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
        parsed: &payloads::KimiHookPayload,
        session_dir: &Path,
    ) -> Option<AgentLifecycleObservation> {
        let matched = match event_name {
            "SubagentStart" => subagents::resolve_start(session_dir, parsed.prompt.as_deref()),
            "SubagentStop" => subagents::resolve_stop(session_dir, parsed.response.as_deref()),
            _ => return None,
        };
        let Some(matched) = matched else {
            debug!(
                target: "rimz::agent::lifecycle",
                kind = self.spec().kind,
                event = event_name,
                session_id = parsed.session_id.as_deref().unwrap_or(""),
                "quarantined Kimi subagent hook without a unique durable child match",
            );
            return None;
        };
        let session_id = parsed.session_id.as_deref()?.trim();
        if session_id.is_empty() {
            return None;
        }
        let signal = match event_name {
            "SubagentStart" => LifecycleSignal::SubagentStarted,
            "SubagentStop" => LifecycleSignal::SubagentStopped { errored: false },
            _ => return None,
        };
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(format!("{session_id}:{}", matched.id))),
            signal,
        )
        .with_worktree_from_payload(payload);
        observation.parent_agent_id = Some(AgentSessionId::from(session_id));
        observation.launch.model = matched.model;
        observation.launch.effort = matched.effort;
        observation.agent_name = parsed
            .agent_name
            .as_deref()
            .and_then(non_empty_trimmed)
            .or(matched.profile);
        observation.task = sanitize_user_prompt(matched.task.as_deref());
        observation.prompt = observation.task.clone();
        observation.transcript_path = Some(matched.transcript_path.to_string_lossy().into_owned());
        Some(observation)
    }
}

fn parse_questions(input: &Value) -> Option<Vec<AskQuestion>> {
    super::question::questions(input, super::question::PreviewPolicy::None)
}

fn refresh_wire_context(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    let path =
        KimiAdapter.session_transcript(ctx.agent_id, ctx.prior_transcript_path.map(Path::new))?;
    let stat = TranscriptStat::from_path(&path)?;
    refresh_wire_path(&path, ctx.agent_id, stat, ctx)
}

fn refresh_wire_path(
    path: &Path,
    session_id: &str,
    stat: TranscriptStat,
    ctx: &LocalContextRefreshCtx<'_>,
) -> Option<LocalContextRefresh> {
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let snapshot = wire::WireSnapshot::read(path)?;
    let records = snapshot.tail_records();
    let attribution = wire::effective_attribution(records);
    let latest_usage = wire::latest_turn_usage(records);
    let configured = spend::configured_model().map(|model| wire::normalize_model_alias(&model));
    let model_id = attribution
        .display_model()
        .or_else(|| ctx.model_hint.map(wire::normalize_model_alias))
        .or(configured);
    let context_window_size = configured_context_window(model_id.as_deref()).or(Some(262_144));
    let tokens = if let Some(input) = wire::latest_context_tokens(records) {
        Some(AgentTokenUsage {
            context_window_size,
            used_percentage: context_window_size.map(|window| percentage(input, window)),
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: latest_usage
                    .as_ref()
                    .and_then(|record| record.usage.input_other),
                output_tokens: latest_usage.as_ref().and_then(|record| record.usage.output),
                cache_creation_input_tokens: latest_usage
                    .as_ref()
                    .and_then(|record| record.usage.input_cache_creation),
                cache_read_input_tokens: latest_usage
                    .as_ref()
                    .and_then(|record| record.usage.input_cache_read),
            }),
            session_usage: None,
        })
    } else {
        // No usage record in the bounded tail. Emit the shared fresh sentinel —
        // unknown fill over zeroed step usage — so the merge layer preserves an
        // established session's context meter instead of flashing it to 0% when a
        // heavy tool record pushes the last usage record out of the tail window.
        // A brand-new session still resolves to the empty baseline.
        Some(AgentTokenUsage {
            context_window_size,
            used_percentage: None,
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage::default()),
            session_usage: None,
        })
    };
    let prices = super::pricing::cached_book(ctx.shared_pricing_cache_path);
    let spend = spend::parse_snapshot(path, &snapshot, &prices);
    let cost = super::spending::session_cost_from_entries(&spend.entries, session_id);
    let session_preview = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(subagents::session_title);
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            session_preview: session_preview.map_or(FieldPatch::Keep, FieldPatch::Set),
            model_id: model_id.map_or(FieldPatch::Keep, FieldPatch::Set),
            effort: attribution
                .thinking_effort
                .map_or(FieldPatch::Keep, FieldPatch::Set),
            tokens: LocalTokenPatch::PreserveEstablished(tokens),
            cost: cost.map_or(FieldPatch::Keep, FieldPatch::Set),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        ..LocalContextRefresh::authoritative_current()
    })
}

fn valid_main_wire(path: &Path, session_id: &str) -> bool {
    path.is_file()
        && path.file_name().is_some_and(|name| name == "wire.jsonl")
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "main")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "agents")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some(session_id)
}

fn percentage(used: u64, capacity: u64) -> u8 {
    if capacity == 0 {
        return 0;
    }
    ((used as f64 / capacity as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

fn configured_context_window(model_hint: Option<&str>) -> Option<u64> {
    let path = install::config_path().ok()?;
    configured_context_window_at(&path, model_hint)
}

fn configured_context_window_at(path: &Path, model_hint: Option<&str>) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: toml::Table = toml::from_str(&text).ok()?;
    let alias = model_hint
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| root.get("default_model").and_then(toml::Value::as_str))?;
    let model = root
        .get("models")
        .and_then(toml::Value::as_table)?
        .get(alias)?
        .as_table()?;
    model
        .get("overrides")
        .and_then(toml::Value::as_table)
        .and_then(|overrides| overrides.get("max_context_size"))
        .or_else(|| model.get("max_context_size"))
        .and_then(toml::Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
}

// Capabilities this agent has no behavior for; every method keeps its
// default from `agents::capabilities`.
impl crate::agents::capabilities::RuntimeControlCapability for KimiAdapter {}
impl crate::agents::capabilities::SessionCapability for KimiAdapter {}

#[cfg(test)]
mod tests;
