//! GitHub Copilot CLI command-hook adapter.
//!
//! Copilot's native camelCase command hooks provide lifecycle truth and its
//! synchronous permission/question gates. RimZ owns one whole hook file at
//! `$COPILOT_HOME/hooks/rimz.json`; empty hook stdout preserves Copilot's native
//! decision UI. Per-session events provide conversation history and optional
//! decision UI. A reversible managed statusline supplies live context, with
//! metadata-only OTel chat spans as the fallback when that bridge is unhealthy.

mod account;
mod account_usage;
mod install;
mod otel;
mod paths;
pub(crate) mod payloads;
mod spend;
mod statusline;
mod subagent;
mod transcript;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability, ThreadKey,
    ToolClassification,
};
use super::hook_types::{HookRecord, decode_catalog_hook, hook_record};
use super::lifecycle::LifecycleSignal;
use super::managed_source::ManagedSource;
use super::managed_statusline::{ManagedStatusLineSpec, RenderingOptions, WrapPolicy};
use super::{
    AgentAdapter, AgentLifecycleObservation, AgentTurnError, AskKind, DecodedHook,
    HookInstallPreview, HookInstallReport, HookRouting, HookUninstallReport, LocalContextRefresh,
    LocalContextRefreshCtx, RefreshTrigger, Result, SessionOrigin, SpawnedSubagent,
    SubagentCorrelation, SubagentCorrelationInput, SubagentIdentity, SubagentSpawnInput,
    TranscriptMessage, TurnErrorClass, optional_payload_string, resolve_subagent_identity,
    sanitize_user_prompt,
};
#[cfg(test)]
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
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &["bash", "powershell", "create", "edit"],
        editing: &["create", "edit"],
        blocking: &[("ask_user", AskKind::Question)],
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
    coverage: COPILOT_COVERAGE,
    lifecycle_hooks: COPILOT_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["copilot", "node"],
    bin_names: &["copilot"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("copilot"),
        fixed_args: &[],
        prompt: super::PromptStyle::Flag("--interactive"),
        resume: Some(super::SessionCommand {
            before_id: &["copilot", "--resume"],
            after_id: &[],
        }),
        fork: None,
        permission: super::LaunchPermissionArgs {
            ask: &[],
            auto: &["--autopilot"],
            yolo: &["--allow-all"],
            plan: &["--plan"],
        },
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

const COPILOT_COVERAGE: IntegrationCoverage = IntegrationCoverage {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "sessionStart/userPromptSubmitted/agentStop",
    },
    permission: ConcernCoverage::Wired {
        via: "permissionRequest",
    },
    plan_approval: ConcernCoverage::Unsupported {
        reason: "plan mode has no approval hook",
    },
    user_question: ConcernCoverage::Wired {
        via: "preToolUse(ask_user)",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "no native answer protocol; answer in the pane",
    },
    compaction: ConcernCoverage::Partial {
        via: "preCompact + next lifecycle signal",
        gap: "no native post-compact hook",
    },
    subagents: ConcernCoverage::Partial {
        via: "child hooks joined to parent subagent.started/subagent.completed records",
        gap: "no child tool/permission hooks",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "no parked-on-background signal",
    },
    session_end: ConcernCoverage::Wired { via: "sessionEnd" },
    idle_notification: ConcernCoverage::Partial {
        via: "agentStop + stall window",
        gap: "notification(agent_idle) is not wired",
    },
    context_usage: ConcernCoverage::Wired {
        via: "statusline window/fill/occupied/current and cumulative token scopes",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "statusline cumulative token scopes priced by the local book",
        gap: "estimated: totals priced at the currently-resolved model; premium-request billing is not modeled",
    },
    rich_context: ConcernCoverage::Wired {
        via: "command statusline payload with metadata-only OTel fallback",
    },
    hook_install: ConcernCoverage::Wired {
        via: "$COPILOT_HOME/hooks/rimz.json + reversible settings.json statusline",
    },
    account_spend: ConcernCoverage::Partial {
        via: "finalized session.shutdown history priced by the local book",
        gap: "no authoritative account dollar ledger",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "remote-control preflight is not wired",
    },
};

const COPILOT_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
    registered: HookCoverage::Native {
        event: "sessionStart",
    },
    turn_started: HookCoverage::Native {
        event: "userPromptSubmitted",
    },
    turn_ended: HookCoverage::Native { event: "agentStop" },
    tool_used: HookCoverage::Native {
        event: "postToolUse",
    },
    awaiting_input: HookCoverage::Native {
        event: "permissionRequest",
    },
    subagent_started: HookCoverage::Derived {
        via: "child userPromptSubmitted joined to parent subagent.started model metadata",
        gap: "no child tool/permission hooks",
    },
    subagent_stopped: HookCoverage::Derived {
        via: "child agentStop plus parent subagent.completed token reconciliation",
        gap: "no child tool/permission hooks",
    },
    compacting: HookCoverage::Native {
        event: "preCompact",
    },
    compaction_ended: HookCoverage::Derived {
        via: "next lifecycle signal + display-window expiry",
        gap: "no native post-compact hook",
    },
    ended: HookCoverage::Native {
        event: "sessionEnd",
    },
    lost: HookCoverage::Derived {
        via: "rimz exec wrapper",
        gap: "native hooks do not report mux-session death",
    },
};

pub(super) const COPILOT_HOOKS: &[HookRecord] = &[
    hook_record!(
        lifecycle,
        "sessionStart",
        r#"{"sessionId":"sess-1","source":"startup"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "userPromptSubmitted",
        r#"{"sessionId":"sess-1","prompt":"fix auth"}"#
    )
    .progress(),
    hook_record!(
        blocking,
        "preToolUse",
        r#"{"sessionId":"sess-1","toolName":"ask_user","toolArgs":{"question":"Proceed?"}}"#,
        AskKind::Question
    )
    .synchronous()
    .with_lifecycle_fallback(),
    hook_record!(
        lifecycle,
        "postToolUse",
        r#"{"sessionId":"sess-1","toolName":"edit"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "postToolUseFailure",
        r#"{"sessionId":"sess-1","toolName":"bash","error":"failed"}"#
    )
    .progress(),
    hook_record!(
        blocking,
        "permissionRequest",
        r#"{"sessionId":"sess-1","toolName":"bash"}"#,
        AskKind::Permission
    )
    .synchronous(),
    hook_record!(
        lifecycle,
        "agentStop",
        r#"{"sessionId":"sess-1","stopReason":"end_turn"}"#
    )
    .progress(),
    hook_record!(
        lifecycle,
        "preCompact",
        r#"{"sessionId":"sess-1","trigger":"auto"}"#
    ),
    hook_record!(
        lifecycle,
        "errorOccurred",
        r#"{"sessionId":"sess-1","recoverable":true,"error":{"message":"retry"}}"#
    ),
    hook_record!(
        lifecycle,
        "sessionEnd",
        r#"{"sessionId":"sess-1","reason":"user_exit"}"#
    )
    .session_ended(),
];

// If Copilot starts rejecting unknown top-level keys, move the ownership
// marker into the first hook entry's `env` overlay after live verification.
const HOOK_SOURCE: &str = include_str!("hooks.json");

const COPILOT_MANAGED_SOURCE: ManagedSource = ManagedSource::new(
    "copilot",
    HOOK_SOURCE,
    COPILOT_HOOKS,
    "hook file",
    paths::hooks_path,
    false,
);

const STATUS_LINE_COMMAND: &str = "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source copilot";
const RIMZ_STATUS_LINE_MARKER: &str = "rimz statusline feed --source copilot";
const STATUS_LINE: ManagedStatusLineSpec = ManagedStatusLineSpec {
    key_path: &["statusLine"],
    command: STATUS_LINE_COMMAND,
    command_marker: RIMZ_STATUS_LINE_MARKER,
    rendering_options: RenderingOptions::Only(&["padding"]),
    wrap_policy: WrapPolicy::CommandMode,
    required_for_install: true,
};

#[derive(Clone, Debug, Default)]
pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &COPILOT_DESCRIPTOR
    }

    fn parse_version(&self, stdout: &str, stderr: &str) -> Option<String> {
        parse_copilot_version(stdout).or_else(|| parse_copilot_version(stderr))
    }

    #[cfg(test)]
    fn context_cost_fixture(&self) -> Option<super::ContextCostFixture> {
        Some(super::ContextCostFixture {
            payload: serde_json::from_str(include_str!("tests/fixtures/statusline-modern.json"))
                .expect("valid Copilot statusline fixture"),
        })
    }

    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<DecodedHook> {
        let parsed = payloads::parse_payload(payload);
        let tools = parsed.normalized_tool_calls();
        let tool = tools.selected();
        let ask_kind = if event_name == "permissionRequest" {
            Some(AskKind::Permission)
        } else if event_name == "preToolUse" {
            self.descriptor()
                .blocking_tool_kind(tool.and_then(|tool| tool.name))
        } else {
            None
        };
        let mut decoded = decode_catalog_hook(COPILOT_HOOKS, event_name, ask_kind);
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(optional_payload_string(payload, &["worktree_path", "cwd"])),
        );
        let questions = if event_name == "preToolUse" {
            tool.filter(|tool| tool.name == Some("ask_user"))
                .and_then(|tool| tool.args?.as_object())
                .and_then(|args| {
                    let question = ["question", "prompt", "message"]
                        .into_iter()
                        .find_map(|key| args.get(key).and_then(Value::as_str))
                        .or_else(|| args.values().find_map(Value::as_str))?
                        .trim();
                    (!question.is_empty()).then(|| {
                        vec![AskQuestion {
                            question: question.to_owned(),
                            options: Vec::new(),
                            multi_select: false,
                            has_option_previews: false,
                        }]
                    })
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let ask_detail = if event_name == "permissionRequest" {
            tool.and_then(|tool| tool.name)
                .map(str::to_owned)
                .filter(|name| !name.is_empty())
        } else {
            questions.first().map(|question| question.question.clone())
        };
        decoded.set_ask(questions, ask_detail);
        decoded.set_turn_error(
            (event_name == "errorOccurred")
                .then(|| {
                    (parsed.recoverable == Some(false)).then_some(())?;
                    let label = parsed
                        .error
                        .clone()
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
                })
                .flatten(),
        );
        if [
            "model",
            "effort",
            "rate_limits",
            "total_cost_usd",
            "context_window",
            "total_tokens",
            "context_pct",
        ]
        .into_iter()
        .any(|key| payload.get(key).is_some())
        {
            decoded.set_observed_context(self.observe_context(self.descriptor().kind, payload));
        }
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
                native_key: None,
            },
            "preToolUse" => match self
                .descriptor()
                .blocking_tool_kind(tool.and_then(|tool| tool.name))
            {
                Some(kind) => LifecycleSignal::AwaitingInput {
                    kind,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                None => LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    native_key: None,
                },
            },
            "postToolUse" | "postToolUseFailure" => LifecycleSignal::ToolUsed {
                mutates: tools.any_named(self.descriptor().tools.mutating),
                edits: tools.any_named(self.descriptor().tools.editing),
                native_key: None,
            },
            "agentStop" => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            "preCompact" => LifecycleSignal::Compacting,
            "sessionEnd" => LifecycleSignal::Ended,
            _ => return Ok(decoded),
        };
        let mut observation = AgentLifecycleObservation::new(
            parsed.session_id.clone().map(AgentSessionId::from),
            signal,
        )
        .with_worktree_from_payload(payload);
        if let Some(session_id) = parsed.session_id.as_deref() {
            observation.transcript_path = parsed
                .transcript_path
                .as_deref()
                .and_then(|path| paths::validated_transcript_path(Path::new(path), session_id))
                .or_else(|| {
                    paths::session_transcript_path(session_id).filter(|path| path.is_file())
                })
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
        decoded.set_final_message(
            (event_name == "agentStop")
                .then_some(observation.transcript_path.as_deref())
                .flatten()
                .and_then(|path| transcript::last_assistant_message(Path::new(path))),
        );
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        super::hook_types::catalog_event_names(COPILOT_HOOKS)
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        use super::{AgentHookClass, ClassificationSample};

        let mut samples = super::hook_types::catalog_classification_corpus(COPILOT_HOOKS);
        samples.push(ClassificationSample::new(
            "preToolUse",
            json!({"sessionId":"sess-1","toolName":"bash"}),
            AgentHookClass::Lifecycle,
            None,
        ));
        samples
    }

    fn correlate_subagent(
        &self,
        input: SubagentCorrelationInput<'_>,
    ) -> Option<SubagentCorrelation> {
        let (child_id, parent_id) = match resolve_subagent_identity(
            self.descriptor().kind,
            "transcript_correlation",
            Some(input.child_agent_id.as_str()),
            Some(input.parent_agent_id.as_str()),
            &Value::Null,
        ) {
            SubagentIdentity::Resolved {
                agent_id,
                parent_agent_id,
            } => (agent_id, parent_agent_id),
            SubagentIdentity::Quarantined => return None,
        };
        let parent_transcript = input
            .parent_transcript_path
            .map(Path::to_path_buf)
            .or_else(|| paths::session_transcript_path(parent_id.as_str()))?;
        let correlated =
            subagent::correlate(&parent_transcript, parent_id.as_str(), child_id.as_str())?;
        Some(SubagentCorrelation {
            agent_name: correlated.agent_name,
            role: None,
            task: correlated.task,
            prompt: sanitize_user_prompt(correlated.prompt.as_deref()),
            model: correlated.model,
        })
    }

    fn spawned_subagents(&self, input: SubagentSpawnInput<'_>) -> Vec<SpawnedSubagent> {
        let Some(parent_transcript) = input
            .parent_transcript_path
            .map(Path::to_path_buf)
            .or_else(|| paths::session_transcript_path(input.parent_agent_id.as_str()))
        else {
            return Vec::new();
        };
        subagent::completed(&parent_transcript, input.parent_agent_id.as_str())
            .into_iter()
            .map(|child| SpawnedSubagent {
                child_agent_id: AgentSessionId::from(child.child_id),
                agent_name: child.agent_name,
                role: child.task.clone(),
                prompt: sanitize_user_prompt(child.prompt.as_deref()),
                model: child.model,
                total_tokens: child.total_tokens,
            })
            .collect()
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn observe_context(&self, source: &str, payload: &Value) -> Option<super::ContextObservation> {
        let parsed = statusline::StatuslinePayload::parse(payload)?;
        let agent_id = parsed.session_id.clone()?;
        super::ContextObservation::new(agent_id, parsed.into_context(source, Timestamp::now())?)
    }

    fn context_cost(&self, payload: &Value, prices: &super::PriceBook) -> Option<super::AgentCost> {
        statusline::StatuslinePayload::parse(payload)?.cost(prices)
    }

    fn local_context_refresh(
        &self,
        trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let statusline_installed =
            paths::settings_path().is_ok_and(|path| install::statusline_installed(&path));
        local_context_refresh_with_statusline(statusline_installed, trigger, ctx)
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

    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        paths::copilot_home()
            .and_then(|home| {
                crate::agents::spending::SpendingSourceTree::new(
                    home.join("session-state"),
                    "*/events.jsonl",
                )
            })
            .map(|tree| crate::agents::spending::SpendingSource::group(vec![tree]))
            .into_iter()
            .collect()
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&crate::agents::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> crate::agents::spending::SpendParse {
        spend::parse(path, resume, prices)
    }

    fn launch_env(&self) -> Vec<(&'static str, &'static str)> {
        vec![(
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            "false",
        )]
    }

    fn room_env(&self, runtime: &crate::store::RuntimePaths) -> BTreeMap<String, String> {
        room_env_from(
            runtime,
            std::env::var_os("COPILOT_OTEL_FILE_EXPORTER_PATH").as_deref(),
            std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").as_deref(),
            std::env::var_os("COPILOT_OTEL_EXPORTER_TYPE").as_deref(),
            std::env::var_os("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT").as_deref(),
        )
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&COPILOT_MANAGED_SOURCE)
    }

    fn wiring_input_paths(&self) -> Vec<PathBuf> {
        [paths::hooks_path(), paths::settings_path()]
            .into_iter()
            .flatten()
            .collect()
    }

    fn install_hooks(&self) -> Result<HookInstallReport> {
        install::install(&paths::hooks_path()?, &paths::settings_path()?)
    }

    fn preview_hook_install(&self) -> Result<HookInstallPreview> {
        install::preview(&paths::hooks_path()?, &paths::settings_path()?)
    }

    fn uninstall_hooks(&self) -> Result<HookUninstallReport> {
        install::uninstall(&paths::hooks_path()?, &paths::settings_path()?)
    }

    fn hooks_installed(&self) -> bool {
        paths::hooks_path()
            .and_then(|hooks| Ok((hooks, paths::settings_path()?)))
            .is_ok_and(|(hooks, settings)| install::installed(&hooks, &settings))
    }

    fn managed_hook_artifacts_present(&self) -> bool {
        paths::hooks_path()
            .and_then(|hooks| Ok((hooks, paths::settings_path()?)))
            .is_ok_and(|(hooks, settings)| install::managed(&hooks, &settings))
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        install::wrapped_statusline_command(&paths::settings_path().ok()?)
    }

    fn probe_account(&self) -> crate::agents::account::AccountProbe {
        account::probe()
    }

    fn probe_account_usage(&self) -> crate::agents::AccountUsageProbe {
        account_usage::probe_usage()
    }
}

fn parse_copilot_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let token = line
            .trim()
            .strip_prefix("GitHub Copilot CLI ")?
            .strip_suffix('.')?;
        token
            .parse::<super::version::CliVersion>()
            .ok()
            .map(|version| version.to_string())
    })
}

fn local_context_refresh_with_statusline(
    statusline_installed: bool,
    trigger: RefreshTrigger<'_>,
    ctx: &LocalContextRefreshCtx<'_>,
) -> Option<LocalContextRefresh> {
    if statusline_installed {
        return None;
    }
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

fn room_env_from(
    runtime: &crate::store::RuntimePaths,
    explicit_file: Option<&OsStr>,
    otlp_endpoint: Option<&OsStr>,
    exporter_type: Option<&OsStr>,
    capture_content: Option<&OsStr>,
) -> BTreeMap<String, String> {
    let mut ambient = BTreeMap::new();
    for (key, value) in [
        ("COPILOT_OTEL_FILE_EXPORTER_PATH", explicit_file),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", otlp_endpoint),
        ("COPILOT_OTEL_EXPORTER_TYPE", exporter_type),
        (
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            capture_content,
        ),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            ambient.insert(key.to_owned(), value.to_string_lossy().into_owned());
        }
    }
    if ambient.contains_key("COPILOT_OTEL_FILE_EXPORTER_PATH") {
        return ambient;
    }

    if paths::otlp_only_config(otlp_endpoint, exporter_type) {
        tracing::warn!(
            "Copilot direct-launch enrichment is unavailable because an OTLP exporter is configured; set COPILOT_OTEL_FILE_EXPORTER_PATH to retain a file source"
        );
        return ambient;
    }

    BTreeMap::from([
        (
            "COPILOT_OTEL_FILE_EXPORTER_PATH".to_owned(),
            runtime.copilot_otel_path().to_string_lossy().into_owned(),
        ),
        (
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT".to_owned(),
            "false".to_owned(),
        ),
    ])
}

#[cfg(test)]
mod tests;
