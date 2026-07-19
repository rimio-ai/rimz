//! Grok Build hooks, durable sessions, account identity, and exact spend.

mod account;
mod install;
mod paths;
mod payloads;
mod spend;
mod transcript;

pub(crate) use crate::agents::capabilities::*;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::definition::{
    AgentSpec, Brand, Capabilities, ConcernCoverage, CoverageAnnotations, HookCoverage,
    LifecycleAnnotations, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    SamePaneSessionPolicy, ThreadKey, ToolClassification,
};
use super::hook_types::{HookEventSpec, decode_catalog_hook};
use super::lifecycle::{AskKind, LifecycleSignal};
use super::{
    AgentCurrentUsage, AgentLifecycleObservation, AgentTokenUsage, AgentTurnError, FieldPatch,
    HookOutput, HookRouting, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx,
    LocalTokenPatch, RefreshTrigger, Result, SessionOrigin, TranscriptMessage, TurnErrorClass,
    non_empty_trimmed, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static GROK_DESCRIPTOR: AgentSpec = AgentSpec {
    kind: "grok",
    aliases: &[],
    display_name: "Grok",
    brand: Brand {
        emblem: None,
        color: 15,
        color_rgb: (0xff, 0xff, 0xff),
    },
    plan_label: PlanLabel::Prefixed { prefix: "Grok" },
    sub_providers: &[],
    expected_windows: &[],
    tools: ToolClassification {
        mutating: &[
            "search_replace",
            "hashline_edit",
            "apply_patch",
            "write",
            "run_terminal_command",
            "run_terminal_cmd",
        ],
        editing: &["search_replace", "hashline_edit", "apply_patch", "write"],
        blocking: &[],
    },
    capabilities: Capabilities {
        native_ask_ui: true,
        transcript_tail_context: true,
        registers_lazily: false,
        local_session_discovery: false,
        daemon_hooked_sessions: false,
        direct_account_usage: false,
        same_pane_session: SamePaneSessionPolicy::KeepPrimary,
        realtime_usage: RealtimeUsageChannel {
            windows_defer_to_fresh_realtime: false,
        },
        remote_control: RemoteControlCapability {
            pane_sessions: false,
            background_sessions: false,
        },
    },
    coverage: GROK_COVERAGE,
    lifecycle_hooks: GROK_LIFECYCLE_HOOKS,
    default_context_window: None,
    default_model: None,
    process_names: &["grok", "xai-grok-pager"],
    bin_names: &["grok"],
    extra_bin_dirs: &[],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("grok"),
        fixed_args: &[],
        prompt: super::PromptStyle::FlagWithSuffix {
            flag: "-p",
            suffix: &["--output-format", "streaming-json"],
        },
        resume: Some(super::SessionCommand {
            before_id: &["grok", "--resume"],
            after_id: &[],
        }),
        fork: Some(super::SessionCommand {
            before_id: &["grok", "--resume"],
            after_id: &["--fork-session"],
        }),
        permission: super::LaunchPermissionArgs {
            ask: &["--permission-mode", "default"],
            auto: &["--permission-mode", "auto"],
            yolo: &["--yolo"],
            plan: &[],
        },
        ping_args: None,
        max_turn_flag: Some("--max-turns"),
        compact_command: Some("/compact"),
        presets: super::PresetMatchers {
            model: Some(super::StaticPresetMatcher::Flag(&["--model"])),
            effort: Some(super::StaticPresetMatcher::Flag(&["--reasoning-effort"])),
            ..super::PresetMatchers::EMPTY
        },
    },
};

const GROK_COVERAGE: CoverageAnnotations = CoverageAnnotations {
    turn_lifecycle: ConcernCoverage::Wired {
        via: "SessionStart/UserPromptSubmit/Stop",
    },
    permission: ConcernCoverage::Wired {
        via: "Notification:permission_prompt",
    },
    plan_approval: ConcernCoverage::Wired {
        via: "Notification:Plan approval requested",
    },
    user_question: ConcernCoverage::Wired {
        via: "Notification:elicitation_dialog",
    },
    answer: ConcernCoverage::Unsupported {
        reason: "answer in the native Grok pane",
    },
    compaction: ConcernCoverage::Wired {
        via: "PreCompact/PostCompact",
    },
    subagents: ConcernCoverage::Wired {
        via: "SubagentStart/SubagentStop",
    },
    background_parking: ConcernCoverage::Unsupported {
        reason: "stock Grok hooks expose no background parking lifecycle",
    },
    session_end: ConcernCoverage::Wired { via: "SessionEnd" },
    idle_notification: ConcernCoverage::Partial {
        via: "Notification",
        gap: "only native asks and agent errors have stable discriminators",
    },
    context_usage: ConcernCoverage::Wired {
        via: "updates.jsonl + signals.json",
    },
    realtime_cost: ConcernCoverage::Partial {
        via: "exact completed-turn turn_completed usage",
        gap: "no trustworthy mid-turn cost",
    },
    rich_context: ConcernCoverage::Partial {
        via: "summary.json + signals.json + updates.jsonl",
        gap: "no provider realtime push outside local session files",
    },
    hook_install: ConcernCoverage::Wired {
        via: "~/.grok/hooks/rimz.json",
    },
    account_spend: ConcernCoverage::Wired {
        via: "auth.json identity + exact completed-turn dollars",
    },
    remote_control: ConcernCoverage::Unsupported {
        reason: "ACP and remote control are outside the stock TUI adapter",
    },
};

const GROK_LIFECYCLE_HOOKS: LifecycleAnnotations = LifecycleAnnotations {
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
        event: "Notification",
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

pub(super) const RIMZ_HOOK_COMMAND: &str = "rimz hooks feed --source grok";
pub(super) const RIMZ_HOOK_MARKER: &str = "rimz hooks feed --source grok";

pub(super) const GROK_HOOKS: &[HookEventSpec] = &[
    HookEventSpec::lifecycle( "SessionStart", r#"{"sessionId":"s1"}"#).progress(),
    HookEventSpec::lifecycle(
        "UserPromptSubmit",
        r#"{"sessionId":"s1","prompt":"hello"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PostToolUse",
        r#"{"sessionId":"s1","toolName":"apply_patch"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PostToolUseFailure",
        r#"{"sessionId":"s1","toolName":"apply_patch","error":"failed"}"#
    )
    .progress(),
    HookEventSpec::blocking(
        "Notification",
        r#"{"sessionId":"s1","notificationType":"permission_prompt","message":"Tool permission requested"}"#,
        AskKind::Permission
    )
    .with_lifecycle_fallback(),
    HookEventSpec::lifecycle(
        "StopFailure",
        r#"{"sessionId":"s1","error":"failed"}"#
    ),
    HookEventSpec::lifecycle(
        "Stop",
        r#"{"sessionId":"s1","reason":"end_turn"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStart",
        r#"{"sessionId":"s1","subagentId":"child"}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "SubagentStop",
        r#"{"sessionId":"s1","subagentId":"child","exitCode":0}"#
    )
    .progress(),
    HookEventSpec::lifecycle(
        "PreCompact",
        r#"{"sessionId":"s1","source":"auto"}"#
    ),
    HookEventSpec::lifecycle(
        "PostCompact",
        r#"{"sessionId":"s1","source":"auto"}"#
    )
    .progress(),
    HookEventSpec::lifecycle( "SessionEnd", r#"{"sessionId":"s1"}"#).session_ended(),
];

const KNOWN_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "StopFailure",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "SessionEnd",
    "PermissionDenied",
];

#[derive(Clone, Debug, Default)]
pub struct GrokAdapter;

impl crate::agents::capabilities::CoreCapability for GrokAdapter {
    fn spec(&self) -> &'static AgentSpec {
        &GROK_DESCRIPTOR
    }
}

impl crate::agents::capabilities::LaunchCapability for GrokAdapter {}

impl crate::agents::capabilities::HookCapability for GrokAdapter {
    fn decode_hook(&self, event_name: &str, payload: &Value) -> Result<HookOutput> {
        let canonical = canonical_event_name(event_name);
        let parsed = payloads::parse(payload);
        let ask_kind = (canonical == "Notification")
            .then(|| notification_ask(&parsed))
            .flatten();
        let mut decoded = decode_catalog_hook(GROK_HOOKS, &canonical, ask_kind);
        let worktree_path = parsed.workspace_root.clone().or_else(|| parsed.cwd.clone());
        decoded.set_routing(
            HookRouting::session(parsed.session_id.clone().map(Into::into))
                .with_worktree(worktree_path.clone()),
        );
        decoded.set_turn_error(
            match canonical.as_str() {
                "StopFailure" | "PostToolUseFailure" => parsed.error.as_deref(),
                "Notification" if parsed.notification_type.as_deref() == Some("agent_error") => {
                    parsed.error.as_deref().or(parsed.message.as_deref())
                }
                _ => None,
            }
            .map(|raw_label| {
                let label = non_empty_trimmed(raw_label)
                    .map(|label| label.chars().take(160).collect::<String>());
                AgentTurnError {
                    class: TurnErrorClass::classify_label(label.as_deref()),
                    at: Timestamp::now(),
                    label,
                }
            }),
        );
        let Some(signal) = lifecycle_signal(self.spec(), &canonical, &parsed) else {
            return Ok(decoded);
        };
        let is_subagent = matches!(canonical.as_str(), "SubagentStart" | "SubagentStop");
        let root_id = parsed.session_id.as_deref().and_then(non_empty_trimmed);
        let child_id = parsed.subagent_id.as_deref().and_then(non_empty_trimmed);
        if is_subagent && (root_id.is_none() || child_id.is_none()) {
            return Ok(decoded);
        }
        let agent_id = if is_subagent {
            child_id.as_deref()
        } else {
            root_id.as_deref()
        }
        .map(AgentSessionId::from);
        let mut observation = AgentLifecycleObservation::new(agent_id, signal);
        if is_subagent {
            observation.parent_agent_id = root_id.as_deref().map(AgentSessionId::from);
        }
        observation.worktree_path = worktree_path;
        observation.prompt = (canonical == "UserPromptSubmit")
            .then(|| sanitize_user_prompt(parsed.prompt.as_deref()))
            .flatten();
        observation.task = is_subagent
            .then(|| {
                parsed
                    .description
                    .clone()
                    .or_else(|| parsed.subagent_type.clone())
                    .or_else(|| parsed.agent_type.clone())
            })
            .flatten();
        observation.transcript_path = root_id.as_deref().and_then(|session_id| {
            parsed
                .transcript_path
                .as_deref()
                .and_then(|path| paths::validate_transcript(Path::new(path), session_id))
                .or_else(|| paths::transcript_for_session(session_id, self.transcript_files()))
                .map(|path| path.to_string_lossy().into_owned())
        });
        if canonical == "SessionStart" {
            observation.origin = match parsed.source.as_deref() {
                Some("new" | "startup" | "clear") => Some(SessionOrigin::Fresh),
                Some("fork") => Some(SessionOrigin::Forked),
                _ => None,
            };
            let summary = observation
                .transcript_path
                .as_deref()
                .and_then(|path| transcript::read_summary(Path::new(path)))
                .filter(|summary| {
                    summary
                        .info
                        .id
                        .as_deref()
                        .is_none_or(|summary_id| Some(summary_id) == root_id.as_deref())
                });
            observation.worktree_path = observation
                .worktree_path
                .or_else(|| summary.as_ref().and_then(|value| value.info.cwd.clone()));
            observation.launch.model = summary
                .as_ref()
                .and_then(|value| value.current_model_id.clone())
                .or_else(|| parsed.model_id.clone());
            observation.launch.effort = summary
                .as_ref()
                .and_then(|value| value.reasoning_effort.clone());
            observation.description = summary.as_ref().and_then(transcript::Summary::title);
        }
        decoded.set_final_message(
            (canonical == "Stop")
                .then_some(observation.transcript_path.as_deref())
                .flatten()
                .and_then(|path| transcript::last_assistant_message(Path::new(path))),
        );
        decoded.attach_lifecycle(observation);
        Ok(decoded)
    }

    #[cfg(test)]
    fn conformance(&self) -> super::AdapterConformance {
        super::AdapterConformance {
            classification: tests::classification_corpus(),
            spend: Some(super::SpendFixture {
                session_id: "s1",
                file_name: "updates.jsonl",
                body: super::SpendFixtureBody::Jsonl(
                    r#"{"timestamp":1700000000,"method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"},"_meta":{"promptIndex":0}}}}
{"timestamp":1700000001,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn","usage":{"inputTokens":100,"cachedReadTokens":20,"outputTokens":10,"costUsdTicks":1000000000}}}}"#,
                ),
            }),
            ..super::AdapterConformance::default()
        }
    }
}

impl crate::agents::capabilities::InstallationCapability for GrokAdapter {
    fn managed_integration(&self) -> Option<&'static dyn super::ManagedIntegration> {
        Some(&install::MANAGED_SOURCE)
    }
}

impl crate::agents::capabilities::SessionCapability for GrokAdapter {
    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<AgentSessionId> {
        resumed_session_id(cmdline)
    }
}

impl crate::agents::capabilities::TranscriptCapability for GrokAdapter {
    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        transcript::parse_assistant_suffix(new_lines)
    }
}

impl crate::agents::capabilities::ContextCapability for GrokAdapter {
    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let path = paths::resolve_transcript(
            ctx.agent_id,
            ctx.current_transcript_path.map(Path::new),
            ctx.prior_transcript_path.map(Path::new),
            self.transcript_files(),
        )?;
        let events = paths::events_companion(&path, ctx.agent_id);
        refresh_resolved_context(&path, events.as_deref(), ctx)
    }
}

impl crate::agents::capabilities::AccountCapability for GrokAdapter {
    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }
}

impl crate::agents::capabilities::SpendingCapability for GrokAdapter {
    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        crate::agents::spending::SpendingSourceTree::new(paths::sessions_root(), "**/updates.jsonl")
            .map(|tree| crate::agents::spending::SpendingSource::group(vec![tree]))
            .into_iter()
            .collect()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        paths::resolve_transcript(session_id, None, prior_path, self.transcript_files())
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

pub(crate) fn refresh_resolved_context(
    path: &Path,
    events: Option<&Path>,
    ctx: &LocalContextRefreshCtx<'_>,
) -> Option<LocalContextRefresh> {
    let stat = transcript::combined_stat(path, events)?;
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let folded = transcript::read(path).ok()?;
    let summary = transcript::read_summary(path).filter(|summary| {
        summary
            .info
            .id
            .as_deref()
            .is_none_or(|summary_id| summary_id == ctx.agent_id)
    });
    let signals = transcript::read_signals(path);
    let sample = folded.latest_token_sample();
    let context_window_size = sample
        .and_then(|value| value.context_window_tokens)
        .or_else(|| {
            signals
                .map(|value| value.context_window_tokens)
                .filter(|value| *value > 0)
        });
    let current_context_tokens = sample.map(|value| value.total_tokens).or_else(|| {
        (!folded.saw_rewind)
            .then(|| signals.map(|value| value.context_tokens_used))
            .flatten()
            .or_else(|| (!folded.saw_rewind).then_some(0))
    });
    let latest_usage = folded
        .completions()
        .rev()
        .find_map(|completion| completion.usage.as_ref());
    let tokens = Some(AgentTokenUsage {
        context_window_size,
        used_percentage: current_context_tokens
            .zip(context_window_size)
            .map(|(used, window)| {
                ((used as f64 / window as f64) * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8
            }),
        remaining_percentage: None,
        current_context_tokens,
        current_usage: latest_usage.map(|usage| AgentCurrentUsage {
            input_tokens: Some(usage.input_tokens.saturating_sub(usage.cached_read_tokens)),
            output_tokens: Some(usage.output_tokens),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(usage.cached_read_tokens),
        }),
        session_usage: None,
    });
    let model_id = summary
        .as_ref()
        .and_then(|value| value.current_model_id.clone())
        .or_else(|| ctx.model_hint.map(ToOwned::to_owned));
    let cost = spend::cost_from_folded(path, &folded, ctx.agent_id);
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            session_preview: summary
                .as_ref()
                .and_then(transcript::Summary::title)
                .map_or(FieldPatch::Keep, FieldPatch::Set),
            model_display_name: model_id
                .as_deref()
                .map(super::model_display::display_model)
                .map_or(FieldPatch::Keep, FieldPatch::Set),
            model_id: model_id.map_or(FieldPatch::Keep, FieldPatch::Set),
            effort: summary
                .and_then(|value| value.reasoning_effort)
                .map_or(FieldPatch::Keep, FieldPatch::Set),
            tokens: LocalTokenPatch::PreserveEstablished(tokens),
            cost: cost.map_or(FieldPatch::Keep, FieldPatch::Set),
            native_permission_wait: events
                .and_then(transcript::native_permission_wait)
                .map_or(FieldPatch::Clear, FieldPatch::Set),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        ..LocalContextRefresh::authoritative_current()
    })
}

fn notification_ask(payload: &payloads::HookPayload) -> Option<AskKind> {
    match (
        payload.notification_type.as_deref(),
        payloads::notification_label(payload),
    ) {
        (
            Some("permission_prompt"),
            Some("Tool permission requested" | "Diff review requested"),
        ) => Some(AskKind::Permission),
        (Some("permission_prompt"), Some("Plan approval requested")) => Some(AskKind::PlanApproval),
        (Some("elicitation_dialog"), Some("User question requested")) => Some(AskKind::Question),
        _ => None,
    }
}

fn lifecycle_signal(
    spec: &AgentSpec,
    event_name: &str,
    payload: &payloads::HookPayload,
) -> Option<LifecycleSignal> {
    Some(match event_name {
        "SessionStart" => LifecycleSignal::Registered,
        "UserPromptSubmit" => LifecycleSignal::TurnStarted,
        "PostToolUse" => {
            let tool = payload.tool_name.as_deref();
            LifecycleSignal::ToolUsed {
                mutates: tool.is_some_and(|tool| spec.tools.mutating.contains(&tool)),
                edits: tool.is_some_and(|tool| spec.tools.editing.contains(&tool)),
                native_key: payload.tool_use_id.clone(),
            }
        }
        "PostToolUseFailure" => LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: payload.tool_use_id.clone(),
        },
        "Notification" => LifecycleSignal::AwaitingInput {
            kind: notification_ask(payload)?,
            ask_id: None,
            detail: payloads::notification_label(payload).map(ToOwned::to_owned),
            native_key: payload.prompt_id.clone(),
        },
        "Stop" => match payload.reason.as_deref() {
            Some("end_turn") => LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            Some("cancelled") => LifecycleSignal::TurnInterrupted,
            Some("error") => LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
            Some("channel_closed" | "shutdown") | None => return None,
            Some(_) => return None,
        },
        "SubagentStart" => LifecycleSignal::SubagentStarted,
        "SubagentStop" => LifecycleSignal::SubagentStopped {
            errored: payload.exit_code.is_some_and(|code| code > 0),
        },
        "PreCompact" => LifecycleSignal::Compacting,
        "PostCompact" => LifecycleSignal::CompactionEnded {
            auto: match payload.source.as_deref() {
                Some("auto" | "automatic") => Some(true),
                Some("manual") => Some(false),
                _ => None,
            },
        },
        "SessionEnd" => LifecycleSignal::Ended,
        "StopFailure" | "PermissionDenied" | "PreToolUse" => return None,
        _ => return None,
    })
}

fn canonical_event_name(raw: &str) -> String {
    let key = raw
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if let Some(event) = KNOWN_EVENTS.iter().find(|event| {
        event
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .eq(key.chars())
    }) {
        return (*event).to_owned();
    }
    let mut output = String::new();
    let mut uppercase = true;
    for character in raw.trim().chars() {
        if !character.is_ascii_alphanumeric() {
            uppercase = true;
        } else if uppercase {
            output.extend(character.to_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

fn resumed_session_id(cmdline: &str) -> Option<AgentSessionId> {
    let mut tokens = cmdline.split_whitespace();
    let program = Path::new(tokens.next()?).file_name()?.to_str()?;
    if program != "grok" {
        return None;
    }
    while let Some(token) = tokens.next() {
        let id = if token == "--resume" {
            tokens.next()?
        } else if let Some(id) = token.strip_prefix("--resume=") {
            id
        } else {
            continue;
        };
        return non_empty_trimmed(id).map(AgentSessionId::from);
    }
    None
}

#[cfg(test)]
mod tests;
