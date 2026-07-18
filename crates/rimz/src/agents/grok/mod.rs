//! Grok Build hooks, durable sessions, account identity, and exact spend.

mod account;
mod install;
mod paths;
mod payloads;
mod spend;
mod transcript;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::Value;

use super::descriptor::{
    AgentDescriptor, Brand, Capabilities, ConcernCoverage, HookCoverage, IntegrationCoverage,
    LifecycleCoverage, PlanLabel, RealtimeUsageChannel, RemoteControlCapability,
    SamePaneSessionPolicy, ThreadKey, ToolClassification,
};
use super::hook_types::{HookRecord, hook_record};
use super::lifecycle::{AskKind, LifecycleSignal};
use super::{
    AgentAdapter, AgentCurrentUsage, AgentLifecycleObservation, AgentTokenUsage, AgentTurnError,
    ClassifiedHook, LocalContextRefresh, LocalContextRefreshCtx, ManagedSource, RefreshTrigger,
    Result, SessionOrigin, TranscriptMessage, TranscriptPage, TranscriptPosition, TurnErrorClass,
    non_empty_trimmed, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

static GROK_DESCRIPTOR: AgentDescriptor = AgentDescriptor {
    kind: "grok",
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
    activity_events: &[
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "SubagentStart",
        "SubagentStop",
        "PostCompact",
    ],
    thread_key: ThreadKey::PerFile,
    launch: super::LaunchSpec {
        program: Some("grok"),
        fixed_args: &[],
        prompt: super::PromptStyle::None,
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

const GROK_COVERAGE: IntegrationCoverage = IntegrationCoverage {
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

const GROK_LIFECYCLE_HOOKS: LifecycleCoverage = LifecycleCoverage {
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

pub(super) const GROK_HOOKS: &[HookRecord] = &[
    hook_record!(lifecycle, "SessionStart", r#"{"sessionId":"s1"}"#),
    hook_record!(
        lifecycle,
        "UserPromptSubmit",
        r#"{"sessionId":"s1","prompt":"hello"}"#
    ),
    hook_record!(
        lifecycle,
        "PostToolUse",
        r#"{"sessionId":"s1","toolName":"apply_patch"}"#
    ),
    hook_record!(
        lifecycle,
        "PostToolUseFailure",
        r#"{"sessionId":"s1","toolName":"apply_patch","error":"failed"}"#
    ),
    hook_record!(
        blocking,
        "Notification",
        r#"{"sessionId":"s1","notificationType":"permission_prompt","message":"Tool permission requested"}"#,
        AskKind::Permission
    )
    .with_lifecycle_fallback(),
    hook_record!(
        lifecycle,
        "StopFailure",
        r#"{"sessionId":"s1","error":"failed"}"#
    ),
    hook_record!(
        lifecycle,
        "Stop",
        r#"{"sessionId":"s1","reason":"end_turn"}"#
    ),
    hook_record!(
        lifecycle,
        "SubagentStart",
        r#"{"sessionId":"s1","subagentId":"child"}"#
    ),
    hook_record!(
        lifecycle,
        "SubagentStop",
        r#"{"sessionId":"s1","subagentId":"child","exitCode":0}"#
    ),
    hook_record!(
        lifecycle,
        "PreCompact",
        r#"{"sessionId":"s1","source":"auto"}"#
    ),
    hook_record!(
        lifecycle,
        "PostCompact",
        r#"{"sessionId":"s1","source":"auto"}"#
    ),
    hook_record!(lifecycle, "SessionEnd", r#"{"sessionId":"s1"}"#),
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

impl AgentAdapter for GrokAdapter {
    fn descriptor(&self) -> &'static AgentDescriptor {
        &GROK_DESCRIPTOR
    }

    fn classify_hook(&self, event_name: &str, payload: &Value) -> ClassifiedHook {
        let canonical = canonical_event_name(event_name);
        let parsed = payloads::parse(payload);
        let ask_kind = (canonical == "Notification")
            .then(|| notification_ask(&parsed))
            .flatten();
        let installed = GROK_HOOKS.iter().any(|hook| hook.event == canonical);
        let class = if ask_kind.is_some() {
            super::AgentHookClass::AwaitingUser
        } else if installed {
            super::AgentHookClass::Lifecycle
        } else {
            super::AgentHookClass::Unknown
        };
        ClassifiedHook {
            class,
            ask_kind,
            event_name: canonical,
        }
    }

    #[cfg(test)]
    fn native_hook_events(&self) -> Vec<&'static str> {
        GROK_HOOKS.iter().map(|hook| hook.event).collect()
    }

    #[cfg(test)]
    fn classification_corpus(&self) -> Vec<super::ClassificationSample> {
        tests::classification_corpus()
    }

    #[cfg(test)]
    fn spend_fixture(&self) -> Option<super::SpendFixture> {
        Some(super::SpendFixture {
            session_id: "s1",
            file_name: "updates.jsonl",
            body: super::SpendFixtureBody::Jsonl(
                r#"{"timestamp":1700000000,"method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"},"_meta":{"promptIndex":0}}}}
{"timestamp":1700000001,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"turn_completed","prompt_id":"p1","stop_reason":"end_turn","usage":{"inputTokens":100,"cachedReadTokens":20,"outputTokens":10,"costUsdTicks":1000000000}}}}"#,
            ),
        })
    }

    fn render_neutral(&self, _event_name: &str) -> Result<Option<Value>> {
        Ok(None)
    }

    fn observe_lifecycle(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentLifecycleObservation> {
        let parsed = payloads::parse(payload);
        let signal = lifecycle_signal(self.descriptor(), event_name, &parsed)?;
        let is_subagent = matches!(event_name, "SubagentStart" | "SubagentStop");
        let root_id = parsed.session_id.as_deref().and_then(non_empty_trimmed);
        let child_id = parsed.subagent_id.as_deref().and_then(non_empty_trimmed);
        if is_subagent && (root_id.is_none() || child_id.is_none()) {
            return None;
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
        observation.worktree_path = parsed.workspace_root.clone().or_else(|| parsed.cwd.clone());
        observation.prompt = (event_name == "UserPromptSubmit")
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
                .or_else(|| paths::transcript_for_session(session_id))
                .map(|path| path.to_string_lossy().into_owned())
        });
        if event_name == "SessionStart" {
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
        Some(observation)
    }

    fn observe_turn_error_from_hook(
        &self,
        event_name: &str,
        payload: &Value,
    ) -> Option<AgentTurnError> {
        let parsed = payloads::parse(payload);
        let raw_label = match event_name {
            "StopFailure" | "PostToolUseFailure" => parsed.error.as_deref(),
            "Notification" if parsed.notification_type.as_deref() == Some("agent_error") => {
                parsed.error.as_deref().or(parsed.message.as_deref())
            }
            _ => return None,
        };
        let label = raw_label
            .and_then(non_empty_trimmed)
            .map(|label| label.chars().take(160).collect::<String>());
        Some(AgentTurnError {
            class: TurnErrorClass::classify_label(label.as_deref()),
            at: Timestamp::now(),
            label,
        })
    }

    fn last_assistant_message(
        &self,
        event_name: &str,
        _payload: &Value,
        observation: &AgentLifecycleObservation,
    ) -> Option<String> {
        (event_name == "Stop").then_some(()).and_then(|()| {
            transcript::last_assistant_message(Path::new(observation.transcript_path.as_deref()?))
        })
    }

    fn parse_transcript_messages(&self, lines: &str) -> Vec<TranscriptMessage> {
        transcript::parse_messages(lines)
    }

    fn stream_assistant_messages(&self, new_lines: &str) -> Vec<String> {
        transcript::parse_assistant_suffix(new_lines)
    }

    fn transcript_position(
        &self,
        path: &Path,
        _session_id: Option<&AgentSessionId>,
    ) -> Option<TranscriptPosition> {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| TranscriptPosition::new(metadata.len()))
    }

    fn read_assistant_transcript_page(
        &self,
        path: &Path,
        _session_id: Option<&AgentSessionId>,
        position: TranscriptPosition,
    ) -> Option<TranscriptPage> {
        let (bytes, next) = super::read_transcript_lines(path, position.get())?;
        Some(TranscriptPage {
            next: TranscriptPosition::new(next),
            messages: transcript::parse_assistant_suffix(&String::from_utf8_lossy(&bytes)),
        })
    }

    fn local_context_refresh(
        &self,
        _trigger: RefreshTrigger<'_>,
        ctx: &LocalContextRefreshCtx<'_>,
    ) -> Option<LocalContextRefresh> {
        let path = paths::resolve_transcript(
            ctx.agent_id,
            ctx.current_transcript_path.map(Path::new),
            ctx.prior_transcript_path.map(Path::new),
        )?;
        let events = paths::events_companion(&path, ctx.agent_id);
        refresh_resolved_context(&path, events.as_deref(), ctx)
    }

    fn resumed_session_id_from_cmdline(&self, cmdline: &str) -> Option<AgentSessionId> {
        resumed_session_id(cmdline)
    }

    fn probe_account(&self) -> super::account::AccountProbe {
        account::probe()
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        spend::files()
    }

    fn spending_sources(&self) -> Vec<crate::agents::spending::SpendingSource> {
        crate::agents::spending::SpendingSourceTree::new(paths::sessions_root(), "**/updates.jsonl")
            .map(|tree| crate::agents::spending::SpendingSource::group(vec![tree]))
            .into_iter()
            .collect()
    }

    fn session_transcript(&self, session_id: &str, prior_path: Option<&Path>) -> Option<PathBuf> {
        paths::resolve_transcript(session_id, None, prior_path)
    }

    fn parse_spend(
        &self,
        path: &Path,
        resume: Option<&super::spending::SpendCursor>,
        prices: &super::PriceBook,
    ) -> super::spending::SpendParse {
        spend::parse(path, resume, prices)
    }

    fn launch_command(&self, extra_args: &[String], prompt: Option<&str>) -> Option<Vec<String>> {
        let mut argv = vec!["grok".to_owned()];
        argv.extend(extra_args.iter().cloned());
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
            argv.extend([
                "-p".to_owned(),
                prompt.to_owned(),
                "--output-format".to_owned(),
                "streaming-json".to_owned(),
            ]);
        }
        Some(argv)
    }

    fn managed_source(&self) -> Option<&'static ManagedSource> {
        Some(&install::MANAGED_SOURCE)
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
        session_preview: summary.as_ref().and_then(transcript::Summary::title),
        model_display_name: model_id.as_deref().map(super::model_display::display_model),
        model_id,
        effort: summary.and_then(|value| value.reasoning_effort),
        tokens,
        cost,
        native_permission_wait: events.and_then(transcript::native_permission_wait),
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        ..LocalContextRefresh::default()
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
    descriptor: &AgentDescriptor,
    event_name: &str,
    payload: &payloads::HookPayload,
) -> Option<LifecycleSignal> {
    Some(match event_name {
        "SessionStart" => LifecycleSignal::Registered,
        "UserPromptSubmit" => LifecycleSignal::TurnStarted,
        "PostToolUse" => {
            let tool = payload.tool_name.as_deref();
            LifecycleSignal::ToolUsed {
                mutates: tool.is_some_and(|tool| descriptor.tools.mutating.contains(&tool)),
                edits: tool.is_some_and(|tool| descriptor.tools.editing.contains(&tool)),
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
